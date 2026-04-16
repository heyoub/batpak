// Intentional impossible-feature guard: exponential backoff belongs in the product supervisor, not the library.
// exponential-backoff is not a declared feature — suppress cfg warning for this guard
#[allow(unexpected_cfgs)]
#[cfg(feature = "exponential-backoff")]
compile_error!(
    "Red flag: only Once and Bounded restart policies. \
     Exponential backoff belongs in the product's supervisor, not here. \
     See: REFERENCE.md."
);

use crate::coordinate::{Coordinate, DagPosition};
use crate::event::{Event, EventHeader, EventKind, HashChain};
use crate::store::contracts::{BatchAppendItem, CausationRef};
pub use crate::store::fanout::Notification;
use crate::store::fanout::{CommittedEventEnvelope, ReactorSubscriberList, SubscriberList};
use crate::store::index::{DiskPos, IndexEntry, StoreIndex};
use crate::store::segment::{self, Active, FramePayloadRef, Segment};
use crate::store::staging::{
    PreparedBatch, PreparedBatchInternedIds, PreparedBatchItem, StagedCommitMeta,
    StagedCommitTiming, StagedCommittedEvent,
};
use crate::store::{AppendReceipt, BatchStage, StoreConfig, StoreError};
use flume::{Receiver, Sender};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info, trace};

/// Entity name for batch system markers (BEGIN/COMMIT). Not user-visible.
const BATCH_MARKER_ENTITY: &str = "_batch";
/// Scope name for batch system markers (BEGIN/COMMIT). Not user-visible.
const BATCH_MARKER_SCOPE: &str = "_system";

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
        index: &Arc<StoreIndex>,
        subscribers: &Arc<SubscriberList>,
        reactor_subscribers: &Arc<ReactorSubscriberList>,
        reader: &Arc<crate::store::reader::Reader>,
    ) -> Result<Self, StoreError> {
        // Fallible init — propagate errors to Store::open() caller
        std::fs::create_dir_all(&config.data_dir).map_err(StoreError::Io)?;
        let initial_segment_id = find_latest_segment_id(&config.data_dir).unwrap_or(0) + 1;
        let initial_segment = Segment::<Active>::create(&config.data_dir, initial_segment_id)?;

        let (tx, rx) = flume::bounded::<WriterCommand>(config.writer.channel_capacity);
        let subs = Arc::clone(subscribers);
        let reactor_subs = Arc::clone(reactor_subscribers);
        let cfg = Arc::clone(config);
        let idx = Arc::clone(index);
        let rdr = Arc::clone(reader);

        let thread_name = format!("batpak-writer-{:08x}", {
            let mut h: u64 = 0xcbf29ce484222325; // FNV-1a basis
            for b in config.data_dir.to_string_lossy().bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x100000001b3); // FNV-1a prime
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
    reader: Arc<crate::store::reader::Reader>,
    /// Accumulates SIDX entries for the current active segment.
    /// Flushed as a footer on segment rotation and shutdown.
    sidx_collector: crate::store::sidx::SidxEntryCollector,
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
        notifications: Vec<Notification>,
        envelopes: Vec<CommittedEventEnvelope>,
    ) {
        self.notifications.extend(notifications);
        self.envelopes.extend(envelopes);
    }

    fn push_response(&mut self, response: PendingFenceResponse) {
        self.responses.push(response);
    }

    fn complete_ok(
        self,
        subscribers: &SubscriberList,
        reactor_subscribers: &ReactorSubscriberList,
    ) {
        for notification in &self.notifications {
            subscribers.broadcast(notification);
        }
        for envelope in &self.envelopes {
            reactor_subscribers.broadcast(envelope);
        }
        for response in self.responses {
            response.complete_ok();
        }
    }

    fn complete_cancelled(self) {
        for response in self.responses {
            response.complete_cancelled();
        }
    }
}

struct SidxRecord {
    entry: crate::store::sidx::SidxEntry,
    coord: Coordinate,
}

