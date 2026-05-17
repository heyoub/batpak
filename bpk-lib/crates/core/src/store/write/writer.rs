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
use crate::store::config::{duration_micros, ValidatedStoreConfig};
use crate::store::index::{DiskPos, StoreIndex};
use crate::store::segment::sidx::kind_to_raw;
use crate::store::segment::{self, Active, FramePayloadRef, Segment};
use crate::store::stats::{FrontierView, HlcPoint, WatermarkKind, WatermarkSnapshot};
use crate::store::{AppendReceipt, StoreConfig, StoreError};
use flume::{Receiver, Sender};
use parking_lot::{Condvar, Mutex, MutexGuard};
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
mod append;
mod batch;
mod fence_runtime;
mod publish;
mod runtime;

pub(crate) use self::append::AppendGuards;
use self::fence_runtime::{CommandResult, DeferredReply, FenceLedger};
pub(crate) use self::runtime::find_latest_segment_id;
use self::runtime::{writer_thread_main, writer_thread_name, WriterRuntime};

#[derive(Clone)]
pub(crate) struct WatermarkAdvanceHandle {
    state: Arc<Mutex<WatermarkState>>,
    cv: Arc<Condvar>,
    poison: Arc<AtomicBool>,
}

pub(crate) struct WatermarkGuard<'a> {
    guard: MutexGuard<'a, WatermarkState>,
    cv: &'a Condvar,
}

impl WatermarkAdvanceHandle {
    fn new(state: WatermarkState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
            cv: Arc::new(Condvar::new()),
            poison: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn lock(&self) -> WatermarkGuard<'_> {
        WatermarkGuard {
            guard: self.state.lock(),
            cv: &self.cv,
        }
    }

    pub(crate) fn mark_writer_crashed(&self) {
        self.poison.store(true, Ordering::Release);
        self.cv.notify_all();
    }

    pub(crate) fn wait_for_durable(
        &self,
        point: HlcPoint,
        timeout: Duration,
    ) -> Result<(), StoreError> {
        self.wait_for_watermark(WatermarkKind::Durable, point, timeout)
    }

    pub(crate) fn wait_for_applied(
        &self,
        point: HlcPoint,
        timeout: Duration,
    ) -> Result<(), StoreError> {
        self.wait_for_watermark(WatermarkKind::Applied, point, timeout)
    }

    pub(crate) fn wait_for_visible(
        &self,
        point: HlcPoint,
        timeout: Duration,
    ) -> Result<(), StoreError> {
        self.wait_for_watermark(WatermarkKind::Visible, point, timeout)
    }

    fn wait_for_watermark(
        &self,
        watermark: WatermarkKind,
        point: HlcPoint,
        timeout: Duration,
    ) -> Result<(), StoreError> {
        let started = Instant::now();
        let mut guard = self.state.lock();
        loop {
            if self.poison.load(Ordering::Acquire) {
                return Err(StoreError::WriterCrashed);
            }
            if watermark.current(guard.snapshot()) >= point {
                tracing::trace!(
                    target: "batpak::frontier_wait",
                    ?watermark,
                    target = ?point,
                    waited_us = duration_micros(started.elapsed()),
                    "frontier wait satisfied",
                );
                return Ok(());
            }

            let elapsed = started.elapsed();
            if elapsed >= timeout {
                tracing::trace!(
                    target: "batpak::frontier_wait",
                    ?watermark,
                    target = ?point,
                    waited_us = duration_micros(elapsed),
                    timed_out = true,
                    "frontier wait timed out",
                );
                return Err(StoreError::WaitTimeout {
                    watermark,
                    target: point,
                    waited_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                });
            }
            let remaining = timeout.saturating_sub(elapsed);
            if remaining.is_zero() {
                tracing::trace!(
                    target: "batpak::frontier_wait",
                    ?watermark,
                    target = ?point,
                    waited_us = duration_micros(elapsed),
                    timed_out = true,
                    "frontier wait timed out",
                );
                return Err(StoreError::WaitTimeout {
                    watermark,
                    target: point,
                    waited_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                });
            }

            let _ = self.cv.wait_for(&mut guard, remaining);
        }
    }

