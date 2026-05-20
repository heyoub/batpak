use super::fence_runtime::CommandResult;
use super::{
    Active, Receiver, RestartPolicy, Segment, StoreConfig, StoreError, ValidatedStoreConfig,
    WriterCommand, WriterLoopPhase, WriterState,
};
use crate::store::index::StoreIndex;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::Arc;

#[derive(Clone, Copy)]
pub(super) struct WriterRuntime<'a> {
    pub(super) rx: &'a Receiver<WriterCommand>,
    pub(super) config: &'a StoreConfig,
    pub(super) validated_cfg: &'a ValidatedStoreConfig,
    pub(super) index: &'a StoreIndex,
    pub(super) subscribers: &'a super::SubscriberList,
    pub(super) reactor_subscribers: &'a super::ReactorSubscriberList,
    pub(super) reader: &'a Arc<crate::store::segment::scan::Reader>,
    pub(super) watermark_handle: &'a super::WatermarkAdvanceHandle,
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
    runtime: WriterRuntime<'_>,
    initial_segment: Segment<Active>,
    initial_segment_id: u64,
) {
    let mut segment = initial_segment;
    let mut seg_id = initial_segment_id;
    let mut restarts: u32 = 0;
    let mut window_start = runtime.validated_cfg.now_mono_ns();

    loop {
        let rdr = Arc::clone(runtime.reader);
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            writer_loop(
                WriterRuntime {
                    rx: runtime.rx,
                    config: runtime.config,
                    validated_cfg: runtime.validated_cfg,
                    index: runtime.index,
                    subscribers: runtime.subscribers,
                    reactor_subscribers: runtime.reactor_subscribers,
                    reader: &rdr,
                    watermark_handle: runtime.watermark_handle,
                },
                segment,
                seg_id,
            );
        }));

        match result {
            Ok(()) => return,
            Err(panic_info) => {
                runtime.watermark_handle.mark_writer_crashed();
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
                        RestartPolicy::Once => 1,
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

                seg_id = find_latest_segment_id(&runtime.config.data_dir).unwrap_or(seg_id) + 1;
                segment = match Segment::<Active>::create_with_created_ns(
                    &runtime.config.data_dir,
                    seg_id,
                    runtime.validated_cfg.now_wall_ns(),
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!(
                            "writer restart failed — cannot create segment: {e}. Thread exiting."
                        );
                        return;
                    }
                };
            }
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

/// The writer's main loop. Runs on the background thread.
/// The spawn closure owns the Arcs; this function borrows them.
fn writer_loop(
    runtime: WriterRuntime<'_>,
    mut active_segment: Segment<Active>,
    mut segment_id: u64,
) {
    let mut events_since_sync: u32 = 0;

    let mut state = WriterState {
        index: runtime.index,
        active_segment: &mut active_segment,
        segment_id: &mut segment_id,
        config: runtime.config,
        runtime: runtime.validated_cfg,
        subscribers: runtime.subscribers,
        reactor_subscribers: runtime.reactor_subscribers,
        reader: Arc::clone(runtime.reader),
        watermark_handle: runtime.watermark_handle.clone(),
        sidx_collector: crate::store::segment::sidx::SidxEntryCollector::new(),
        fence_ledger: None,
    };

    for cmd in runtime.rx.iter() {
        let result = state.execute_command(WriterLoopPhase::Main, cmd);
        if let Some(respond) = result.shutdown_drain_respond {
            let shutdown_result = drain_shutdown_queue(
                &mut state,
                runtime.rx,
                runtime.validated_cfg.shutdown_drain_limit,
            );
            let _ = respond.send(shutdown_result);
            return;
        }

        let outcome = settle_command_result(&mut state, &mut events_since_sync, result);
        if outcome.exit_writer {
            return;
        }

        if outcome.enter_group_commit_drain {
            let extra_budget = runtime.validated_cfg.group_commit_drain_budget;
            let mut drained = 0u32;
            while drained < extra_budget {
                let Ok(next_cmd) = runtime.rx.try_recv() else {
                    break;
                };
                let drain_result =
                    state.execute_command(WriterLoopPhase::GroupCommitDrain, next_cmd);
                let drain_outcome =
                    settle_command_result(&mut state, &mut events_since_sync, drain_result);
                drained = drained.saturating_add(drain_outcome.sync_event_delta);
                if drain_outcome.exit_writer {
                    return;
                }
                if drain_outcome.break_loop {
                    break;
                }
            }
        }

        if events_since_sync >= runtime.config.sync.every_n_events {
            if let Err(error) = state.sync_active_segment() {
                tracing::error!("periodic sync failed: {error}");
            }
            events_since_sync = 0;
        }
    }
}

