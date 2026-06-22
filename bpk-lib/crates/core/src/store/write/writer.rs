// Intentional impossible-feature guard: exponential backoff belongs in the
// product supervisor, not the library (ADR-0006: only Once and Bounded restart
// policies). The `exponential-backoff` feature is deliberately undeclared in
// Cargo.toml; build.rs registers the cfg via `cargo::rustc-check-cfg` so this
// compile_error tripwire fires only if someone adds the feature, warning-free.
#[cfg(feature = "exponential-backoff")]
compile_error!(
    "Red flag: only Once and Bounded restart policies. \
     Exponential backoff belongs in the product's supervisor, not here. \
     See CIRCUITS.md."
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
#[cfg(test)]
use crate::store::SystemClock;
use crate::store::{AppendReceipt, StoreConfig, StoreError};
use flume::{Receiver, Sender};
use std::sync::Arc;
mod append;
mod batch;
mod fence_runtime;
mod publish;
mod runtime;
mod watermark;

pub(crate) use self::append::AppendGuards;
use self::fence_runtime::{CommandResult, DeferredReply, FenceLedger};
pub(crate) use self::runtime::find_latest_segment_id;
use self::runtime::{writer_thread_main, writer_thread_name, WriterRuntime};
pub(crate) use self::watermark::{WatermarkAdvanceHandle, WatermarkState};

pub(super) fn checked_next_clock(
    latest_clock: Option<u32>,
    entity: &str,
) -> Result<u32, StoreError> {
    match latest_clock {
        Some(clock) => clock
            .checked_add(1)
            .ok_or_else(|| StoreError::EntityClockOverflow {
                entity: entity.to_string(),
            }),
        None => Ok(0),
    }
}

pub(super) fn ignore_closed_response_channel<T>(result: Result<(), flume::SendError<T>>) {
    drop(result);
}

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
    watermark_handle: WatermarkAdvanceHandle,
    thread: Option<Box<dyn crate::store::platform::spawn::SimJoin>>,
}

