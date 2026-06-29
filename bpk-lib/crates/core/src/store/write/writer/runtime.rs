use super::fence_runtime::CommandResult;
use super::{
    ignore_closed_response_channel, Active, Receiver, RestartPolicy, Segment, StoreConfig,
    StoreError, ValidatedStoreConfig, WriterCommand, WriterCore, WriterLoopPhase,
};
use crate::store::file_classification::StoreFileKind;
use crate::store::index::StoreIndex;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct WriterRuntime<'a> {
    pub(super) rx: &'a Receiver<WriterCommand>,
    pub(super) config: Arc<StoreConfig>,
    pub(super) validated_cfg: Arc<ValidatedStoreConfig>,
    pub(super) index: Arc<StoreIndex>,
    pub(super) subscribers: Arc<super::SubscriberList>,
    pub(super) reactor_subscribers: Arc<super::ReactorSubscriberList>,
    pub(super) reader: Arc<crate::store::segment::scan::Reader>,
    pub(super) watermark_handle: super::WatermarkAdvanceHandle,
}

pub(super) fn writer_thread_name(data_dir: &Path) -> String {
    const FNV_1A_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_1A_PRIME: u64 = 0x100000001b3;

    let hash = data_dir
        .to_string_lossy()
        .bytes()
        .fold(FNV_1A_BASIS, |hash, byte| {
            let hash = hash ^ byte as u64;
            hash.wrapping_mul(FNV_1A_PRIME)
        });

    format!("batpak-writer-{hash:08x}")
}

#[derive(Debug)]
struct LoopOutcome {
    break_loop: bool,
    exit_writer: bool,
    sync_event_delta: u32,
    enter_group_commit_drain: bool,
}

/// Writer thread entry point with panic recovery and restart logic.
/// Wraps writer_loop() in catch_unwind, implementing RestartPolicy.
/// The rx (command receiver) survives across restarts because it lives
/// outside catch_unwind. Segments are re-created on restart since the
/// previous one is dropped during unwind.
pub(super) fn writer_thread_main(
    runtime: &WriterRuntime<'_>,
    initial_segment: Segment<Active>,
    initial_segment_id: u64,
) {
    let mut segment = initial_segment;
    let mut seg_id = initial_segment_id;
    let mut restarts: u32 = 0;
    let mut window_start = runtime.validated_cfg.now_mono_ns();

    loop {
        let loop_runtime = runtime.clone();
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            writer_loop(loop_runtime, segment, seg_id);
        }));

        match result {
            Ok(()) => return,
            Err(panic_info) => {
                // Do NOT poison the durability gate here: a panic within the
                // restart budget is recoverable. Poisoning is deferred to the
                // terminal exits below, so a transient panic + clean restart does
                // not leave wait_for_durable/applied/visible failing forever
                // (audit R3).
                let panic_msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                    (*s).to_string()
                } else if let Some(s) = panic_info.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };

                let budget_ok = restart_budget_allows(
                    &runtime.config.writer.restart_policy,
                    &mut restarts,
                    &mut window_start,
                    runtime.validated_cfg.now_mono_ns(),
                );

                if !budget_ok {
                    // Terminal exit: budget exhausted, the writer is giving up —
                    // now poison the durability gate so waiters fail fast.
                    runtime.watermark_handle.mark_writer_crashed();
                    tracing::error!(
                        "writer restart budget exhausted — thread exiting. \
                         Last panic: {panic_msg}. Policy: {:?}",
                        runtime.config.writer.restart_policy
                    );
                    if let Some(token) = runtime.index.active_visibility_fence() {
                        if runtime.index.cancel_visibility_fence(token).is_ok() {
                            let ranges = runtime.index.cancelled_visibility_ranges();
                            if let Err(error) = crate::store::hidden_ranges::write_cancelled_ranges(
                                &runtime.config.data_dir,
                                &ranges,
                            ) {
                                tracing::error!(
                                    error = %error,
                                    "failed to persist cancelled visibility ranges on terminal writer exit"
                                );
                            }
                        }
                    }
                    return;
                }

                tracing::warn!(
                    "writer panic — restarting ({restarts}/{max}). Panic: {panic_msg}",
                    max = match &runtime.config.writer.restart_policy {
                        RestartPolicy::Once => 1_u32,
                        RestartPolicy::Bounded { max_restarts, .. } => *max_restarts,
                    }
                );

                if let Some(token) = runtime.index.active_visibility_fence() {
                    if runtime.index.cancel_visibility_fence(token).is_ok() {
                        let ranges = runtime.index.cancelled_visibility_ranges();
                        if let Err(error) = crate::store::hidden_ranges::write_cancelled_ranges(
                            &runtime.config.data_dir,
                            &ranges,
                        ) {
                            tracing::error!(
                                error = %error,
                                "failed to persist cancelled visibility ranges during writer restart"
                            );
                        }
                    }
                }

                seg_id = match find_latest_segment_id(&runtime.config.data_dir) {
                    Ok(latest) => next_restart_segment_id(latest, seg_id),
                    Err(error) => {
                        // Terminal exit: cannot resume the writer — poison the gate.
                        runtime.watermark_handle.mark_writer_crashed();
                        tracing::error!(
                            "writer restart failed — cannot enumerate segments: {error}. Thread exiting."
                        );
                        return;
                    }
                };
                segment = match recreate_restart_segment(runtime, seg_id) {
                    Some(s) => s,
                    None => return,
                };
            }
        }
    }
}