impl SidxRecord {
    fn record(self, collector: &mut crate::store::sidx::SidxEntryCollector) {
        collector.record(self.entry, self.coord.entity(), self.coord.scope());
    }
}

struct CommitArtifacts {
    index_entry: IndexEntry,
    sidx_record: SidxRecord,
    notification: Notification,
    envelope: Option<CommittedEventEnvelope>,
}

struct BatchCommitArtifacts {
    entries: Vec<IndexEntry>,
    sidx_records: Vec<SidxRecord>,
    notifications: Vec<Notification>,
    envelopes: Vec<CommittedEventEnvelope>,
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
    enter_shutdown_drain: bool,
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
            enter_shutdown_drain: false,
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

    fn enter_shutdown_drain(mut self, deferred_reply: DeferredReply) -> Self {
        self.exit_writer = true;
        self.enter_shutdown_drain = true;
        self.deferred_reply = deferred_reply;
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
    index: &'a StoreIndex,
    subscribers: &'a SubscriberList,
    reactor_subscribers: &'a ReactorSubscriberList,
    reader: &'a Arc<crate::store::reader::Reader>,
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
                        if let Err(error) = crate::store::visibility_ranges::write_cancelled_ranges(
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
        sidx_collector: crate::store::sidx::SidxEntryCollector::new(),
        fence_ledger: None,
    };

    for cmd in runtime.rx.iter() {
        let result = state.execute_command(WriterLoopPhase::Main, cmd);
        if result.enter_shutdown_drain {
            let DeferredReply::Shutdown { respond } = result.deferred_reply else {
                unreachable!("shutdown drain must carry a shutdown reply sender");
            };
            let shutdown_result = drain_shutdown_queue(
                &mut state,
                runtime.rx,
                runtime.config.writer.shutdown_drain_limit,
            );
            let _ = respond.send(shutdown_result);
            return;
        }

        let outcome = settle_command_result(&mut state, &mut events_since_sync, result);
        if outcome.exit_writer {
            return;
        }

        if outcome.enter_group_commit_drain {
            let extra_budget =
                group_commit_drain_budget(runtime.config.batch.group_commit_max_batch);
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
        exit_writer: result.exit_writer && !result.enter_shutdown_drain,
        sync_event_delta: result.sync_event_delta,
        enter_group_commit_drain: result.enter_group_commit_drain,
    }
}

fn group_commit_drain_budget(group_commit_max_batch: u32) -> u32 {
    if group_commit_max_batch == 0 {
        u32::MAX
    } else if group_commit_max_batch == 1 {
        0
    } else {
        group_commit_max_batch.saturating_sub(1)
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
                WriterLoopPhase::Main => CommandResult::immediate(0)
                    .enter_shutdown_drain(DeferredReply::Shutdown { respond }),
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

        self.index
            .finish_visibility_fence(token, fence.publish_up_to)?;
        fence.complete_ok(self.subscribers, self.reactor_subscribers);
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
        crate::store::visibility_ranges::write_cancelled_ranges(
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
    /// Callers needing batch-specific error context should wrap with
    /// `.map_err(|e| StoreError::BatchFailed { ... })`.
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
        self.sidx_collector = crate::store::sidx::SidxEntryCollector::new();
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

    /// STEPs 1-2: Validate batch size, total bytes, and reject CAS in batches.
    fn validate_batch(&self, items: &[BatchAppendItem]) -> Result<(), StoreError> {
        if items.len() > self.config.batch.max_size as usize {
            return Err(StoreError::BatchFailed {
                item_index: 0,
                stage: BatchStage::Validation,
                source: Box::new(StoreError::Configuration(format!(
                    "batch size {} exceeds max {}",
                    items.len(),
                    self.config.batch.max_size
                ))),
            });
        }
        let total_bytes: usize = items.iter().map(|i| i.payload_bytes.len()).sum();
        if total_bytes > self.config.batch.max_bytes as usize {
            return Err(StoreError::BatchFailed {
                item_index: 0,
                stage: BatchStage::Validation,
                source: Box::new(StoreError::Configuration(format!(
                    "batch bytes {} exceeds max {}",
                    total_bytes, self.config.batch.max_bytes
                ))),
            });
        }
        for (idx, item) in items.iter().enumerate() {
            if item.options.expected_sequence.is_some() {
                return Err(StoreError::BatchFailed {
                    item_index: idx,
                    stage: BatchStage::Validation,
                    source: Box::new(StoreError::Configuration(
                        "CAS not supported in batch append (v1)".into(),
                    )),
                });
            }
        }
        Ok(())
    }

    /// STEP 3: Preflight idempotency check.
    /// Returns `Ok(Some(receipts))` if every item is already committed (full replay),
    /// `Ok(None)` to proceed with the batch write, or `Err` for partial-replay ambiguity.
    fn preflight_batch_idempotency(
        &self,
        items: &[BatchAppendItem],
    ) -> Result<Option<Vec<AppendReceipt>>, StoreError> {
        let mut cached_receipts: Vec<Option<AppendReceipt>> = vec![None; items.len()];
        let mut cached_count = 0usize;
        for (idx, item) in items.iter().enumerate() {
            if let Some(key) = item.options.idempotency_key {
                if let Some(entry) = self.index.get_by_id(key) {
                    cached_receipts[idx] = Some(AppendReceipt {
                        event_id: entry.event_id,
                        sequence: entry.global_sequence,
                        disk_pos: entry.disk_pos,
                    });
                    cached_count += 1;
                }
            }
        }
        if cached_count == items.len() {
            return Ok(Some(
                cached_receipts
                    .into_iter()
                    .map(|r| r.expect("full replay: all cached_receipts verified as Some"))
                    .collect(),
            ));
        }
        if cached_count > 0 {
            return Err(StoreError::BatchFailed {
                item_index: cached_receipts
                    .iter()
                    .position(|r| r.is_none())
                    .unwrap_or(0),
                stage: BatchStage::Validation,
                source: Box::new(StoreError::Configuration(
                    "partial batch replay: some items already committed, some not".into(),
                )),
            });
        }
        Ok(None)
    }

    /// Pre-compute per-item global sequences, clocks, wall_ms, prev_hashes,
    /// event_hashes, event_ids, and causation. Builds intra-batch per-entity
    /// chains for clock, wall_ms, and hash so multi-item same-entity batches
    /// produce a continuous sequence and a continuous hash chain on disk.
    ///
    /// **Single timestamp invariant.** A single `now_us()` is captured at the
    /// top and reused for every item's `wall_us`. The corresponding `wall_ms`
    /// is `max(now_ms, entity_last_ms)` per entity to mirror the single-append
    /// monotonicity guard at `handle_append::STEP 4` — without this clamp, a
    /// regressing clock (mocked test clock, NTP slew) could reorder
    /// `stream()` results within a batch.
    ///
    /// **Eager hash invariant.** `event_hash` is computed here (not deferred
    /// to the frame-write phase) so the next same-entity item can read it as
    /// its `prev_hash`. Without this, the on-disk frame chain and the
    /// in-memory IndexEntry chain diverge. `StagedCommittedEvent` now carries
    /// the per-item committed shape end-to-end so there is no scratch map or
    /// reconstruction step left to drift.
    fn precompute_batch_items(
        &self,
        prepared: &PreparedBatch,
        first_seq: u64,
    ) -> Result<Vec<StagedCommittedEvent>, StoreError> {
        let mut computed: Vec<StagedCommittedEvent> = Vec::with_capacity(prepared.len());
        let mut entity_prev_hashes: std::collections::HashMap<Arc<str>, [u8; 32]> =
            std::collections::HashMap::new();
        let mut entity_batch_clocks: std::collections::HashMap<Arc<str>, u32> =
            std::collections::HashMap::new();
        let mut entity_batch_wall_ms: std::collections::HashMap<Arc<str>, u64> =
            std::collections::HashMap::new();

        // Single timestamp for the entire batch (see Single timestamp invariant
        // above). Header `timestamp_us` and the IndexEntry `wall_ms` are both
        // derived from this one capture.
        let now_us = self.config.now_us();
        #[allow(clippy::cast_sign_loss)] // timestamp_us is always positive (from SystemTime)
        let now_ms = (now_us / 1000) as u64;

        for (idx, item) in prepared.items().iter().enumerate() {
            let entity = Arc::clone(item.entity_arc());

            // prev_hash: previous batch item if same entity, else the index's
            // latest entry for the entity, else genesis [0; 32].
            let prev_hash = if let Some(&hash) = entity_prev_hashes.get(&entity) {
                hash
            } else {
                self.index
                    .get_latest(&entity)
                    .map(|e| e.hash_chain.event_hash)
                    .unwrap_or([0u8; 32])
            };

            // clock: monotonic per entity, +1 from prior batch item or index.
            let clock = if let Some(&last_clock) = entity_batch_clocks.get(&entity) {
                last_clock + 1
            } else {
                self.index
                    .get_latest(&entity)
                    .map(|e| e.clock + 1)
                    .unwrap_or(0)
            };
            entity_batch_clocks.insert(Arc::clone(&entity), clock);

            // wall_ms: monotonic per entity. The clamp source must consider
            // BOTH the index AND prior batch items on the same entity — a
            // batch-internal clock regression would otherwise reorder
            // BTreeMap entries in `StoreIndex::streams`.
            let last_ms = entity_batch_wall_ms
                .get(&entity)
                .copied()
                .unwrap_or_else(|| {
                    self.index
                        .get_latest(&entity)
                        .map(|e| e.wall_ms)
                        .unwrap_or(0)
                });
            let wall_ms = now_ms.max(last_ms);
            entity_batch_wall_ms.insert(Arc::clone(&entity), wall_ms);

            let event_id = uuid::Uuid::now_v7().as_u128();

            let causation_id = match item.causation() {
                CausationRef::None => item.options().causation_id,
                CausationRef::Absolute(id) => Some(id),
                CausationRef::PriorItem(prior_idx) => {
                    if prior_idx >= idx {
                        return Err(StoreError::BatchFailed {
                            item_index: idx,
                            stage: BatchStage::Validation,
                            source: Box::new(StoreError::Configuration(
                                "PriorItem causation must reference earlier batch item".into(),
                            )),
                        });
                    }
                    Some(computed[prior_idx].event_id())
                }
            };

            // Compute event_hash NOW (eager hash invariant — see fn doc).
            #[cfg(feature = "blake3")]
            let event_hash = crate::event::hash::compute_hash(item.payload_bytes());
            #[cfg(not(feature = "blake3"))]
            let event_hash = [0u8; 32];

            // Populate the prev_hash source for the next same-entity item
            // with the ACTUAL hash (was a `[0u8; 32]` placeholder before,
            // which broke the chain).
            entity_prev_hashes.insert(entity, event_hash);

            let global_seq = first_seq + idx as u64;
            let meta = StagedCommitMeta::new(
                event_id,
                item.options().correlation_id.unwrap_or(event_id),
                causation_id,
                item.kind(),
                global_seq,
            );
            let position_hint = item.options().position_hint.unwrap_or_default();
            let timing = StagedCommitTiming::new(
                now_us,
                wall_ms,
                clock,
                position_hint.lane,
                position_hint.depth,
            );
            computed.push(StagedCommittedEvent::new(
                item.coord(),
                meta,
                timing,
                HashChain {
                    prev_hash,
                    event_hash,
                },
            ));
        }
        Ok(computed)
    }

    /// Encode and write a batch marker frame (BEGIN or COMMIT).
    /// Handles segment rotation before the write. Returns the frame offset.
    fn write_batch_marker_frame(
        &mut self,
        batch_id: u64,
        kind: EventKind,
        payload_size: u32,
        item_index_for_error: usize,
    ) -> Result<u64, StoreError> {
        let now_us = self.config.now_us();
        let header = EventHeader::new(
            batch_id as u128,
            batch_id as u128,
            None,
            now_us,
            #[allow(clippy::cast_sign_loss)] // timestamp_us is always positive (from SystemTime)
            DagPosition::child_at(0, (now_us / 1000) as u64, 0),
            payload_size,
            kind,
        );
        let event = Event::new(header, Vec::<u8>::new());
        let payload = crate::store::segment::FramePayloadRef {
            event: &event,
            entity: BATCH_MARKER_ENTITY,
            scope: BATCH_MARKER_SCOPE,
        };
        let frame = segment::frame_encode(&payload).map_err(|e| StoreError::BatchFailed {
            item_index: item_index_for_error,
            stage: BatchStage::Encoding,
            source: Box::new(e),
        })?;

        self.maybe_rotate_segment()
            .map_err(|e| StoreError::BatchFailed {
                item_index: item_index_for_error,
                stage: BatchStage::Syncing,
                source: Box::new(e),
            })?;

        let offset =
            self.active_segment
                .write_frame(&frame)
                .map_err(|e| StoreError::BatchFailed {
                    item_index: item_index_for_error,
                    stage: BatchStage::Writing,
                    source: Box::new(e),
                })?;
        Ok(offset)
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
        #[allow(clippy::cast_sign_loss)] // timestamp_us is always positive (from SystemTime)
        let raw_ms = (event.header.timestamp_us / 1000) as u64;
        let last_ms = latest.as_ref().map(|entry| entry.wall_ms).unwrap_or(0);
        let now_ms = raw_ms.max(last_ms);
        let position =
            DagPosition::with_hlc(now_ms, 0, guards.dag_depth, guards.dag_lane, clock);
        event.header.position = position;
        event.header.event_kind = kind;
        event.header.correlation_id = correlation_id;
        event.header.causation_id = causation_id;

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
            #[allow(clippy::cast_possible_truncation)] // checked_payload_len already verified < u32::MAX
            length: frame.len() as u32,
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
            coord,
            meta,
            timing,
            HashChain {
                prev_hash,
                event_hash,
            },
        );
        let artifact = self.materialize_commit_artifacts(
            &staged,
            disk_pos,
            &event.payload,
            event.header.flags,
        );
        self.index.insert(artifact.index_entry);
        artifact.sidx_record.record(&mut self.sidx_collector);

        debug!(event_id = %event.header.event_id, clock = clock, "append committed");

        // STEP 10: Broadcast notification to subscribers.
        if let Some(fence) = fence {
            self.index
                .note_visibility_fence_progress(
                    fence.token,
                    global_seq,
                    global_seq.saturating_add(1),
                )
                .expect("active fence token verified before fenced append");
            fence.record_publish_up_to(global_seq.saturating_add(1));
            fence.extend_artifacts(
                vec![artifact.notification],
                artifact.envelope.into_iter().collect(),
            );
        } else {
            // Publish: make this entry visible to concurrent readers.
            // Explicit boundary: the entry has global_sequence == global_seq,
            // so visible_sequence must advance to global_seq + 1.
            self.index.publish(global_seq + 1);
            self.broadcast_commit_artifacts(
                vec![artifact.notification],
                artifact.envelope.into_iter().collect(),
            );
        }

        Ok(AppendReceipt {
            event_id: event.header.event_id,
            sequence: global_seq,
            disk_pos,
        })
    }

    /// Batch append protocol: atomic multi-event commit with SYSTEM_BATCH_BEGIN envelope.
    fn handle_append_batch(
        &mut self,
        items: Vec<BatchAppendItem>,
        fence: Option<&mut FenceLedger>,
    ) -> Result<Vec<AppendReceipt>, StoreError> {
        // STEPs 1-2: Validate size, bytes, and reject CAS.
        self.validate_batch(&items)?;

        // STEP 3: Preflight idempotency. Full replay returns cached receipts;
        // partial replay errors out; clean batch proceeds.
        if let Some(cached) = self.preflight_batch_idempotency(&items)? {
            return Ok(cached);
        }

        let prepared = PreparedBatch::from_items(items)?;
        self.handle_prepared_batch(&prepared, fence)
    }

    fn handle_prepared_batch(
        &mut self,
        prepared: &PreparedBatch,
        fence: Option<&mut FenceLedger>,
    ) -> Result<Vec<AppendReceipt>, StoreError> {
        debug_assert_eq!(
            prepared.total_bytes(),
            prepared
                .items()
                .iter()
                .map(|item| item.payload_bytes().len())
                .sum::<usize>()
        );

        // STEPs 4-5: (no locks needed) — single writer thread owns all
        // index mutation. The previous per-entity Mutex was a leftover from
        // a multi-writer design and added overhead with no concurrency benefit.

        // STEP 6: Generate batch_id and reserve global sequences.
        let batch_id = self.index.global_sequence();
        let first_seq = self.index.reserve_sequences(prepared.len() as u64);

        // FAULT INJECTION: Batch start
        #[cfg(feature = "dangerous-test-hooks")]
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::BatchStart {
                batch_id,
                item_count: prepared.len(),
            },
            &self.config.fault_injector,
        )?;

        // STEP 7: Pre-compute per-item global sequences, clocks, prev_hashes,
        // event_ids, and intra-batch causation chains.
        let computed = self.precompute_batch_items(prepared, first_seq)?;

        // STEP 8: Write SYSTEM_BATCH_BEGIN marker. Stores batch count in payload_size.
        // batch_max_size validation guarantees items.len() <= u32::MAX.
        #[allow(clippy::cast_possible_truncation)]
        let batch_count = prepared.len() as u32;
        let marker_offset =
            self.write_batch_marker_frame(batch_id, EventKind::SYSTEM_BATCH_BEGIN, batch_count, 0)?;
        trace!(batch_id, offset = marker_offset, "batch marker written");

        // FAULT INJECTION: After BEGIN marker written
        #[cfg(feature = "dangerous-test-hooks")]
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::BatchBeginWritten {
                batch_id,
                item_count: prepared.len(),
            },
            &self.config.fault_injector,
        )?;

        // STEP 9: Write all event frames. Returns receipts;
        // every per-item value the stage step needs (`prev_hash`,
        // `event_hash`, `wall_ms`, `clock`) was already locked in by
        // `precompute_batch_items`.
        let receipts = self.write_batch_event_frames(prepared, &computed, batch_id)?;

        // FAULT INJECTION: All batch items complete
        #[cfg(feature = "dangerous-test-hooks")]
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::BatchItemsComplete {
                batch_id,
                item_count: prepared.len(),
            },
            &self.config.fault_injector,
        )?;

        // STEP 10: Write SYSTEM_BATCH_COMMIT marker (two-phase commit).
        let _commit_offset = self.write_batch_marker_frame(
            batch_id,
            EventKind::SYSTEM_BATCH_COMMIT,
            0,
            prepared.len() - 1,
        )?;
        trace!(batch_id, "batch commit marker written");

        // FAULT INJECTION: After COMMIT written, before fsync
        #[cfg(feature = "dangerous-test-hooks")]
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::BatchCommitWritten { batch_id },
            &self.config.fault_injector,
        )?;

