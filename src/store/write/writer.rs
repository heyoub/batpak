// Intentional impossible-feature guard: exponential backoff belongs in the product supervisor, not the library.
// exponential-backoff is not a declared feature — suppress cfg warning for this guard
// justifies: ADR-0006; the `exponential-backoff` feature is deliberately undeclared in src/store/write/writer.rs — this block is a compile_error tripwire for anyone who adds the feature to Cargo.toml.
#[allow(unexpected_cfgs)]
#[cfg(feature = "exponential-backoff")]
compile_error!(
    "Red flag: only Once and Bounded restart policies. \
     Exponential backoff belongs in the product's supervisor, not here. \
     See: REFERENCE.md."
);

pub use super::fanout::Notification;
use super::fanout::{ReactorSubscriberList, SubscriberList};
use super::staging::{StagedCommitMeta, StagedCommitTiming, StagedCommittedEvent};
use crate::coordinate::{Coordinate, DagPosition};
use crate::event::{Event, EventHeader, EventKind, HashChain};
use crate::store::append::BatchAppendItem;
use crate::store::config::ValidatedStoreConfig;
use crate::store::index::{DiskPos, StoreIndex};
use crate::store::segment::sidx::kind_to_raw;
use crate::store::segment::{self, Active, FramePayloadRef, Segment};
use crate::store::{AppendReceipt, StoreConfig, StoreError};
use flume::{Receiver, Sender};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Instant;
mod append;
mod batch;
mod fence_runtime;
mod publish;

pub(crate) use self::append::AppendGuards;
use self::fence_runtime::{CommandResult, DeferredReply, FenceLedger};

/// WriterCommand: messages sent to the background writer thread via flume.
/// All respond channels use `flume::Sender`: sync send from the writer, async recv from callers.
pub(crate) enum WriterCommand {
    BeginVisibilityFence {
        token: u64,
        respond: Sender<Result<(), StoreError>>,
    },
    Append {
        coord: Coordinate,
        event: Box<Event<Vec<u8>>>, // pre-serialized payload as msgpack bytes
        kind: EventKind,
        guards: AppendGuards,
        respond: Sender<Result<AppendReceipt, StoreError>>,
    },
    FenceAppend {
        token: u64,
        coord: Coordinate,
        event: Box<Event<Vec<u8>>>,
        kind: EventKind,
        guards: AppendGuards,
        respond: Sender<Result<AppendReceipt, StoreError>>,
    },
    AppendBatch {
        items: Vec<BatchAppendItem>,
        respond: Sender<Result<Vec<AppendReceipt>, StoreError>>,
    },
    FenceAppendBatch {
        token: u64,
        items: Vec<BatchAppendItem>,
        respond: Sender<Result<Vec<AppendReceipt>, StoreError>>,
    },
    CommitVisibilityFence {
        token: u64,
        respond: Sender<Result<(), StoreError>>,
    },
    CancelVisibilityFence {
        token: u64,
        respond: Sender<Result<(), StoreError>>,
    },
    Sync {
        respond: Sender<Result<(), StoreError>>,
    },
    Shutdown {
        respond: Sender<Result<(), StoreError>>,
    },
    /// Test-only: trigger a panic in the writer thread to exercise restart_policy.
    #[cfg(feature = "dangerous-test-hooks")]
    #[doc(hidden)]
    PanicForTest {
        respond: Sender<Result<(), StoreError>>,
    },
}

/// WriterHandle: owned by Store. Communicates with the background thread.
pub(crate) struct WriterHandle {
    pub tx: Sender<WriterCommand>,
    pub subscribers: Arc<SubscriberList>,
    pub reactor_subscribers: Arc<ReactorSubscriberList>,
    _thread: Option<std::thread::JoinHandle<()>>,
}

/// RestartPolicy: how the writer recovers from panics.
/// Keep this surface intentionally small: exactly two variants.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub enum RestartPolicy {
    /// Allow at most one automatic restart after a writer panic.
    #[default]
    Once,
    /// Allow up to `max_restarts` automatic restarts within a rolling `within_ms` millisecond window.
    Bounded {
        /// Maximum number of restarts permitted within the time window.
        max_restarts: u32,
        /// Time window in milliseconds over which `max_restarts` is enforced.
        within_ms: u64,
    },
}