/// Recreate the active segment after a writer restart, routing the create+fsync
/// through the configured [`StoreFs`] backend. On failure it poisons the
/// durability gate and logs the terminal exit, returning `None` so the caller
/// (the restart loop in [`writer_thread_main`]) returns and the thread exits.
/// Extracted to keep `writer_thread_main` within its complexity-ratchet budget
/// once the create call carries the fs-seam argument.
///
/// [`StoreFs`]: crate::store::platform::fs::StoreFs
fn recreate_restart_segment(runtime: &WriterRuntime<'_>, seg_id: u64) -> Option<Segment<Active>> {
    match Segment::<Active>::create_with_created_ns_on(
        &runtime.config.data_dir,
        seg_id,
        runtime.validated_cfg.now_wall_ns(),
        runtime.config.fs(),
    ) {
        Ok(segment) => Some(segment),
        Err(error) => {
            // Terminal exit: cannot resume the writer — poison the gate.
            runtime.watermark_handle.mark_writer_crashed();
            tracing::error!(
                "writer restart failed — cannot create segment: {error}. Thread exiting."
            );
            None
        }
    }
}

fn restart_budget_allows(
    policy: &RestartPolicy,
    restarts: &mut u32,
    window_start_ns: &mut i64,
    now_ns: i64,
) -> bool {
    match policy {
        RestartPolicy::Once => {
            if *restarts >= 1 {
                false
            } else {
                *restarts += 1;
                true
            }
        }
        RestartPolicy::Bounded {
            max_restarts,
            within_ms,
        } => {
            let elapsed_ms = now_ns.saturating_sub(*window_start_ns).max(0) / 1_000_000;
            if elapsed_ms > i64::try_from(*within_ms).unwrap_or(i64::MAX) {
                *restarts = 0;
                *window_start_ns = now_ns;
            }
            if *restarts >= *max_restarts {
                false
            } else {
                *restarts += 1;
                true
            }
        }
    }
}

fn next_restart_segment_id(latest: Option<u64>, fallback: u64) -> u64 {
    latest.unwrap_or(fallback).saturating_add(1)
}

fn group_commit_drain_budget_remaining(drained: u32, extra_budget: u32) -> bool {
    drained < extra_budget
}

/// Whether the writer loop should keep pulling commands or exit the thread.
///
/// Returned by [`WriterCore::drive_command`] in place of the bare `return`s the
/// per-command body used to perform directly in `writer_loop`.
///
/// `pub(super)` so the cooperative pump in the parent `writer` module can match
/// on the step exactly as `writer_loop` does on the threaded path.
pub(super) enum DriveStep {
    Continue,
    Exit,
}

impl WriterCore {
    /// Drive a single command pulled from `rx` through the full per-command
    /// pipeline: execute, settle, optional shutdown drain, optional group-commit
    /// drain, and periodic sync. Returns [`DriveStep::Exit`] wherever the writer
    /// loop previously returned, so the caller can exit the thread.
    ///
    /// `events_since_sync` is threaded by `&mut` so its count persists across
    /// commands exactly as it did when this body lived inline in `writer_loop`.
    ///
    /// `pub(super)` so the cooperative pump in the parent `writer` module can
    /// run the identical per-command pipeline inline on the calling thread.
    pub(super) fn drive_command(
        &mut self,
        rx: &Receiver<WriterCommand>,
        validated_cfg: &ValidatedStoreConfig,
        config: &StoreConfig,
        events_since_sync: &mut u32,
        cmd: WriterCommand,
    ) -> DriveStep {
        let result = self.execute_command(WriterLoopPhase::Main, cmd);
        if let Some(respond) = result.shutdown_drain_respond {
            let shutdown_result =
                drain_shutdown_queue(self, rx, validated_cfg.shutdown_drain_limit);
            ignore_closed_response_channel(respond.send(shutdown_result));
            return DriveStep::Exit;
        }

        let outcome = settle_command_result(self, events_since_sync, result);
        if outcome.exit_writer {
            return DriveStep::Exit;
        }

        if outcome.enter_group_commit_drain {
            let extra_budget = validated_cfg.group_commit_drain_budget;
            let mut drained = 0u32;
            while group_commit_drain_budget_remaining(drained, extra_budget) {
                let Ok(next_cmd) = rx.try_recv() else {
                    break;
                };
                let drain_result =
                    self.execute_command(WriterLoopPhase::GroupCommitDrain, next_cmd);
                let drain_outcome = settle_command_result(self, events_since_sync, drain_result);
                drained = drained.saturating_add(drain_outcome.sync_event_delta);
                if drain_outcome.exit_writer {
                    return DriveStep::Exit;
                }
                if drain_outcome.break_loop {
                    break;
                }
            }
        }

        if *events_since_sync >= config.sync.every_n_events {
            if let Err(error) = self.sync_active_segment() {
                tracing::error!("periodic sync failed: {error}");
            }
            *events_since_sync = 0;
        }

        DriveStep::Continue
    }
}

