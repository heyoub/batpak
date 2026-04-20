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
use super::fanout::{CommittedEventEnvelope, ReactorSubscriberList, SubscriberList};
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
use tracing::{debug, info, trace};

mod batch;
mod publish;

use self::publish::CommitInternedIds;

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

enum PendingFenceResponse {
    Single {
        respond: Sender<Result<AppendReceipt, StoreError>>,
        receipt: AppendReceipt,
    },
    Batch {
        respond: Sender<Result<Vec<AppendReceipt>, StoreError>>,
        receipts: Vec<AppendReceipt>,
    },
}

impl PendingFenceResponse {
    fn complete_cancelled(self) {
        match self {
            Self::Single { respond, .. } => {
                let _ = respond.send(Err(StoreError::VisibilityFenceCancelled));
            }
            Self::Batch { respond, .. } => {
                let _ = respond.send(Err(StoreError::VisibilityFenceCancelled));
            }
        }
    }

    fn complete_ok(self) {
        match self {
            Self::Single { respond, receipt } => {
                let _ = respond.send(Ok(receipt));
            }
            Self::Batch { respond, receipts } => {
                let _ = respond.send(Ok(receipts));
            }
        }
    }
}

struct FenceLedger {
    token: u64,
    publish_up_to: Option<u64>,
    notifications: Vec<Notification>,
    envelopes: Vec<CommittedEventEnvelope>,
    responses: Vec<PendingFenceResponse>,
}

impl FenceLedger {
    fn new(token: u64) -> Self {
        Self {
            token,
            publish_up_to: None,
            notifications: Vec::new(),
            envelopes: Vec::new(),
            responses: Vec::new(),
        }
    }

    fn record_publish_up_to(&mut self, publish_up_to: u64) {
        self.publish_up_to = Some(self.publish_up_to.unwrap_or(0).max(publish_up_to));
    }

    fn extend_artifacts(
        &mut self,
        notifications: impl IntoIterator<Item = Notification>,
        envelopes: impl IntoIterator<Item = CommittedEventEnvelope>,
    ) {
        self.notifications.extend(notifications);
        self.envelopes.extend(envelopes);
    }

    fn push_response(&mut self, response: PendingFenceResponse) {
        self.responses.push(response);
    }