impl WriterHandle {
    /// Spawn the background writer thread.
    pub(crate) fn spawn(
        config: &Arc<StoreConfig>,
        runtime: &Arc<ValidatedStoreConfig>,
        index: &Arc<StoreIndex>,
        subscribers: &Arc<SubscriberList>,
        reactor_subscribers: &Arc<ReactorSubscriberList>,
        reader: &Arc<crate::store::segment::scan::Reader>,
    ) -> Result<Self, StoreError> {
        // Fallible init — propagate errors to Store::open() caller
        std::fs::create_dir_all(&config.data_dir).map_err(StoreError::Io)?;
        let initial_segment_id = find_latest_segment_id(&config.data_dir).unwrap_or(0) + 1;
        let initial_segment = Segment::<Active>::create(&config.data_dir, initial_segment_id)?;

        let (tx, rx) = flume::bounded::<WriterCommand>(config.writer.channel_capacity);
        let subs = Arc::clone(subscribers);
        let reactor_subs = Arc::clone(reactor_subscribers);
        let cfg = Arc::clone(config);
        let validated = Arc::clone(runtime);
        let idx = Arc::clone(index);
        let rdr = Arc::clone(reader);

        const FNV_1A_BASIS: u64 = 0xcbf29ce484222325;
        const FNV_1A_PRIME: u64 = 0x100000001b3;
        let thread_name = format!("batpak-writer-{:08x}", {
            let mut h: u64 = FNV_1A_BASIS;
            for b in config.data_dir.to_string_lossy().bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(FNV_1A_PRIME);
            }
            h
        });

        let mut builder = std::thread::Builder::new().name(thread_name);
        if let Some(stack_size) = config.writer.stack_size {
            builder = builder.stack_size(stack_size);
        }
        let thread = builder
            .spawn(move || {
                writer_thread_main(
                    WriterRuntime {
                        rx: &rx,
                        config: &cfg,
                        validated_cfg: &validated,
                        index: &idx,
                        subscribers: &subs,
                        reactor_subscribers: &reactor_subs,
                        reader: &rdr,
                    },
                    initial_segment,
                    initial_segment_id,
                );
            })
            .map_err(StoreError::Io)?;

        Ok(Self {
            tx,
            subscribers: Arc::clone(subscribers),
            reactor_subscribers: Arc::clone(reactor_subscribers),
            _thread: Some(thread),
        })
    }

    #[cfg(test)]
    pub(crate) fn from_parts_for_test(
        tx: Sender<WriterCommand>,
        subscribers: Arc<SubscriberList>,
    ) -> Self {
        Self {
            tx,
            subscribers,
            reactor_subscribers: Arc::new(ReactorSubscriberList::new()),
            _thread: None,
        }
    }

    // NOTE: No send_append() method here. Store::append() and Store::append_reaction()
    // in store/mod.rs create one-shot flume channels and send WriterCommands directly
    // via self.writer.tx.send(). This avoids an unnecessary abstraction layer.
    // `WriterHandle::tx` is `pub(crate)` so store control paths can talk to the writer directly.
}

/// Writer's mutable runtime state, grouped to reduce handle_append param count.
struct WriterState<'a> {
    index: &'a StoreIndex,
    active_segment: &'a mut Segment<Active>,
    segment_id: &'a mut u64,
    config: &'a StoreConfig,
    subscribers: &'a SubscriberList,
    reactor_subscribers: &'a ReactorSubscriberList,
    /// Reader handle — updated on segment rotation so mmap dispatch is correct.
    reader: Arc<crate::store::segment::scan::Reader>,
    /// Accumulates SIDX entries for the current active segment.
    /// Flushed as a footer on segment rotation and shutdown.
    sidx_collector: crate::store::segment::sidx::SidxEntryCollector,
    /// Currently active public visibility fence, if any.
    fence_ledger: Option<FenceLedger>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WriterLoopPhase {
    Main,
    GroupCommitDrain,
    ShutdownDrain,
}

#[derive(Clone, Copy)]
struct WriterRuntime<'a> {
    rx: &'a Receiver<WriterCommand>,
    config: &'a StoreConfig,
    validated_cfg: &'a ValidatedStoreConfig,
    index: &'a StoreIndex,
    subscribers: &'a SubscriberList,
    reactor_subscribers: &'a ReactorSubscriberList,
    reader: &'a Arc<crate::store::segment::scan::Reader>,
}