/// RestartPolicy: how the writer recovers from panics.
/// Keep this surface intentionally small: exactly two variants. The enum is
/// deliberately exhaustive (not `#[non_exhaustive]`) so every match over it is
/// total without a forward-compat wildcard arm.
#[derive(Clone, Debug, Default)]
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
        config
            .fs()
            .create_dir_all(&config.data_dir)
            .map_err(StoreError::Io)?;
        let initial_segment_id = find_latest_segment_id(&config.data_dir)?.unwrap_or(0) + 1;
        let initial_segment = Segment::<Active>::create_with_created_ns_on(
            &config.data_dir,
            initial_segment_id,
            runtime.now_wall_ns(),
            config.fs(),
        )?;

        let (tx, rx) = flume::bounded::<WriterCommand>(config.writer.channel_capacity);
        let subs = Arc::clone(subscribers);
        let reactor_subs = Arc::clone(reactor_subscribers);
        let watermark_handle = WatermarkState::handle(runtime.clock_arc());
        let cfg = Arc::clone(config);
        let validated = Arc::clone(runtime);
        let idx = Arc::clone(index);
        let rdr = Arc::clone(reader);
        let watermark_for_thread = watermark_handle.clone();

        let thread = config
            .spawner()
            .spawn(
                writer_thread_name(&config.data_dir),
                config.writer.stack_size,
                Box::new(move || {
                    writer_thread_main(
                        WriterRuntime {
                            rx: &rx,
                            config: &cfg,
                            validated_cfg: &validated,
                            index: &idx,
                            subscribers: &subs,
                            reactor_subscribers: &reactor_subs,
                            reader: &rdr,
                            watermark_handle: &watermark_for_thread,
                        },
                        initial_segment,
                        initial_segment_id,
                    );
                }),
            )
            .map_err(StoreError::Io)?;

        Ok(Self {
            tx,
            subscribers: Arc::clone(subscribers),
            reactor_subscribers: Arc::clone(reactor_subscribers),
            watermark_handle,
            thread: Some(thread),
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
            watermark_handle: WatermarkState::handle(Arc::new(SystemClock::new())),
            thread: None,
        }
    }

    pub(crate) fn watermark_handle(&self) -> WatermarkAdvanceHandle {
        self.watermark_handle.clone()
    }

    pub(crate) fn fail_if_exited(&self) -> Result<(), StoreError> {
        if self
            .thread
            .as_ref()
            .is_some_and(|thread| thread.is_finished())
        {
            self.watermark_handle.mark_writer_crashed();
            return Err(StoreError::WriterCrashed);
        }
        Ok(())
    }

    pub(crate) fn join(&mut self) -> Result<(), StoreError> {
        if let Some(thread) = self.thread.take() {
            thread.join().map_err(|_| {
                self.watermark_handle.mark_writer_crashed();
                StoreError::WriterCrashed
            })?;
        }
        Ok(())
    }

    /// Test-only: abandon the writer the way a power loss would — close its
    /// command channel (so the loop ends WITHOUT a `Shutdown`-triggered drain,
    /// footer, or final sync) and join the thread to quiescence. Consumes the
    /// handle, dropping `tx` (the sole sender) so the writer's `rx.iter()`
    /// terminates naturally. Used by [`super::super::Store::abandon_without_shutdown`].
    #[cfg(feature = "dangerous-test-hooks")]
    pub(crate) fn close_channel_and_join(self) {
        let Self { tx, thread, .. } = self;
        // Drop the only sender first so the writer loop's `rx.iter()` ends.
        drop(tx);
        if let Some(thread) = thread {
            let _join_result = thread.join();
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
    runtime: &'a ValidatedStoreConfig,
    subscribers: &'a SubscriberList,
    reactor_subscribers: &'a ReactorSubscriberList,
    /// Reader handle — updated on segment rotation so mmap dispatch is correct.
    reader: Arc<crate::store::segment::scan::Reader>,
    /// Shared frontier state for coherent watermark snapshots.
    watermark_handle: WatermarkAdvanceHandle,
    /// Accumulates SIDX entries for the current active segment.
    /// Flushed as a footer on segment rotation and shutdown.
    sidx_collector: crate::store::segment::sidx::SidxEntryCollector,
    /// Currently active public visibility fence, if any.
    fence_ledger: Option<FenceLedger>,
}

#[cfg(test)]
mod tests {
    use super::watermark::elapsed_ms_since;
    use super::{
        checked_next_clock, ReactorSubscriberList, SubscriberList, WatermarkState, WriterCommand,
        WriterHandle,
    };
    use crate::store::stats::HlcPoint;
    use crate::store::{StoreError, SystemClock};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn checked_next_clock_advances_and_overflow_fails_closed() {
        assert_eq!(
            checked_next_clock(None, "entity:test").expect("genesis clock"),
            0
        );
        assert_eq!(
            checked_next_clock(Some(7), "entity:test").expect("increment clock"),
            8
        );

        let err = checked_next_clock(Some(u32::MAX), "entity:overflow")
            .expect_err("entity clock overflow must fail closed");
        assert!(matches!(
            err,
            StoreError::EntityClockOverflow { ref entity } if entity == "entity:overflow"
        ));
    }

    #[test]
    fn duplicate_accepted_advance_does_not_restart_pending_write_age() {
        let point = HlcPoint {
            wall_ms: 10,
            global_sequence: 1,
        };
        let mut state = WatermarkState::default();

        state.advance_accepted_on_lane(0, point);
        state.advance_durable(point);
        assert_eq!(
            state.snapshot().oldest_pending_write_age_ms,
            None,
            "PROPERTY: durability to accepted clears pending write age"
        );

        state.advance_accepted_on_lane(0, point);
        assert_eq!(
            state.snapshot().oldest_pending_write_age_ms,
            None,
            "PROPERTY: duplicate accepted advance must not reopen pending write age"
        );
    }

    #[test]
    fn pending_write_age_reports_elapsed_milliseconds_not_nanoseconds_or_products() {
        assert_eq!(
            elapsed_ms_since(3_500_000, 1_000_000),
            2,
            "PROPERTY: frontier pending-write age is floor(elapsed_ns / 1_000_000)"
        );
        assert_eq!(
            elapsed_ms_since(1_000_000, 3_500_000),
            0,
            "PROPERTY: backwards monotonic samples saturate to zero"
        );
    }

    #[test]
    fn writer_handle_join_surfaces_thread_panic_and_poisons_watermarks() {
        let (tx, _rx) = flume::bounded::<WriterCommand>(1);
        let watermark_handle = WatermarkState::handle(Arc::new(SystemClock::new()));
        let thread = crate::store::platform::spawn::Spawn::spawn(
            &crate::store::platform::spawn::ThreadSpawn,
            "writer-join-panic-proof".to_owned(),
            None,
            Box::new(|| {
                // Deterministically unwind this writer body to prove join
                // surfaces the panic as WriterCrashed. `black_box` hides the
                // `None` from the literal-unwrap lint; `expect` is the
                // permitted in-test panic shape (not the `panic!` macro).
                std::hint::black_box(Option::<()>::None)
                    .expect("intentional writer join panic proof");
            }),
        )
        .expect("spawn panic proof thread");

        let mut handle = WriterHandle {
            tx,
            subscribers: Arc::new(SubscriberList::new()),
            reactor_subscribers: Arc::new(ReactorSubscriberList::new()),
            watermark_handle: watermark_handle.clone(),
            thread: Some(thread),
        };

        let err = handle
            .join()
            .expect_err("PROPERTY: writer thread panic must surface through join");
        assert!(matches!(err, StoreError::WriterCrashed));

        let poisoned =
            watermark_handle.wait_for_durable(HlcPoint::ORIGIN, Duration::from_millis(1));
        assert!(
            matches!(poisoned, Err(StoreError::WriterCrashed)),
            "PROPERTY: join panic must poison frontier waiters"
        );
    }

    #[test]
    fn dangerous_notify_all_wakes_condvar_waiters() {
        let handle = WatermarkState::handle(std::sync::Arc::new(crate::store::SystemClock::new()));
        let waiter_handle = handle.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();

        let waiter = std::thread::Builder::new()
            .name("watermark-dangerous-notify-proof".to_string())
            .spawn(move || {
                ready_tx.send(()).expect("signal waiter readiness");
                let timed_out =
                    waiter_handle.dangerous_wait_for_notification(Duration::from_secs(2));
                done_tx.send(timed_out).expect("signal waiter outcome");
            })
            .expect("spawn condvar waiter");

        ready_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("waiter reached condvar wait setup");
        let deadline = Instant::now() + Duration::from_secs(1);
        let timed_out = loop {
            handle.dangerous_notify_all();
            match done_rx.recv_timeout(Duration::from_millis(10)) {
                Ok(timed_out) => break timed_out,
                Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
                Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {
                    break true;
                }
            }
        };

        waiter.join().expect("condvar waiter joins");
        assert!(
            !timed_out,
            "PROPERTY: dangerous_notify_all must wake frontier waiters before their timeout"
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WriterLoopPhase {
    Main,
    GroupCommitDrain,
    ShutdownDrain,
}

impl WriterState<'_> {
    fn execute_command(&mut self, phase: WriterLoopPhase, cmd: WriterCommand) -> CommandResult {
        match cmd {
            WriterCommand::BeginVisibilityFence { token, respond } => match phase {
                WriterLoopPhase::Main | WriterLoopPhase::ShutdownDrain => {
                    ignore_closed_response_channel(
                        respond.send(self.begin_visibility_fence(token)),
                    );
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
                ignore_closed_response_channel(respond.send(result));
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
                    ignore_closed_response_channel(respond.send(Err(error)));
                    CommandResult::immediate(0)
                } else {
                    CommandResult::immediate(1)
                }
            }
            WriterCommand::AppendBatch { items, respond } => {
                let result = self.handle_append_batch(items, None);
                ignore_closed_response_channel(respond.send(result));
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
                    ignore_closed_response_channel(respond.send(Err(error)));
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
                    ignore_closed_response_channel(
                        respond.send(self.commit_visibility_fence(token)),
                    );
                    CommandResult::immediate(0)
                }
            },
            WriterCommand::CancelVisibilityFence { token, respond } => {
                ignore_closed_response_channel(respond.send(self.cancel_visibility_fence(token)));
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
                    ignore_closed_response_channel(respond.send(self.sync_active_segment()));
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
                    ignore_closed_response_channel(respond.send(Ok(())));
                    CommandResult::immediate(0)
                }
            },
            #[cfg(feature = "dangerous-test-hooks")]
            WriterCommand::PanicForTest { respond } => match phase {
                WriterLoopPhase::Main => {
                    ignore_closed_response_channel(respond.send(Ok(())));
                    std::panic::resume_unwind(Box::new(
                        "PanicForTest: intentional writer panic for restart_policy testing",
                    ));
                }
                WriterLoopPhase::GroupCommitDrain | WriterLoopPhase::ShutdownDrain => {
                    ignore_closed_response_channel(respond.send(Ok(())));
                    CommandResult::immediate(0).break_after_reply()
                }
            },
        }
    }

    fn sync_active_segment(&mut self) -> Result<(), StoreError> {
        self.active_segment.sync_with_mode(&self.config.sync.mode)?;
        self.watermark_handle.lock().advance_durable_to_accepted();
        Ok(())
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
        #[cfg(feature = "dangerous-test-hooks")]
        let old_segment = *self.segment_id;
        #[cfg(feature = "dangerous-test-hooks")]
        let new_segment = *self.segment_id + 1;
        // Create + fsync the NEW segment FIRST, before touching the old segment
        // or the collector. `create_with_created_ns` performs the file create
        // plus the file/dir fsync (Batch F/C4), and is the only step here that
        // both can fail AND has rollback-requiring side effects. If it fails we
        // return `?` with the old segment and collector FULLY INTACT: rotation
        // simply did not happen. The triggering append errors cleanly and the
        // next append retries against the unchanged old segment — no SIDX footer
        // has been written to the old segment, the collector still holds every
        // old entry, so there is no "frame bytes after footer bytes" corruption
        // and no lost SIDX coverage. (Previously the footer write + collector
        // reset + old-segment sync happened BEFORE this fallible create, so a
        // create/fsync failure left the writer running on a half-sealed old
        // segment with a wiped collector — the silent-corruption P1.)
        //
        // Fault injection point models the create/fsync FAILING here, while the
        // old segment + collector are still pristine. Firing it before the real
        // create (rather than making the create itself fallible-by-injection)
        // exercises the exact same rollback path: `?` returns with nothing
        // mutated, so rotation cleanly did not happen.
        #[cfg(feature = "dangerous-test-hooks")]
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::SegmentRotationCreate {
                old_segment,
                new_segment,
            },
            &self.config.fault_injector,
        )?;
        let new_active = Segment::<Active>::create_with_created_ns_on(
            &self.config.data_dir,
            *self.segment_id + 1,
            self.runtime.now_wall_ns(),
            self.config.fs(),
        )?;
        // New segment is durably present. Now flush the OLD segment's committed
        // frames while it is still pristine — before writing the footer or
        // resetting the collector. This is the second (and last) fallible step
        // that touches the old segment, so doing it here keeps the same
        // invariant: if it fails we return `?` with the old segment and
        // collector still fully intact (no footer written, collector unchanged),
        // so rotation cleanly did not happen and the next append retries against
        // the unchanged old segment.
        self.active_segment.sync_with_mode(&self.config.sync.mode)?;
        self.watermark_handle.lock().advance_durable_to_accepted();
        // From here on every step is infallible or non-fatal, so the rotation
        // completes atomically with respect to its fallible side effects.
        //
        // Write SIDX footer before sealing. append_frames_from_segment now
        // strips SIDX data via detect_sidx_boundary, so this is safe. The footer
        // is a cold-start fast-rebuild optimization: if writing OR its best-effort
        // durability flush fails, recovery falls back to a full frame scan, so
        // both are treated as non-fatal and never abort the rotation (aborting
        // here would reintroduce the half-rotated state this reorder fixes — the
        // footer would be partially written and the collector about to be reset).
        if let Err(e) = self.active_segment.write_sidx_footer(&self.sidx_collector) {
            tracing::warn!("SIDX footer write failed (non-fatal): {e}");
        } else if let Err(e) = self.active_segment.sync_with_mode(&self.config.sync.mode) {
            tracing::warn!("SIDX footer durability flush failed (non-fatal): {e}");
        }
        self.sidx_collector = crate::store::segment::sidx::SidxEntryCollector::new();
        let old = std::mem::replace(self.active_segment, new_active);
        let _sealed = old.seal();
        *self.segment_id += 1;
        // Notify the reader of the new active segment so mmap dispatch is correct.
        self.reader.set_active_segment(*self.segment_id);
        // Inject a crash during rotation AFTER in-memory state is fully rolled
        // forward (active segment swapped, id incremented, reader advanced), so a
        // returned injected error leaves writer state CONSISTENT — a real crash
        // discards in-memory state, it does not leave active_segment pointing at
        // the new file while segment_id still names the old one (which would make
        // the next append publish DiskPos values that read the wrong segment).
        // The on-disk recovery scenario is unchanged: old segment sealed (with
        // footer), new empty active file present, triggering append not yet
        // written — exactly what cold-start recovery must handle.
        #[cfg(feature = "dangerous-test-hooks")]
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::SegmentRotation {
                old_segment,
                new_segment,
            },
            &self.config.fault_injector,
        )?;
        Ok(true)
    }
}