    #[cfg(any(test, feature = "dangerous-test-hooks"))]
    pub(crate) fn dangerous_notify_all(&self) {
        self.cv.notify_all();
    }
}

impl Deref for WatermarkGuard<'_> {
    type Target = WatermarkState;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl DerefMut for WatermarkGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

impl Drop for WatermarkGuard<'_> {
    fn drop(&mut self) {
        self.cv.notify_all();
    }
}

/// Internal mutable frontier state. All snapshots take this single mutex once.
pub(crate) struct WatermarkState {
    accepted_hlc: HlcPoint,
    written_hlc: HlcPoint,
    durable_hlc: HlcPoint,
    visible_hlc: HlcPoint,
    applied_hlc: HlcPoint,
    emitted_hlc: HlcPoint,
    pending_write_start: Option<Instant>,
}

impl Default for WatermarkState {
    fn default() -> Self {
        Self {
            accepted_hlc: HlcPoint::ORIGIN,
            written_hlc: HlcPoint::ORIGIN,
            durable_hlc: HlcPoint::ORIGIN,
            visible_hlc: HlcPoint::ORIGIN,
            applied_hlc: HlcPoint::ORIGIN,
            emitted_hlc: HlcPoint::ORIGIN,
            pending_write_start: None,
        }
    }
}

impl WatermarkState {
    pub(crate) fn handle() -> WatermarkAdvanceHandle {
        WatermarkAdvanceHandle::new(Self::default())
    }

    pub(crate) fn bootstrap_handle(point: HlcPoint) -> WatermarkAdvanceHandle {
        WatermarkAdvanceHandle::new(Self::for_bootstrap(point))
    }

    pub(crate) fn for_bootstrap(point: HlcPoint) -> Self {
        Self {
            accepted_hlc: point,
            written_hlc: point,
            durable_hlc: point,
            visible_hlc: point,
            applied_hlc: point,
            emitted_hlc: point,
            pending_write_start: None,
        }
    }

    pub(crate) fn reset_to_bootstrap(&mut self, point: HlcPoint) {
        *self = Self::for_bootstrap(point);
    }

    pub(crate) fn advance_accepted(&mut self, point: HlcPoint) {
        if point > self.accepted_hlc {
            self.accepted_hlc = point;
            if self.pending_write_start.is_none() {
                self.pending_write_start = Some(Instant::now());
            }
        }
    }

    pub(crate) fn advance_written(&mut self, point: HlcPoint) {
        self.written_hlc = self.written_hlc.max(point);
    }

    pub(crate) fn advance_durable(&mut self, point: HlcPoint) {
        self.durable_hlc = self.durable_hlc.max(point);
        if self.durable_hlc == self.accepted_hlc {
            self.pending_write_start = None;
        }
    }

    pub(crate) fn advance_durable_to_accepted(&mut self) {
        self.advance_durable(self.accepted_hlc);
    }

    pub(crate) fn advance_visible(&mut self, point: HlcPoint) {
        self.visible_hlc = self.visible_hlc.max(point);
    }

    pub(crate) fn advance_emitted(&mut self, point: HlcPoint) {
        self.emitted_hlc = self.emitted_hlc.max(point);
    }

    pub(crate) fn advance_visible_and_emitted(&mut self, point: HlcPoint) {
        self.advance_visible(point);
        self.advance_emitted(point);
    }

    pub(crate) fn set_applied(&mut self, point: HlcPoint) {
        self.applied_hlc = point;
    }