/// Writer thread entry point with panic recovery and restart logic.
/// Wraps writer_loop() in catch_unwind, implementing RestartPolicy.
/// The rx (command receiver) survives across restarts because it lives
/// outside catch_unwind. Segments are re-created on restart since the
/// previous one is dropped during unwind.
fn writer_thread_main(
    runtime: WriterRuntime<'_>,
    initial_segment: Segment<Active>,
    initial_segment_id: u64,
) {
    let mut segment = initial_segment;
    let mut seg_id = initial_segment_id;
    let mut restarts: u32 = 0;
    let mut window_start = Instant::now();

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
                },
                segment,
                seg_id,
            );
        }));

        match result {
            Ok(()) => return, // clean shutdown via WriterCommand::Shutdown
            Err(panic_info) => {
                let panic_msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                    (*s).to_string()
                } else if let Some(s) = panic_info.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };

                let budget_ok = match &runtime.config.writer.restart_policy {
                    RestartPolicy::Once => {
                        if restarts >= 1 {
                            false
                        } else {
                            restarts += 1;
                            true
                        }
                    }
                    RestartPolicy::Bounded {
                        max_restarts,
                        within_ms,
                    } => {
                        // Reset counter if window has elapsed
                        if window_start.elapsed() > std::time::Duration::from_millis(*within_ms) {
                            restarts = 0;
                            window_start = Instant::now();
                        }
                        if restarts >= *max_restarts {
                            false
                        } else {
                            restarts += 1;
                            true
                        }
                    }
                };

                if !budget_ok {
                    tracing::error!(
                        "writer restart budget exhausted — thread exiting. \
                         Last panic: {panic_msg}. Policy: {:?}",
                        runtime.config.writer.restart_policy
                    );
                    // A2: auto-cancel any active visibility fence on
                    // terminal exit so cancellation is visible to callers
                    // holding a VisibilityFence handle and so the
                    // persisted hidden-ranges on disk stay honest.
                    //
                    // Pending fence response Senders inside the previous
                    // WriterState.fence_ledger were dropped during panic
                    // unwind; their receivers will observe
                    // `RecvError::Disconnected` and map it to
                    // `StoreError::WriterCrashed`, so no caller blocks
                    // forever. The index-side cancellation below is the
                    // durable counterpart — equivalent to
                    // `WriterState::auto_cancel_fence_on_shutdown` but
                    // operating without a live WriterState.
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

                // Re-create segment: the previous one was dropped during unwind.
                seg_id = find_latest_segment_id(&runtime.config.data_dir).unwrap_or(seg_id) + 1;
                segment = match Segment::<Active>::create(&runtime.config.data_dir, seg_id) {
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
        subscribers: runtime.subscribers,
        reactor_subscribers: runtime.reactor_subscribers,
        reader: Arc::clone(runtime.reader),
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

#[derive(Debug)]
struct LoopOutcome {
    break_loop: bool,
    exit_writer: bool,
    sync_event_delta: u32,
    enter_group_commit_drain: bool,
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

impl WriterState<'_> {
    fn execute_command(&mut self, phase: WriterLoopPhase, cmd: WriterCommand) -> CommandResult {
        match cmd {
            WriterCommand::BeginVisibilityFence { token, respond } => match phase {
                WriterLoopPhase::Main | WriterLoopPhase::ShutdownDrain => {
                    let _ = respond.send(self.begin_visibility_fence(token));
                    CommandResult::immediate(0)
                }
                WriterLoopPhase::GroupCommitDrain => CommandResult::immediate(0)
                    .with_sync(DeferredReply::BeginVisibilityFence { token, respond })
                    .break_after_reply(),
            },
            WriterCommand::Append {
                coord,
                event,
                kind,
                guards,
                respond,
            } => {
                let result = self.handle_append(&coord, *event, kind, &guards, None);
                let _ = respond.send(result);
                let base = CommandResult::immediate(1);
                if matches!(phase, WriterLoopPhase::Main) {
                    base.enter_group_commit_drain()
                } else {
                    base
                }
            }
            WriterCommand::FenceAppend {
                token,
                coord,
                event,
                kind,
                guards,
                respond,
            } => {
                if let Err(error) = self.handle_fence_append_command(
                    token,
                    &coord,
                    *event,
                    kind,
                    &guards,
                    respond.clone(),
                ) {
                    let _ = respond.send(Err(error));
                    CommandResult::immediate(0)
                } else {
                    CommandResult::immediate(1)
                }
            }
            WriterCommand::AppendBatch { items, respond } => {
                let result = self.handle_append_batch(items, None);
                let _ = respond.send(result);
                CommandResult::immediate(1)
            }
            WriterCommand::FenceAppendBatch {
                token,
                items,
                respond,
            } => {
                if let Err(error) =
                    self.handle_fence_append_batch_command(token, items, respond.clone())
                {
                    let _ = respond.send(Err(error));
                    CommandResult::immediate(0)
                } else {
                    CommandResult::immediate(1)
                }
            }
            WriterCommand::CommitVisibilityFence { token, respond } => match phase {
                WriterLoopPhase::Main | WriterLoopPhase::GroupCommitDrain => {
                    CommandResult::immediate(0)
                        .with_sync(DeferredReply::CommitVisibilityFence { token, respond })
                        .break_after_reply_if(matches!(phase, WriterLoopPhase::GroupCommitDrain))
                }
                WriterLoopPhase::ShutdownDrain => {
                    let _ = respond.send(self.commit_visibility_fence(token));
                    CommandResult::immediate(0)
                }
            },
            WriterCommand::CancelVisibilityFence { token, respond } => {
                let _ = respond.send(self.cancel_visibility_fence(token));
                let base = CommandResult::immediate(0);
                if matches!(phase, WriterLoopPhase::GroupCommitDrain) {
                    base.break_after_reply()
                } else {
                    base
                }
            }
            WriterCommand::Sync { respond } => match phase {
                WriterLoopPhase::Main | WriterLoopPhase::GroupCommitDrain => {
                    CommandResult::immediate(0)
                        .with_sync(DeferredReply::Sync { respond })
                        .break_after_reply_if(matches!(phase, WriterLoopPhase::GroupCommitDrain))
                }
                WriterLoopPhase::ShutdownDrain => {
                    let _ = respond.send(self.sync_active_segment());
                    CommandResult::immediate(0)
                }
            },
            WriterCommand::Shutdown { respond } => match phase {
                WriterLoopPhase::Main => CommandResult::immediate(0).enter_shutdown_drain(respond),
                WriterLoopPhase::GroupCommitDrain => CommandResult::immediate(0)
                    .with_sync(DeferredReply::Shutdown { respond })
                    .break_after_reply()
                    .exit_writer(),
                WriterLoopPhase::ShutdownDrain => {
                    let _ = respond.send(Ok(()));
                    CommandResult::immediate(0)
                }
            },
            #[cfg(feature = "dangerous-test-hooks")]
            WriterCommand::PanicForTest { respond } => match phase {
                WriterLoopPhase::Main => {
                    let _ = respond.send(Ok(()));
                    std::panic::resume_unwind(Box::new(
                        "PanicForTest: intentional writer panic for restart_policy testing",
                    ));
                }
                WriterLoopPhase::GroupCommitDrain | WriterLoopPhase::ShutdownDrain => {
                    let _ = respond.send(Ok(()));
                    CommandResult::immediate(0).break_after_reply()
                }
            },
        }
    }

    fn sync_active_segment(&mut self) -> Result<(), StoreError> {
        self.active_segment.sync_with_mode(&self.config.sync.mode)
    }

    /// Check whether the active segment needs rotation, and if so, seal it,
    /// write its SIDX footer, sync, and create a new active segment.
    ///
    /// Returns `Ok(true)` if a rotation occurred, `Ok(false)` if no rotation
    /// was needed. On rotation, the SIDX collector is reset, the old segment
    /// is sealed, segment_id is advanced, and the reader is notified.
    ///
    /// Callers needing batch-specific error context should wrap errors with
    /// the writer-local `batch_failed(...)` helper.
    fn maybe_rotate_segment(&mut self) -> Result<bool, StoreError> {
        if !self
            .active_segment
            .needs_rotation(self.config.segment_max_bytes)
        {
            return Ok(false);
        }
        // Write SIDX footer before sealing. append_frames_from_segment now
        // strips SIDX data via detect_sidx_boundary, so this is safe.
        if let Err(e) = self.active_segment.write_sidx_footer(&self.sidx_collector) {
            tracing::warn!("SIDX footer write failed (non-fatal): {e}");
        }
        self.sidx_collector = crate::store::segment::sidx::SidxEntryCollector::new();
        self.active_segment.sync_with_mode(&self.config.sync.mode)?;
        let old = std::mem::replace(
            self.active_segment,
            Segment::<Active>::create(&self.config.data_dir, *self.segment_id + 1)?,
        );
        let _sealed = old.seal();
        *self.segment_id += 1;
        // Notify the reader of the new active segment so mmap dispatch is correct.
        self.reader.set_active_segment(*self.segment_id);
        Ok(true)
    }
}

/// Find the latest segment ID by scanning data_dir for .fbat files.
pub(crate) fn find_latest_segment_id(dir: &std::path::Path) -> Option<u64> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_str()?;
            if name.ends_with(".fbat") {
                name.trim_end_matches(".fbat").parse::<u64>().ok()
            } else {
                None
            }
        })
        .max()
}