/// The writer's main loop. Runs on the background thread.
/// The spawn closure owns the Arcs; this function borrows them.
fn writer_loop(runtime: WriterRuntime<'_>, active_segment: Segment<Active>, segment_id: u64) {
    let mut events_since_sync: u32 = 0;

    let rx = runtime.rx;
    let config = Arc::clone(&runtime.config);
    let validated_cfg = Arc::clone(&runtime.validated_cfg);

    let mut state = WriterCore {
        index: runtime.index,
        active_segment,
        segment_id,
        config: runtime.config,
        runtime: runtime.validated_cfg,
        subscribers: runtime.subscribers,
        reactor_subscribers: runtime.reactor_subscribers,
        reader: runtime.reader,
        watermark_handle: runtime.watermark_handle,
        sidx_collector: crate::store::segment::sidx::SidxEntryCollector::new(),
        fence_ledger: None,
    };

    for cmd in rx.iter() {
        match state.drive_command(rx, &validated_cfg, &config, &mut events_since_sync, cmd) {
            DriveStep::Exit => return,
            DriveStep::Continue => {}
        }
    }
}

fn settle_command_result(
    state: &mut WriterCore,
    events_since_sync: &mut u32,
    result: CommandResult,
) -> LoopOutcome {
    *events_since_sync = events_since_sync.saturating_add(result.sync_event_delta);

    if result.must_sync_before_continue {
        let sync_result = state.sync_active_segment();
        if let Err(error) = &sync_result {
            tracing::error!("writer sync barrier failed: {error}");
        }
        drop(result.deferred_reply.send(state, sync_result));
        *events_since_sync = 0;
    }

    LoopOutcome {
        break_loop: result.break_after_reply,
        exit_writer: result.exit_writer && result.shutdown_drain_respond.is_none(),
        sync_event_delta: result.sync_event_delta,
        enter_group_commit_drain: result.enter_group_commit_drain,
    }
}

fn drain_shutdown_queue(
    state: &mut WriterCore,
    rx: &Receiver<WriterCommand>,
    shutdown_drain_limit: usize,
) -> Result<(), StoreError> {
    let mut drained = 0usize;
    let mut shutdown_sync_count = 0u32;
    while drained < shutdown_drain_limit {
        let Ok(cmd) = rx.try_recv() else {
            break;
        };
        let result = state.execute_command(WriterLoopPhase::ShutdownDrain, cmd);
        let _loop_outcome = settle_command_result(state, &mut shutdown_sync_count, result);
        drained += 1;
    }

    state.auto_cancel_fence_on_shutdown();
    if let Err(error) = state
        .active_segment
        .write_sidx_footer(&state.sidx_collector)
    {
        tracing::warn!("shutdown SIDX footer write failed (non-fatal): {error}");
    }
    let sync_result = state.sync_active_segment();
    if let Err(error) = &sync_result {
        tracing::error!("shutdown sync failed: {error}");
    }
    sync_result
}

/// Find the latest segment ID by scanning data_dir for .fbat files.
pub(crate) fn find_latest_segment_id(dir: &std::path::Path) -> Result<Option<u64>, StoreError> {
    let mut latest = None;
    for entry in crate::store::platform::fs::read_dir(dir).map_err(StoreError::Io)? {
        let entry = entry.map_err(StoreError::Io)?;
        let path = entry.path();
        match StoreFileKind::from_path(&path) {
            StoreFileKind::Segment(segment_id) => {
                latest = Some(latest.unwrap_or(0).max(segment_id.as_u64()));
            }
            StoreFileKind::MalformedSegment(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "skipping malformed segment filename"
                );
            }
            StoreFileKind::VisibilityRanges
            | StoreFileKind::Checkpoint
            | StoreFileKind::MmapIndex
            | StoreFileKind::IdempotencyStore
            | StoreFileKind::PendingCompactionMarker
            | StoreFileKind::CompactSource
            | StoreFileKind::CursorDirectory
            | StoreFileKind::Other => {}
        }
    }
    Ok(latest)
}

#[cfg(test)]
mod mutation_tests;

#[cfg(test)]
mod tests;