    pub(crate) fn snapshot(&self) -> WatermarkSnapshot {
        WatermarkSnapshot {
            accepted_hlc: self.accepted_hlc,
            written_hlc: self.written_hlc,
            durable_hlc: self.durable_hlc,
            visible_hlc: self.visible_hlc,
            applied_hlc: self.applied_hlc,
            emitted_hlc: self.emitted_hlc,
            oldest_pending_write_age_ms: self
                .pending_write_start
                .map(|start| u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)),
        }
    }

    pub(crate) fn snapshot_view(&self) -> FrontierView {
        debug_assert!(
            self.accepted_hlc >= self.written_hlc,
            "accepted must be >= written: {:?} vs {:?}",
            self.accepted_hlc,
            self.written_hlc
        );
        debug_assert!(
            self.written_hlc >= self.durable_hlc,
            "written must be >= durable: {:?} vs {:?}",
            self.written_hlc,
            self.durable_hlc
        );
        debug_assert!(
            self.accepted_hlc >= self.visible_hlc,
            "accepted must be >= visible: {:?} vs {:?}",
            self.accepted_hlc,
            self.visible_hlc
        );
        debug_assert!(
            self.visible_hlc >= self.applied_hlc,
            "visible must be >= applied: {:?} vs {:?}",
            self.visible_hlc,
            self.applied_hlc
        );
        debug_assert!(
            self.emitted_hlc >= self.visible_hlc,
            "emitted must be >= visible: {:?} vs {:?}",
            self.emitted_hlc,
            self.visible_hlc
        );

        FrontierView {
            accepted_hlc: self.accepted_hlc,
            written_hlc: self.written_hlc,
            durable_hlc: self.durable_hlc,
            visible_hlc: self.visible_hlc,
            applied_hlc: self.applied_hlc,
            emitted_hlc: self.emitted_hlc,
            visible_minus_durable_seq: (self.visible_hlc.global_sequence as i64)
                - (self.durable_hlc.global_sequence as i64),
            oldest_pending_write_age_ms: self
                .pending_write_start
                .map(|start| u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)),
        }
    }
}

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
    thread: Option<std::thread::JoinHandle<()>>,
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
        let watermark_handle = WatermarkState::handle();
        let cfg = Arc::clone(config);
        let validated = Arc::clone(runtime);
        let idx = Arc::clone(index);
        let rdr = Arc::clone(reader);
        let watermark_for_thread = watermark_handle.clone();

        let mut builder = std::thread::Builder::new().name(writer_thread_name(&config.data_dir));
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
                        watermark_handle: &watermark_for_thread,
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
            watermark_handle: WatermarkState::handle(),
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
            .is_some_and(std::thread::JoinHandle::is_finished)
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
    use super::{checked_next_clock, WatermarkState};
    use crate::store::stats::HlcPoint;
    use crate::store::StoreError;
    use std::sync::mpsc;
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

        state.advance_accepted(point);
        state.advance_durable(point);
        assert_eq!(
            state.snapshot().oldest_pending_write_age_ms,
            None,
            "PROPERTY: durability to accepted clears pending write age"
        );

        state.advance_accepted(point);
        assert_eq!(
            state.snapshot().oldest_pending_write_age_ms,
            None,
            "PROPERTY: duplicate accepted advance must not reopen pending write age"
        );
    }

    #[test]
    fn dangerous_notify_all_wakes_condvar_waiters() {
        let handle = WatermarkState::handle();
        let waiter_handle = handle.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();

        let waiter = std::thread::Builder::new()
            .name("watermark-dangerous-notify-proof".to_string())
            .spawn(move || {
                let mut guard = waiter_handle.state.lock();
                ready_tx.send(()).expect("signal waiter readiness");
                let wait_result = waiter_handle
                    .cv
                    .wait_for(&mut guard, Duration::from_secs(2));
                done_tx
                    .send(wait_result.timed_out())
                    .expect("signal waiter outcome");
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
        // Write SIDX footer before sealing. append_frames_from_segment now
        // strips SIDX data via detect_sidx_boundary, so this is safe.
        if let Err(e) = self.active_segment.write_sidx_footer(&self.sidx_collector) {
            tracing::warn!("SIDX footer write failed (non-fatal): {e}");
        }
        self.sidx_collector = crate::store::segment::sidx::SidxEntryCollector::new();
        self.active_segment.sync_with_mode(&self.config.sync.mode)?;
        self.watermark_handle.lock().advance_durable_to_accepted();
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