        // STEP 11: Sync to disk (atomic durability point).
        // If this fails, the batch may be partially on disk but without the
        // commit marker. Recovery will discard incomplete batches.

        // FAULT INJECTION: During fsync
        #[cfg(feature = "dangerous-test-hooks")]
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::BatchFsync { batch_id },
            &self.config.fault_injector,
        )?;

        self.active_segment
            .sync_with_mode(&self.config.sync.mode)
            .map_err(|e| StoreError::BatchFailed {
                item_index: prepared.len() - 1,
                stage: BatchStage::Syncing,
                source: Box::new(e),
            })?;

        // STEP 12/14: Materialize all post-write projections in one pass.
        let artifacts = self.materialize_batch_commit_artifacts(prepared, &computed, &receipts);
        Self::record_sidx_records(artifacts.sidx_records, &mut self.sidx_collector);

        // FAULT INJECTION: Before atomic publish to index
        #[cfg(feature = "dangerous-test-hooks")]
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::BatchPrePublish {
                batch_id,
                item_count: prepared.len(),
            },
            &self.config.fault_injector,
        )?;

        // STEP 13: Insert all entries into the in-memory index, then publish
        // atomically. Entries occupy [first_seq, first_seq + items.len()).
        self.index.insert_batch(artifacts.entries);
        #[allow(clippy::cast_possible_truncation)] // prepared.len() bounded by batch_max_size (u32)
        let publish_up_to = first_seq + prepared.len() as u64;

        if let Some(fence) = fence {
            self.index
                .note_visibility_fence_progress(fence.token, first_seq, publish_up_to)
                .expect("active fence token verified before fenced batch append");
            fence.record_publish_up_to(publish_up_to);
            fence.extend_artifacts(artifacts.notifications, artifacts.envelopes);
        } else {
            self.index.publish(publish_up_to);
            // STEP 14: Broadcast notifications. A subscriber that reacts by calling
            // query/get will now see the full batch (publish happened first).
            self.broadcast_commit_artifacts(artifacts.notifications, artifacts.envelopes);
        }

        debug!(batch_id, count = prepared.len(), "batch committed");
        Ok(receipts)
    }

    /// STEP 9: Write all event frames for the batch. Returns per-item receipts.
    /// All per-item state (`prev_hash`, `event_hash`,
    /// `wall_us`, etc.) is taken verbatim from the precomputed staged slice —
    /// this function does NOT recompute hashes, timestamps, or chain links.
    fn write_batch_event_frames(
        &mut self,
        prepared: &PreparedBatch,
        staged: &[StagedCommittedEvent],
        batch_id: u64,
    ) -> Result<Vec<AppendReceipt>, StoreError> {
        let mut receipts: Vec<AppendReceipt> = Vec::with_capacity(prepared.len());

        for (idx, item) in prepared.items().iter().enumerate() {
            let staged = &staged[idx];

            // Build the committed frame event from the staged packet so batch
            // write, index, and broadcast all share one source of truth.
            let event = staged
                .borrowed_frame_event(item.payload_bytes())
                .map_err(|e| StoreError::BatchFailed {
                    item_index: idx,
                    stage: BatchStage::Encoding,
                    source: Box::new(e),
                })?;

            // Encode frame.
            let frame_payload = FramePayloadRef {
                event: &event,
                entity: staged.entity(),
                scope: staged.scope(),
            };
            let frame =
                segment::frame_encode(&frame_payload).map_err(|e| StoreError::BatchFailed {
                    item_index: idx,
                    stage: BatchStage::Encoding,
                    source: Box::new(e),
                })?;

            // Check segment rotation.
            self.maybe_rotate_segment()
                .map_err(|e| StoreError::BatchFailed {
                    item_index: idx,
                    stage: BatchStage::Syncing,
                    source: Box::new(e),
                })?;

            // Write frame.
            let offset =
                self.active_segment
                    .write_frame(&frame)
                    .map_err(|e| StoreError::BatchFailed {
                        item_index: idx,
                        stage: BatchStage::Writing,
                        source: Box::new(e),
                    })?;

            // Build receipt (index update happens after all writes succeed).
            let disk_pos = DiskPos {
                segment_id: *self.segment_id,
                offset,
                #[allow(clippy::cast_possible_truncation)] // frame size bounded by segment_max_bytes
                length: frame.len() as u32,
            };
            receipts.push(AppendReceipt {
                event_id: staged.event_id(),
                sequence: staged.global_sequence(),
                disk_pos,
            });

            // FAULT INJECTION: After each batch item written
            #[cfg(feature = "dangerous-test-hooks")]
            crate::store::fault::maybe_inject(
                crate::store::fault::InjectionPoint::BatchItemWritten {
                    batch_id,
                    item_index: idx,
                    total_items: prepared.len(),
                },
                &self.config.fault_injector,
            )?;
        }
        // Suppress unused warning when dangerous-test-hooks is disabled.
        let _ = batch_id;

        Ok(receipts)
    }

    fn materialize_commit_artifacts(
        &self,
        staged: &StagedCommittedEvent,
        disk_pos: DiskPos,
        payload_bytes: &[u8],
        flags: u8,
    ) -> CommitArtifacts {
        let index_entry = staged.index_entry(self.index, disk_pos);
        let notification = staged.notification();
        let envelope = staged
            .stored_event(payload_bytes, flags)
            .map(|stored| CommittedEventEnvelope {
                notification: notification.clone(),
                stored,
            })
            .ok();
        let sidx_record = SidxRecord {
            entry: staged.sidx_entry(disk_pos),
            coord: staged.coord().clone(),
        };

        CommitArtifacts {
            index_entry,
            sidx_record,
            notification,
            envelope,
        }
    }

    fn materialize_prepared_commit_artifacts(
        &self,
        prepared_item: &PreparedBatchItem,
        staged: &StagedCommittedEvent,
        disk_pos: DiskPos,
        interned_ids: &PreparedBatchInternedIds,
    ) -> CommitArtifacts {
        let index_entry = staged.index_entry_with_ids(
            disk_pos,
            interned_ids.entity_id(prepared_item),
            interned_ids.scope_id(prepared_item),
        );
        let notification = staged.notification();
        let envelope = staged
            .stored_event(prepared_item.payload_bytes(), prepared_item.options().flags)
            .map(|stored| CommittedEventEnvelope {
                notification: notification.clone(),
                stored,
            })
            .ok();
        let sidx_record = SidxRecord {
            entry: staged.sidx_entry(disk_pos),
            coord: staged.coord().clone(),
        };

        CommitArtifacts {
            index_entry,
            sidx_record,
            notification,
            envelope,
        }
    }

    /// STEP 12/14: Materialize all post-write views in one pass from the
    /// committed staged facts plus receipts. This is the product split over
    /// the same semantic source, so index/SIDX/notification/envelope derivation
    /// cannot silently drift apart.
    fn materialize_batch_commit_artifacts(
        &self,
        prepared: &PreparedBatch,
        staged: &[StagedCommittedEvent],
        receipts: &[AppendReceipt],
    ) -> BatchCommitArtifacts {
        let mut entries: Vec<IndexEntry> = Vec::with_capacity(staged.len());
        let mut sidx_records: Vec<SidxRecord> = Vec::with_capacity(staged.len());
        let mut notifications: Vec<Notification> = Vec::with_capacity(staged.len());
        let mut envelopes: Vec<CommittedEventEnvelope> = Vec::with_capacity(staged.len());
        let interned_ids = prepared.interned_ids(self.index);

        for ((item, staged), receipt) in prepared
            .items()
            .iter()
            .zip(staged.iter())
            .zip(receipts.iter())
        {
            let artifact = self.materialize_prepared_commit_artifacts(
                item,
                staged,
                receipt.disk_pos,
                &interned_ids,
            );
            entries.push(artifact.index_entry);
            sidx_records.push(artifact.sidx_record);
            notifications.push(artifact.notification);
            if let Some(envelope) = artifact.envelope {
                envelopes.push(envelope);
            }
        }

        BatchCommitArtifacts {
            entries,
            sidx_records,
            notifications,
            envelopes,
        }
    }

    fn record_sidx_records(
        records: Vec<SidxRecord>,
        collector: &mut crate::store::sidx::SidxEntryCollector,
    ) {
        for record in records {
            record.record(collector);
        }
    }

    fn broadcast_commit_artifacts(
        &self,
        notifications: Vec<Notification>,
        envelopes: Vec<CommittedEventEnvelope>,
    ) {
        for notification in notifications {
            self.subscribers.broadcast(&notification);
        }
        for envelope in envelopes {
            self.reactor_subscribers.broadcast(&envelope);
        }
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