    fn complete_cancelled(self) {
        for response in self.responses {
            response.complete_cancelled();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WriterLoopPhase {
    Main,
    GroupCommitDrain,
    ShutdownDrain,
}

#[derive(Debug)]
enum DeferredReply {
    None,
    Sync {
        respond: Sender<Result<(), StoreError>>,
    },
    BeginVisibilityFence {
        token: u64,
        respond: Sender<Result<(), StoreError>>,
    },
    CommitVisibilityFence {
        token: u64,
        respond: Sender<Result<(), StoreError>>,
    },
    Shutdown {
        respond: Sender<Result<(), StoreError>>,
    },
}

impl DeferredReply {
    fn send(
        self,
        state: &mut WriterState<'_>,
        sync_result: Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        match self {
            Self::None => Ok(()),
            Self::Sync { respond } => {
                let _ = respond.send(sync_result);
                Ok(())
            }
            Self::BeginVisibilityFence { token, respond } => {
                let result = sync_result.and_then(|_| state.begin_visibility_fence(token));
                let _ = respond.send(result);
                Ok(())
            }
            Self::CommitVisibilityFence { token, respond } => {
                let result = sync_result.and_then(|_| state.commit_visibility_fence(token));
                let _ = respond.send(result);
                Ok(())
            }
            Self::Shutdown { respond } => {
                let _ = respond.send(sync_result);
                Ok(())
            }
        }
    }
}

#[derive(Debug)]
struct CommandResult {
    sync_event_delta: u32,
    break_after_reply: bool,
    must_sync_before_continue: bool,
    exit_writer: bool,
    deferred_reply: DeferredReply,
    /// Some = enter shutdown drain after this command; carries the reply sender directly.
    /// Replaces the previous (enter_shutdown_drain: bool, DeferredReply::Shutdown) pair,
    /// which required an unreachable!() at the call site to extract the sender.
    shutdown_drain_respond: Option<Sender<Result<(), StoreError>>>,
    enter_group_commit_drain: bool,
}

impl CommandResult {
    fn immediate(sync_event_delta: u32) -> Self {
        Self {
            sync_event_delta,
            break_after_reply: false,
            must_sync_before_continue: false,
            exit_writer: false,
            deferred_reply: DeferredReply::None,
            shutdown_drain_respond: None,
            enter_group_commit_drain: false,
        }
    }

    fn break_after_reply(mut self) -> Self {
        self.break_after_reply = true;
        self
    }

    fn break_after_reply_if(self, condition: bool) -> Self {
        if condition {
            self.break_after_reply()
        } else {
            self
        }
    }

    fn with_sync(mut self, deferred_reply: DeferredReply) -> Self {
        self.must_sync_before_continue = true;
        self.deferred_reply = deferred_reply;
        self
    }

    fn exit_writer(mut self) -> Self {
        self.exit_writer = true;
        self
    }

    fn enter_shutdown_drain(mut self, respond: Sender<Result<(), StoreError>>) -> Self {
        self.exit_writer = true;
        self.shutdown_drain_respond = Some(respond);
        self
    }

    fn enter_group_commit_drain(mut self) -> Self {
        self.enter_group_commit_drain = true;
        self
    }
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

/// Options and guards for an append operation, passed through the channel.
/// CAS + idempotency checks execute on the single writer thread, so there
/// is no producer/producer race to guard against.
pub(crate) struct AppendGuards {
    pub correlation_id: u128,
    pub causation_id: Option<u128>,
    pub expected_sequence: Option<u32>,
    pub idempotency_key: Option<u128>,
    pub dag_lane: u32,
    pub dag_depth: u32,
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

    fn auto_cancel_fence_on_shutdown(&mut self) {
        if let Some(fence) = self.fence_ledger.take() {
            tracing::warn!(
                token = fence.token,
                pending = fence.responses.len(),
                "auto-cancelling active visibility fence during shutdown"
            );
            let _ = self.index.cancel_visibility_fence(fence.token);
            if let Err(error) = self.persist_cancelled_visibility_ranges() {
                tracing::error!(
                    error = %error,
                    "failed to persist cancelled visibility ranges during shutdown"
                );
            }
            fence.complete_cancelled();
        }
    }

    fn with_matching_fence_ledger<R>(
        &mut self,
        token: u64,
        f: impl FnOnce(&mut Self, &mut FenceLedger) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        if self.fence_ledger.as_ref().map(|fence| fence.token) != Some(token) {
            return Err(StoreError::VisibilityFenceNotActive);
        }
        let mut fence = self
            .fence_ledger
            .take()
            .expect("token check guaranteed fence ledger");
        let result = f(self, &mut fence);
        self.fence_ledger = Some(fence);
        result
    }

    fn handle_fence_append_command(
        &mut self,
        token: u64,
        coord: &Coordinate,
        event: Event<Vec<u8>>,
        kind: EventKind,
        guards: &AppendGuards,
        respond: Sender<Result<AppendReceipt, StoreError>>,
    ) -> Result<(), StoreError> {
        self.with_matching_fence_ledger(token, |state, fence| {
            let receipt = state.handle_append(coord, event, kind, guards, Some(fence))?;
            fence.push_response(PendingFenceResponse::Single { respond, receipt });
            Ok(())
        })
    }

    fn handle_fence_append_batch_command(
        &mut self,
        token: u64,
        items: Vec<BatchAppendItem>,
        respond: Sender<Result<Vec<AppendReceipt>, StoreError>>,
    ) -> Result<(), StoreError> {
        self.with_matching_fence_ledger(token, |state, fence| {
            let receipts = state.handle_append_batch(items, Some(fence))?;
            fence.push_response(PendingFenceResponse::Batch { respond, receipts });
            Ok(())
        })
    }

    fn begin_visibility_fence(&mut self, token: u64) -> Result<(), StoreError> {
        if self.fence_ledger.is_some() {
            return Err(StoreError::VisibilityFenceActive);
        }
        if self.index.active_visibility_fence() != Some(token) {
            return Err(StoreError::VisibilityFenceNotActive);
        }
        self.fence_ledger = Some(FenceLedger::new(token));
        Ok(())
    }

    fn commit_visibility_fence(&mut self, token: u64) -> Result<(), StoreError> {
        let Some(fence) = self.fence_ledger.take() else {
            return Err(StoreError::VisibilityFenceNotActive);
        };
        if fence.token != token {
            self.fence_ledger = Some(fence);
            return Err(StoreError::VisibilityFenceNotActive);
        }

        // F2: publish index boundary first, then broadcast — lifted into
        // `fence_finish_then_broadcast`. Subscribers woken by the
        // broadcast must already see the events as visible: if the
        // broadcast ran before `finish_visibility_fence`, a subscriber
        // could observe a notification yet see the entry still hidden.
        //
        // Receipt-completion happens after the broadcast; the receipt
        // reply is a private caller ack, not a visibility event, so it
        // is sent after the ordered finish-then-broadcast sequence.
        let FenceLedger {
            publish_up_to,
            notifications,
            envelopes,
            responses,
            ..
        } = fence;
        self.fence_finish_then_broadcast(token, publish_up_to, notifications, envelopes)?;
        for response in responses {
            response.complete_ok();
        }
        Ok(())
    }

    fn cancel_visibility_fence(&mut self, token: u64) -> Result<(), StoreError> {
        let Some(fence) = self.fence_ledger.take() else {
            return Err(StoreError::VisibilityFenceNotActive);
        };
        if fence.token != token {
            self.fence_ledger = Some(fence);
            return Err(StoreError::VisibilityFenceNotActive);
        }

        self.index.cancel_visibility_fence(token)?;
        self.persist_cancelled_visibility_ranges()?;
        fence.complete_cancelled();
        Ok(())
    }

    fn persist_cancelled_visibility_ranges(&self) -> Result<(), StoreError> {
        crate::store::hidden_ranges::write_cancelled_ranges(
            &self.config.data_dir,
            &self.index.cancelled_visibility_ranges(),
        )
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

    /// The 10-step commit protocol.
    fn handle_append(
        &mut self,
        coord: &Coordinate,
        mut event: Event<Vec<u8>>,
        kind: EventKind,
        guards: &AppendGuards,
        fence: Option<&mut FenceLedger>,
    ) -> Result<AppendReceipt, StoreError> {
        let correlation_id = guards.correlation_id;
        let causation_id = guards.causation_id;
        let entity = coord.entity();
        let scope = coord.scope();

        // STEP 1: Read latest entry. No lock needed: this function runs on the
        // single writer thread, which is the only writer of the index. There
        // is no producer/producer race to guard against.
        let latest = self.index.get_latest(entity);

        // STEP 1a: CAS check.
        if let Some(expected) = guards.expected_sequence {
            let actual = latest.as_ref().map(|entry| entry.clock).unwrap_or(0);
            if actual != expected {
                return Err(StoreError::SequenceMismatch {
                    entity: entity.to_string(),
                    expected,
                    actual,
                });
            }
        }

        // STEP 1b: Idempotency check.
        if let Some(key) = guards.idempotency_key {
            if let Some(entry) = self.index.get_by_id(key) {
                return Ok(AppendReceipt {
                    event_id: entry.event_id,
                    sequence: entry.global_sequence,
                    disk_pos: entry.disk_pos,
                });
            }
        }

        // STEP 2: Get prev_hash from index (or [0u8;32] for genesis).
        // Clone the value out of the DashMap Ref immediately.
        let prev_hash = latest
            .as_ref()
            .map(|entry| entry.hash_chain.event_hash)
            .unwrap_or([0u8; 32]);

        // STEP 3: Compute sequence (latest.clock + 1, or 0).
        let clock = latest.as_ref().map(|entry| entry.clock + 1).unwrap_or(0);

        // STEP 4: Set event header position with HLC wall clock.
        // Ensure wall_ms is monotonically non-decreasing per entity to prevent
        // BTreeMap reordering on clock regression.
        let raw_ms = u64::try_from(event.header.timestamp_us / 1000)
            .expect("invariant: timestamp_us is always positive (from SystemTime)");
        let last_ms = latest.as_ref().map(|entry| entry.wall_ms).unwrap_or(0);
        let now_ms = raw_ms.max(last_ms);
        let position = DagPosition::with_hlc(now_ms, 0, guards.dag_depth, guards.dag_lane, clock);
        event.header.position = position;
        event.header.event_kind = kind;
        event.header.correlation_id = correlation_id;
        event.header.causation_id = causation_id.filter(|&id| id != 0);

        // STEP 5: Compute the event hash and set the hash chain.
        // `blake3` is the only supported hash algorithm for committed events.
        #[cfg(feature = "blake3")]
        let event_hash = crate::event::hash::compute_hash(&event.payload);
        #[cfg(not(feature = "blake3"))]
        let event_hash = [0u8; 32];

        event.hash_chain = Some(HashChain {
            prev_hash,
            event_hash,
        });
        // Set content_hash on header for projection cache auto-invalidation.
        event.header.content_hash = event_hash;

        // STEP 6: Serialize to named MessagePack + CRC32 frame.
        let frame_payload = FramePayloadRef {
            event: &event,
            entity,
            scope,
        };
        let frame = segment::frame_encode(&frame_payload)?;

        // STEP 7: Check segment rotation.
        if self.maybe_rotate_segment()? {
            info!(segment_id = *self.segment_id, "segment rotated");
        }

        // STEP 8: Write frame to segment file.
        let offset = self.active_segment.write_frame(&frame)?;
        trace!(offset = offset, len = frame.len(), "frame written");

        // STEP 9: Update index.
        let global_seq = self.index.global_sequence();
        let disk_pos = DiskPos {
            segment_id: *self.segment_id,
            offset,
            length: u32::try_from(frame.len())
                .expect("invariant: frame length bounded by segment_max_bytes, well within u32"),
        };
        let meta = StagedCommitMeta::new(
            event.header.event_id,
            correlation_id,
            causation_id,
            kind,
            global_seq,
        );
        let timing = StagedCommitTiming::new(
            event.header.timestamp_us,
            now_ms,
            clock,
            guards.dag_lane,
            guards.dag_depth,
        );
        let staged = StagedCommittedEvent::new(
            coord.clone(),
            meta,
            timing,
            HashChain {
                prev_hash,
                event_hash,
            },
        );
        let emit_envelope = self.reactor_subscribers.has_subscribers();
        let entity_id = self.index.interner.intern(coord.entity());
        let scope_id = self.index.interner.intern(coord.scope());
        let committed = self.materialize_commit_artifacts(
            &staged,
            disk_pos,
            CommitInternedIds {
                entity_id,
                scope_id,
            },
            &event.payload,
            event.header.flags,
            emit_envelope,
        );
        self.sidx_collector.record(
            committed.sidx_entry,
            committed.index_entry.coord.entity(),
            committed.index_entry.coord.scope(),
        );
        self.index.insert(committed.index_entry);

        debug!(event_id = %event.header.event_id, clock = clock, "append committed");

        // STEP 10: Broadcast notification to subscribers.
        if let Some(fence) = fence {
            // Record the in-memory ledger progress FIRST, then mark the
            // on-disk fence-progress range. Ordering rationale: if this
            // sequence is interrupted between the two calls (panic, crash),
            // the on-disk ledger must never claim progress beyond what the
            // in-memory fence knows about — the opposite ordering would
            // leave the disk ledger ahead of the fence's publish_up_to, and
            // a subsequent restart-time recovery could publish events that
            // the fence intended to cancel.
            fence.record_publish_up_to(global_seq.saturating_add(1));
            self.index
                .note_visibility_fence_progress(
                    fence.token,
                    global_seq,
                    global_seq.saturating_add(1),
                )
                .expect("active fence token verified before fenced append");
            fence.extend_artifacts([committed.notification], committed.envelope);
        } else {
            // F2: publish-then-broadcast ordering is load-bearing; lifted
            // into a named helper so the ordering cannot be silently
            // swapped at the call-site. See
            // `publish_then_broadcast_unfenced` for the contract.
            self.publish_then_broadcast_unfenced(
                global_seq + 1,
                [committed.notification],
                committed.envelope,
            );
        }

        Ok(AppendReceipt {
            event_id: event.header.event_id,
            sequence: global_seq,
            disk_pos,
        })
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