fn settle_command_result(
    state: &mut WriterState<'_>,
    events_since_sync: &mut u32,
    result: CommandResult,
) -> LoopOutcome {
    *events_since_sync = events_since_sync.saturating_add(result.sync_event_delta);

    if result.must_sync_before_continue {
        let sync_result = state.sync_active_segment();
        if let Err(error) = &sync_result {
            tracing::error!("writer sync barrier failed: {error}");
        }
        let _ = result.deferred_reply.send(state, sync_result);
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
    state: &mut WriterState<'_>,
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
        let _ = settle_command_result(state, &mut shutdown_sync_count, result);
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
pub(crate) fn find_latest_segment_id(dir: &std::path::Path) -> Option<u64> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            if !path
                .extension()
                .map(|ext| ext == crate::store::segment::SEGMENT_EXTENSION)
                .unwrap_or(false)
            {
                return None;
            }
            match crate::store::segment::SegmentId::from_filename(&path) {
                Ok(parsed) => Some(parsed.as_u64()),
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "skipping malformed segment filename"
                    );
                    None
                }
            }
        })
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn writer_thread_name_is_stable_nonempty_and_prefixed() {
        let path = Path::new("batpak/writer-name");
        let name = writer_thread_name(path);

        assert!(
            name.starts_with("batpak-writer-"),
            "PROPERTY: writer thread names carry a stable batpak prefix for diagnostics"
        );
        assert!(
            name.len() > "batpak-writer-".len(),
            "PROPERTY: writer thread names include a data-dir-derived suffix rather than the empty string"
        );
        assert_eq!(
            name,
            writer_thread_name(path),
            "PROPERTY: writer thread names are deterministic for a store directory"
        );
        assert_ne!(
            name,
            writer_thread_name(Path::new("batpak/other-writer-name")),
            "PROPERTY: distinct store directories should not collapse to one diagnostic thread name"
        );
    }

    #[test]
    fn restart_budget_once_allows_exactly_one_restart() {
        let mut restarts = 0;
        let mut window_start = 0;

        assert!(
            restart_budget_allows(&RestartPolicy::Once, &mut restarts, &mut window_start, 0,),
            "PROPERTY: RestartPolicy::Once grants the first restart"
        );
        assert_eq!(
            restarts, 1,
            "PROPERTY: accepting a restart increments the budget counter"
        );
        assert!(
            !restart_budget_allows(&RestartPolicy::Once, &mut restarts, &mut window_start, 0,),
            "PROPERTY: RestartPolicy::Once rejects a second restart"
        );
        assert_eq!(
            restarts, 1,
            "PROPERTY: rejecting a restart must not mutate the accepted restart count"
        );
    }

    #[test]
    fn bounded_restart_budget_resets_after_window() {
        let policy = RestartPolicy::Bounded {
            max_restarts: 1,
            within_ms: 10,
        };
        let base = 1_000_000_000;
        let mut window_start = base;
        let mut restarts = 0;

        assert!(
            restart_budget_allows(&policy, &mut restarts, &mut window_start, base),
            "PROPERTY: bounded policy accepts the first restart in the window"
        );
        assert!(
            !restart_budget_allows(&policy, &mut restarts, &mut window_start, base + 1_000_000),
            "PROPERTY: bounded policy rejects restarts past the per-window cap"
        );
        assert!(
            restart_budget_allows(&policy, &mut restarts, &mut window_start, base + 11_000_000),
            "PROPERTY: bounded policy resets after its configured time window"
        );
        assert_eq!(
            restarts, 1,
            "PROPERTY: reset starts a fresh window with one accepted restart"
        );
    }
}
