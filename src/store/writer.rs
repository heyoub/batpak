// Intentional impossible-feature guard: exponential backoff belongs in the product supervisor, not the library.
// exponential-backoff is not a declared feature — suppress cfg warning for this guard
#[allow(unexpected_cfgs)]
#[cfg(feature = "exponential-backoff")]
compile_error!(
    "Red flag: only Once and Bounded restart policies. \
     Exponential backoff belongs in the product's supervisor, not here. \
     See: SPEC.md ## RED FLAGS."
);

use crate::coordinate::{Coordinate, DagPosition};
use crate::event::{Event, EventHeader, EventKind, HashChain};
use crate::store::contracts::{BatchAppendItem, CausationRef};
use crate::store::index::{DiskPos, IndexEntry, StoreIndex};
use crate::store::segment::{self, Active, FramePayloadRef, Segment};
use crate::store::{AppendReceipt, BatchStage, StoreConfig, StoreError};
use flume::{Receiver, Sender, TrySendError};
use parking_lot::Mutex;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info, trace};

/// Entity name for batch system markers (BEGIN/COMMIT). Not user-visible.
const BATCH_MARKER_ENTITY: &str = "_batch";
/// Scope name for batch system markers (BEGIN/COMMIT). Not user-visible.
const BATCH_MARKER_SCOPE: &str = "_system";

/// WriterCommand: messages sent to the background writer thread via flume.
/// All respond channels: flume::Sender — sync send from writer, async recv from caller.
/// [SPEC:src/store/writer.rs]
pub(crate) enum WriterCommand {
    Append {
        coord: Coordinate,
        event: Box<Event<Vec<u8>>>, // pre-serialized payload as msgpack bytes
        kind: EventKind,
        guards: AppendGuards,
        respond: Sender<Result<AppendReceipt, StoreError>>,
    },
    AppendBatch {
        items: Vec<BatchAppendItem>,
        respond: Sender<Result<Vec<AppendReceipt>, StoreError>>,
    },
    Sync {
        respond: Sender<Result<(), StoreError>>,
    },
    Shutdown {
        respond: Sender<Result<(), StoreError>>,
    },
    /// Test-only: trigger a panic in the writer thread to exercise restart_policy.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    PanicForTest {
        respond: Sender<Result<(), StoreError>>,
    },
}

/// WriterHandle: owned by Store. Communicates with the background thread.
pub(crate) struct WriterHandle {
    pub tx: Sender<WriterCommand>,
    pub subscribers: Arc<SubscriberList>,
    _thread: Option<std::thread::JoinHandle<()>>,
}

/// SubscriberList: push-based notification fanout via flume channels.
/// [SPEC:src/store/writer.rs — try_send pattern]
pub(crate) struct SubscriberList {
    senders: Mutex<Vec<Sender<Notification>>>,
}

/// Notification: lightweight event summary pushed to subscribers.
/// Must derive Clone (used in try_send broadcast loop).
/// [SPEC:src/store/writer.rs — Notification struct]
#[derive(Clone, Debug)]
pub struct Notification {
    /// Unique ID of the event that was appended.
    pub event_id: u128,
    /// Correlation ID linking this event to a causal chain.
    pub correlation_id: u128,
    /// ID of the event that caused this one; `None` for root-cause events.
    pub causation_id: Option<u128>,
    /// Entity and scope coordinates for the event.
    pub coord: Coordinate,
    /// Event kind (type discriminant).
    pub kind: EventKind,
    /// Global sequence number assigned to this event at commit time.
    pub sequence: u64,
}

/// RestartPolicy: how the writer recovers from panics.
/// [SPEC:src/store/writer.rs — RestartPolicy]
/// EXACTLY two variants. Adding a third violates the RED FLAGS.
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

impl SubscriberList {
    pub(crate) fn new() -> Self {
        Self {
            senders: Mutex::new(Vec::new()),
        }
    }

    /// Subscribe: create a new bounded channel, store the sender, return the receiver.
    pub(crate) fn subscribe(&self, capacity: usize) -> Receiver<Notification> {
        let (tx, rx) = flume::bounded(capacity);
        self.senders.lock().push(tx);
        rx
    }

    /// Broadcast: try_send to all, retain on Ok or Full, prune on Disconnected.
    /// NEVER use blocking send() — one slow subscriber must not block the writer.
    /// [DEP:flume::Sender::try_send] → Result<(), TrySendError<T>>
    /// [DEP:flume::TrySendError::Full] / [DEP:flume::TrySendError::Disconnected]
    pub(crate) fn broadcast(&self, notif: &Notification) {
        let mut guard = self.senders.lock();
        guard.retain(|tx| match tx.try_send(notif.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        });
    }
}

impl WriterHandle {
    /// Spawn the background writer thread.
    /// [SPEC:src/store/writer.rs — "batpak-writer-{hash}" thread]
    pub(crate) fn spawn(
        config: &Arc<StoreConfig>,
        index: &Arc<StoreIndex>,
        subscribers: &Arc<SubscriberList>,
        reader: &Arc<crate::store::reader::Reader>,
    ) -> Result<Self, StoreError> {
        // Fallible init — propagate errors to Store::open() caller
        std::fs::create_dir_all(&config.data_dir).map_err(StoreError::Io)?;
        let initial_segment_id = find_latest_segment_id(&config.data_dir).unwrap_or(0) + 1;
        let initial_segment = Segment::<Active>::create(&config.data_dir, initial_segment_id)?;

        let (tx, rx) = flume::bounded::<WriterCommand>(config.writer.channel_capacity);
        let subs = Arc::clone(subscribers);
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
                    &rx,
                    &cfg,
                    &idx,
                    &subs,
                    &rdr,
                    initial_segment,
                    initial_segment_id,
                );
            })
            .map_err(StoreError::Io)?;

        Ok(Self {
            tx,
            subscribers: Arc::clone(subscribers),
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
            _thread: None,
        }
    }

    // NOTE: No send_append() method here. Store::append() and Store::append_reaction()
    // in store/mod.rs create one-shot flume channels and send WriterCommands directly
    // via self.writer.tx.send(). This avoids an unnecessary abstraction layer.
    // WriterHandle.tx is pub(crate) for direct access. [SPEC:INVARIANTS item 4]
}

/// Writer's mutable runtime state, grouped to reduce handle_append param count.
struct WriterState<'a> {
    index: &'a StoreIndex,
    active_segment: &'a mut Segment<Active>,
    segment_id: &'a mut u64,
    config: &'a StoreConfig,
    subscribers: &'a SubscriberList,
    /// Reader handle — updated on segment rotation so mmap dispatch is correct.
    reader: Arc<crate::store::reader::Reader>,
    /// Accumulates SIDX entries for the current active segment.
    /// Flushed as a footer on segment rotation and shutdown.
    sidx_collector: crate::store::sidx::SidxEntryCollector,
}

/// Writer thread entry point with panic recovery and restart logic.
/// Wraps writer_loop() in catch_unwind, implementing RestartPolicy.
/// The rx (command receiver) survives across restarts because it lives
/// outside catch_unwind. Segments are re-created on restart since the
/// previous one is dropped during unwind.
/// [SPEC:src/store/writer.rs — RestartPolicy enforcement]
fn writer_thread_main(
    rx: &Receiver<WriterCommand>,
    config: &StoreConfig,
    index: &StoreIndex,
    subscribers: &SubscriberList,
    reader: &Arc<crate::store::reader::Reader>,
    initial_segment: Segment<Active>,
    initial_segment_id: u64,
) {
    let mut segment = initial_segment;
    let mut seg_id = initial_segment_id;
    let mut restarts: u32 = 0;
    let mut window_start = Instant::now();

    loop {
        let rdr = Arc::clone(reader);
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            writer_loop(rx, config, index, subscribers, rdr, segment, seg_id);
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

                let budget_ok = match &config.writer.restart_policy {
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
                        config.writer.restart_policy
                    );
                    return;
                }

                tracing::warn!(
                    "writer panic — restarting ({restarts}/{max}). Panic: {panic_msg}",
                    max = match &config.writer.restart_policy {
                        RestartPolicy::Once => 1,
                        RestartPolicy::Bounded { max_restarts, .. } => *max_restarts,
                    }
                );

                // Re-create segment: the previous one was dropped during unwind.
                seg_id = find_latest_segment_id(&config.data_dir).unwrap_or(seg_id) + 1;
                segment = match Segment::<Active>::create(&config.data_dir, seg_id) {
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
    rx: &Receiver<WriterCommand>,
    config: &StoreConfig,
    index: &StoreIndex,
    subscribers: &SubscriberList,
    reader: Arc<crate::store::reader::Reader>,
    mut active_segment: Segment<Active>,
    mut segment_id: u64,
) {
    let mut events_since_sync: u32 = 0;

    let mut state = WriterState {
        index,
        active_segment: &mut active_segment,
        segment_id: &mut segment_id,
        config,
        subscribers,
        reader,
        sidx_collector: crate::store::sidx::SidxEntryCollector::new(),
    };

    // Main loop: recv commands, dispatch.
    for cmd in rx.iter() {
        match cmd {
            WriterCommand::Append {
                coord,
                event,
                kind,
                guards,
                respond,
            } => {
                // Process first command in batch.
                let result = state.handle_append(&coord, *event, kind, &guards);
                let _ = respond.send(result);
                events_since_sync += 1;

                // Group commit: drain additional pending Append commands before syncing.
                // group_commit_max_batch == 0 means unbounded drain (drain all pending).
                // group_commit_max_batch == 1 means no draining (backward compat, per-event).
                // group_commit_max_batch > 1 means drain up to (batch - 1) more.
                let extra_budget = if config.batch.group_commit_max_batch == 0 {
                    u32::MAX
                } else if config.batch.group_commit_max_batch == 1 {
                    0u32
                } else {
                    config.batch.group_commit_max_batch.saturating_sub(1)
                };
                let mut drained = 0u32;
                while drained < extra_budget {
                    match rx.try_recv() {
                        Ok(WriterCommand::Append {
                            coord: c2,
                            event: ev2,
                            kind: k2,
                            guards: g2,
                            respond: r2,
                        }) => {
                            let res2 = state.handle_append(&c2, *ev2, k2, &g2);
                            let _ = r2.send(res2);
                            events_since_sync += 1;
                            drained += 1;
                        }
                        Ok(WriterCommand::AppendBatch { items, respond: r }) => {
                            // Batches are atomic — drain them as a single unit.
                            let res = state.handle_append_batch(&items);
                            let _ = r.send(res);
                            events_since_sync += 1;
                            drained += 1;
                        }
                        Ok(WriterCommand::Sync { respond: r }) => {
                            // Sync mid-batch: honor immediately, stop draining.
                            let sr = state.active_segment.sync_with_mode(&config.sync.mode);
                            let _ = r.send(sr);
                            events_since_sync = 0;
                            break;
                        }
                        Ok(WriterCommand::Shutdown { respond: r }) => {
                            // Shutdown mid-batch: sync current batch, then exit.
                            // Propagate sync errors honestly — lifecycle honesty invariant.
                            let shutdown_result = if events_since_sync > 0 {
                                let sr = state.active_segment.sync_with_mode(&config.sync.mode);
                                if let Err(ref e) = sr {
                                    tracing::error!("group commit pre-shutdown sync: {e}");
                                }
                                sr
                            } else {
                                Ok(())
                            };
                            let _ = r.send(shutdown_result);
                            return;
                        }
                        #[cfg(feature = "test-support")]
                        Ok(WriterCommand::PanicForTest { respond: r }) => {
                            // Don't panic mid-drain — acknowledge and stop draining.
                            // The test should send PanicForTest as a standalone command
                            // (through the main loop) not mid-batch. Panicking mid-drain
                            // would leave the batch partially synced with some callers
                            // never receiving their receipt.
                            let _ = r.send(Ok(()));
                            break;
                        }
                        Err(_) => break, // channel empty — batch complete
                    }
                }

                // Single fsync for the entire batch.
                if events_since_sync >= config.sync.every_n_events {
                    if let Err(e) = state.active_segment.sync_with_mode(&config.sync.mode) {
                        tracing::error!("periodic sync failed: {e}");
                    }
                    events_since_sync = 0;
                }
            }
            WriterCommand::AppendBatch { items, respond } => {
                let result = state.handle_append_batch(&items);
                let _ = respond.send(result);
                events_since_sync += 1; // Batch counts as one sync event

                // Sync after batch if needed.
                if events_since_sync >= config.sync.every_n_events {
                    if let Err(e) = state.active_segment.sync_with_mode(&config.sync.mode) {
                        tracing::error!("post-batch sync failed: {e}");
                    }
                    events_since_sync = 0;
                }
            }
            WriterCommand::Sync { respond } => {
                let result = state.active_segment.sync_with_mode(&config.sync.mode);
                let _ = respond.send(result);
                events_since_sync = 0;
            }
            WriterCommand::Shutdown { respond } => {
                // Drain up to shutdown_drain_limit queued commands.
                // [SPEC:src/store/writer.rs — Shutdown drain semantics]
                let mut drained = 0;
                while drained < config.writer.shutdown_drain_limit {
                    match rx.try_recv() {
                        Ok(WriterCommand::Append {
                            coord,
                            event,
                            kind,
                            guards,
                            respond: r,
                        }) => {
                            let result = state.handle_append(&coord, *event, kind, &guards);
                            let _ = r.send(result);
                            drained += 1;
                        }
                        Ok(WriterCommand::Shutdown { respond: r }) => {
                            let _ = r.send(Ok(()));
                        }
                        Ok(WriterCommand::Sync { respond: r }) => {
                            let _ = r.send(state.active_segment.sync_with_mode(&config.sync.mode));
                        }
                        Ok(WriterCommand::AppendBatch { items, respond: r }) => {
                            // Drain batches during shutdown.
                            let res = state.handle_append_batch(&items);
                            let _ = r.send(res);
                            drained += 1;
                        }
                        // test-only: discard PanicForTest during shutdown drain
                        #[cfg(feature = "test-support")]
                        Ok(WriterCommand::PanicForTest { respond: r }) => {
                            let _ = r.send(Ok(())); // discard during drain
                        }
                        Err(_) => break, // channel empty
                    }
                }
                // Write SIDX footer on active segment before shutdown sync.
                if let Err(e) = state
                    .active_segment
                    .write_sidx_footer(&state.sidx_collector)
                {
                    tracing::warn!("shutdown SIDX footer write failed (non-fatal): {e}");
                }
                let sync_result = state.active_segment.sync_with_mode(&config.sync.mode);
                if let Err(ref e) = sync_result {
                    tracing::error!("shutdown sync failed: {e}");
                }
                let _ = respond.send(sync_result);
                return; // exit writer loop
            }
            // test-only: intentional panic to exercise restart_policy
            #[cfg(feature = "test-support")]
            // intentional: this panic IS the test - it exercises catch_unwind in writer_thread_main
            #[allow(clippy::panic)]
            // intentional: this panic IS the test — it exercises catch_unwind in writer_thread_main
            WriterCommand::PanicForTest { respond } => {
                // Acknowledge receipt before panicking so the test knows the command was processed.
                let _ = respond.send(Ok(()));
                panic!("PanicForTest: intentional writer panic for restart_policy testing");
            }
        }
    }
}

/// Options and guards for an append operation, passed through the channel.
/// CAS + idempotency checks execute on the single writer thread, so there
/// is no producer/producer race to guard against.
pub(crate) struct AppendGuards {
    pub correlation_id: u128,
    pub causation_id: Option<u128>,
    pub expected_sequence: Option<u32>,
    pub idempotency_key: Option<u128>,
}

/// Pre-computed per-item batch state shared between the precompute, write,
/// stage, and broadcast phases of `handle_append_batch`.
struct BatchItemComputed {
    global_seq: u64,
    clock: u32,
    prev_hash: [u8; 32],
    event_id: u128,
    causation_id: Option<u128>,
}

impl WriterState<'_> {
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

    /// Pre-compute per-item global sequences, clocks, prev_hashes, event_ids,
    /// and causation. Builds intra-batch entity clock + hash chains so multiple
    /// items per entity get incrementing clocks before the index is updated.
    fn precompute_batch_items(
        &self,
        items: &[BatchAppendItem],
        first_seq: u64,
    ) -> Result<Vec<BatchItemComputed>, StoreError> {
        let mut computed: Vec<BatchItemComputed> = Vec::with_capacity(items.len());
        let mut entity_prev_hashes: std::collections::HashMap<Arc<str>, [u8; 32]> =
            std::collections::HashMap::new();
        let mut entity_batch_clocks: std::collections::HashMap<Arc<str>, u32> =
            std::collections::HashMap::new();

        for (idx, item) in items.iter().enumerate() {
            let entity: Arc<str> = Arc::from(item.coord.entity());

            let prev_hash = if let Some(&hash) = entity_prev_hashes.get(&entity) {
                hash
            } else {
                self.index
                    .get_latest(&entity)
                    .map(|e| e.hash_chain.event_hash)
                    .unwrap_or([0u8; 32])
            };

            let clock = if let Some(&last_clock) = entity_batch_clocks.get(&entity) {
                last_clock + 1
            } else {
                self.index
                    .get_latest(&entity)
                    .map(|e| e.clock + 1)
                    .unwrap_or(0)
            };
            entity_batch_clocks.insert(Arc::clone(&entity), clock);

            let event_id = uuid::Uuid::now_v7().as_u128();

            let causation_id = match item.causation {
                CausationRef::None => item.options.causation_id,
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
                    Some(computed[prior_idx].event_id)
                }
            };

            entity_prev_hashes.insert(entity, [0u8; 32]);

            let global_seq = first_seq + idx as u64;
            computed.push(BatchItemComputed {
                global_seq,
                clock,
                prev_hash,
                event_id,
                causation_id,
            });
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
    /// [SPEC:src/store/writer.rs — handle_append]
    fn handle_append(
        &mut self,
        coord: &Coordinate,
        mut event: Event<Vec<u8>>,
        kind: EventKind,
        guards: &AppendGuards,
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
        let position = DagPosition::child_at(clock, now_ms, 0);
        event.header.position = position;
        event.header.event_kind = kind;
        event.header.correlation_id = correlation_id;
        event.header.causation_id = causation_id;

        // STEP 5: Compute blake3 hash, set hash chain (or skip if feature off).
        // [SPEC:INVARIANTS item 5 — blake3 only]
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

        // STEP 6: Serialize to MessagePack + CRC32 frame.
        // [SPEC:WIRE FORMAT DECISIONS — rmp_serde::to_vec_named() ALWAYS]
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
        let entity_id = self.index.interner.intern(entity);
        let scope_id = self.index.interner.intern(scope);
        let entry = IndexEntry {
            event_id: event.header.event_id,
            correlation_id,
            causation_id,
            coord: coord.clone(),
            entity_id,
            scope_id,
            kind,
            wall_ms: now_ms,
            clock,
            hash_chain: event.hash_chain.clone().unwrap_or_default(),
            disk_pos,
            global_sequence: global_seq,
        };
        self.index.insert(entry);

        // Publish: make this entry visible to concurrent readers.
        // Explicit boundary: the entry has global_sequence == global_seq,
        // so visible_sequence must advance to global_seq + 1.
        self.index.publish(global_seq + 1);

        // Record SIDX entry for the segment footer (written on rotation/shutdown).
        let hash_chain_ref = event.hash_chain.as_ref();
        let sidx_entry = crate::store::sidx::SidxEntry {
            event_id: event.header.event_id,
            entity_idx: 0, // filled by collector.record()
            scope_idx: 0,  // filled by collector.record()
            kind: crate::store::sidx::kind_to_raw(kind),
            wall_ms: now_ms,
            clock,
            prev_hash: hash_chain_ref.map(|h| h.prev_hash).unwrap_or([0u8; 32]),
            event_hash: hash_chain_ref.map(|h| h.event_hash).unwrap_or([0u8; 32]),
            frame_offset: offset,
            #[allow(clippy::cast_possible_truncation)] // frame.len() checked by checked_payload_len
            frame_length: frame.len() as u32,
            global_sequence: global_seq,
            correlation_id,
            causation_id: causation_id.unwrap_or(0),
        };
        self.sidx_collector.record(sidx_entry, entity, scope);

        debug!(event_id = %event.header.event_id, clock = clock, "append committed");

        // STEP 10: Broadcast notification to subscribers.
        self.subscribers.broadcast(&Notification {
            event_id: event.header.event_id,
            correlation_id,
            causation_id,
            coord: coord.clone(),
            kind,
            sequence: global_seq,
        });

        Ok(AppendReceipt {
            event_id: event.header.event_id,
            sequence: global_seq,
            disk_pos,
        })
    }

    /// Batch append protocol: atomic multi-event commit with SYSTEM_BATCH_BEGIN envelope.
    /// [SPEC:src/store/writer.rs — handle_append_batch]
    fn handle_append_batch(
        &mut self,
        items: &[BatchAppendItem],
    ) -> Result<Vec<AppendReceipt>, StoreError> {
        // STEPs 1-2: Validate size, bytes, and reject CAS.
        self.validate_batch(items)?;

        // STEP 3: Preflight idempotency. Full replay returns cached receipts;
        // partial replay errors out; clean batch proceeds.
        if let Some(cached) = self.preflight_batch_idempotency(items)? {
            return Ok(cached);
        }

        // STEPs 4-5: (no locks needed) — single writer thread owns all
        // index mutation. The previous per-entity Mutex was a leftover from
        // a multi-writer design and added overhead with no concurrency benefit.

        // STEP 6: Generate batch_id and reserve global sequences.
        let batch_id = self.index.global_sequence();
        let first_seq = self.index.reserve_sequences(items.len() as u64);

        // FAULT INJECTION: Batch start
        #[cfg(feature = "test-support")]
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::BatchStart {
                batch_id,
                item_count: items.len(),
            },
            &self.config.fault_injector,
        )?;

        // STEP 7: Pre-compute per-item global sequences, clocks, prev_hashes,
        // event_ids, and intra-batch causation chains.
        let computed = self.precompute_batch_items(items, first_seq)?;

        // STEP 8: Write SYSTEM_BATCH_BEGIN marker. Stores batch count in payload_size.
        // batch_max_size validation guarantees items.len() <= u32::MAX.
        #[allow(clippy::cast_possible_truncation)]
        let batch_count = items.len() as u32;
        let marker_offset =
            self.write_batch_marker_frame(batch_id, EventKind::SYSTEM_BATCH_BEGIN, batch_count, 0)?;
        trace!(batch_id, offset = marker_offset, "batch marker written");

        // FAULT INJECTION: After BEGIN marker written
        #[cfg(feature = "test-support")]
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::BatchBeginWritten {
                batch_id,
                item_count: items.len(),
            },
            &self.config.fault_injector,
        )?;

        // STEP 9: Write all event frames. Returns offsets, receipts, and the
        // populated entity_prev_hashes map (used by stage step for IndexEntry).
        let (frame_offsets, receipts, entity_prev_hashes) =
            self.write_batch_event_frames(items, &computed, batch_id)?;

        // FAULT INJECTION: All batch items complete
        #[cfg(feature = "test-support")]
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::BatchItemsComplete {
                batch_id,
                item_count: items.len(),
            },
            &self.config.fault_injector,
        )?;

        // STEP 10: Write SYSTEM_BATCH_COMMIT marker (two-phase commit).
        let _commit_offset = self.write_batch_marker_frame(
            batch_id,
            EventKind::SYSTEM_BATCH_COMMIT,
            0,
            items.len() - 1,
        )?;
        trace!(batch_id, "batch commit marker written");

        // FAULT INJECTION: After COMMIT written, before fsync
        #[cfg(feature = "test-support")]
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::BatchCommitWritten { batch_id },
            &self.config.fault_injector,
        )?;

        // STEP 11: Sync to disk (atomic durability point).
        // If this fails, the batch may be partially on disk but without the
        // commit marker. Recovery will discard incomplete batches.

        // FAULT INJECTION: During fsync
        #[cfg(feature = "test-support")]
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::BatchFsync { batch_id },
            &self.config.fault_injector,
        )?;

        self.active_segment
            .sync_with_mode(&self.config.sync.mode)
            .map_err(|e| StoreError::BatchFailed {
                item_index: items.len() - 1,
                stage: BatchStage::Syncing,
                source: Box::new(e),
            })?;

        // STEP 12: Build index entries from the precomputed data + frame offsets.
        let staged_entries = self.stage_batch_index_entries(
            items,
            &computed,
            &frame_offsets,
            &receipts,
            &entity_prev_hashes,
        )?;

        // FAULT INJECTION: Before atomic publish to index
        #[cfg(feature = "test-support")]
        crate::store::fault::maybe_inject(
            crate::store::fault::InjectionPoint::BatchPrePublish {
                batch_id,
                item_count: items.len(),
            },
            &self.config.fault_injector,
        )?;

        // STEP 13: Insert all entries into the in-memory index, then publish
        // atomically. Entries occupy [first_seq, first_seq + items.len()).
        self.index.insert_batch(staged_entries);
        #[allow(clippy::cast_possible_truncation)] // items.len() bounded by batch_max_size (u32)
        self.index.publish(first_seq + items.len() as u64);

        // STEP 14: Broadcast notifications. A subscriber that reacts by calling
        // query/get will now see the full batch (publish happened first).
        self.broadcast_batch_notifications(items, &computed)?;

        debug!(batch_id, count = items.len(), "batch committed");
        Ok(receipts)
    }

    /// STEP 9: Write all event frames for the batch. Returns frame offsets,
    /// per-item receipts, and the populated entity_prev_hashes map (which the
    /// stage step needs to fill in `IndexEntry::hash_chain.event_hash`).
    #[allow(clippy::type_complexity)] // tuple is the natural shape; refactoring would obscure flow
    fn write_batch_event_frames(
        &mut self,
        items: &[BatchAppendItem],
        computed: &[BatchItemComputed],
        batch_id: u64,
    ) -> Result<
        (
            Vec<u64>,
            Vec<AppendReceipt>,
            std::collections::HashMap<Arc<str>, [u8; 32]>,
        ),
        StoreError,
    > {
        let mut frame_offsets: Vec<u64> = Vec::with_capacity(items.len());
        let mut receipts: Vec<AppendReceipt> = Vec::with_capacity(items.len());
        let mut entity_prev_hashes: std::collections::HashMap<Arc<str>, [u8; 32]> =
            std::collections::HashMap::new();

        for (idx, item) in items.iter().enumerate() {
            let c = &computed[idx];
            let global_seq = c.global_seq;
            let clock = c.clock;
            let prev_hash = c.prev_hash;
            let event_id = c.event_id;
            let causation_id = c.causation_id;

            // Build event header.
            let now_us = self.config.now_us();
            #[allow(clippy::cast_sign_loss)] // timestamp_us is always positive (from SystemTime)
            let now_ms = (now_us / 1000) as u64;
            let position = DagPosition::child_at(clock, now_ms, 0);
            let correlation_id = item.options.correlation_id.unwrap_or(event_id);

            let header = EventHeader::new(
                event_id,
                correlation_id,
                causation_id,
                now_us,
                position,
                // Payload sizes bounded by batch_max_bytes validation
                #[allow(clippy::cast_possible_truncation)]
                {
                    item.payload_bytes.len() as u32
                },
                item.kind,
            );

            // Build event.
            let mut event = Event::new(header, item.payload_bytes.clone());

            // Compute hash.
            #[cfg(feature = "blake3")]
            let event_hash = crate::event::hash::compute_hash(&event.payload);
            #[cfg(not(feature = "blake3"))]
            let event_hash = [0u8; 32];
            event.hash_chain = Some(HashChain {
                prev_hash,
                event_hash,
            });
            event.header.content_hash = event_hash;

            // Update entity_prev_hashes for intra-batch chain (and stage step).
            let entity: Arc<str> = Arc::from(item.coord.entity());
            entity_prev_hashes.insert(entity, event_hash);

            // Encode frame.
            let frame_payload = FramePayloadRef {
                event: &event,
                entity: item.coord.entity(),
                scope: item.coord.scope(),
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
            frame_offsets.push(offset);

            // Build receipt (index update happens after all writes succeed).
            let disk_pos = DiskPos {
                segment_id: *self.segment_id,
                offset,
                #[allow(clippy::cast_possible_truncation)] // frame size bounded by segment_max_bytes
                length: frame.len() as u32,
            };
            receipts.push(AppendReceipt {
                event_id,
                sequence: global_seq,
                disk_pos,
            });

            // FAULT INJECTION: After each batch item written
            #[cfg(feature = "test-support")]
            crate::store::fault::maybe_inject(
                crate::store::fault::InjectionPoint::BatchItemWritten {
                    batch_id,
                    item_index: idx,
                    total_items: items.len(),
                },
                &self.config.fault_injector,
            )?;
        }
        // Suppress unused warning when test-support is disabled.
        let _ = batch_id;

        Ok((frame_offsets, receipts, entity_prev_hashes))
    }

    /// STEP 12: Build IndexEntry vec from precomputed data + write outputs.
    /// Also records SIDX entries for the segment footer.
    fn stage_batch_index_entries(
        &mut self,
        items: &[BatchAppendItem],
        computed: &[BatchItemComputed],
        frame_offsets: &[u64],
        receipts: &[AppendReceipt],
        entity_prev_hashes: &std::collections::HashMap<Arc<str>, [u8; 32]>,
    ) -> Result<Vec<IndexEntry>, StoreError> {
        let mut staged_entries: Vec<IndexEntry> = Vec::with_capacity(items.len());
        for (idx, item) in items.iter().enumerate() {
            let c = &computed[idx];
            let global_seq = c.global_seq;
            let clock = c.clock;
            let prev_hash = c.prev_hash;
            let event_id = c.event_id;
            let causation_id = c.causation_id;
            let offset = frame_offsets[idx];

            let entity: Arc<str> = Arc::from(item.coord.entity());
            let scope: Arc<str> = Arc::from(item.coord.scope());

            // Use disk_pos captured at write time — if rotation happened mid-batch,
            // earlier items live on a prior segment.
            let disk_pos = receipts[idx].disk_pos;
            let coord =
                Coordinate::new(entity.as_ref(), scope.as_ref()).map_err(StoreError::Coordinate)?;
            let entity_id = self.index.interner.intern(entity.as_ref());
            let scope_id = self.index.interner.intern(scope.as_ref());

            let event_hash = entity_prev_hashes
                .get(&entity)
                .copied()
                .unwrap_or([0u8; 32]);

            // Use the injectable clock for wall_ms (matches single-append path).
            #[allow(clippy::cast_sign_loss)] // timestamp_us is always positive
            let wall_ms = (self.config.now_us() / 1000) as u64;

            let entry = IndexEntry {
                event_id,
                correlation_id: item.options.correlation_id.unwrap_or(event_id),
                causation_id,
                coord: coord.clone(),
                entity_id,
                scope_id,
                kind: item.kind,
                wall_ms,
                clock,
                hash_chain: HashChain {
                    prev_hash,
                    event_hash,
                },
                disk_pos,
                global_sequence: global_seq,
            };

            staged_entries.push(entry);

            // Record SIDX entry.
            let sidx_entry = crate::store::sidx::SidxEntry {
                event_id,
                entity_idx: 0,
                scope_idx: 0,
                kind: crate::store::sidx::kind_to_raw(item.kind),
                wall_ms,
                clock,
                prev_hash,
                event_hash,
                frame_offset: offset,
                frame_length: receipts[idx].disk_pos.length,
                global_sequence: global_seq,
                correlation_id: item.options.correlation_id.unwrap_or(event_id),
                causation_id: causation_id.unwrap_or(0),
            };
            self.sidx_collector
                .record(sidx_entry, entity.as_ref(), scope.as_ref());
        }
        Ok(staged_entries)
    }

    /// STEP 14: Broadcast a Notification for each item in the committed batch.
    fn broadcast_batch_notifications(
        &self,
        items: &[BatchAppendItem],
        computed: &[BatchItemComputed],
    ) -> Result<(), StoreError> {
        for (idx, item) in items.iter().enumerate() {
            let c = &computed[idx];
            let global_seq = c.global_seq;
            let event_id = c.event_id;
            let causation_id = c.causation_id;
            let entity: Arc<str> = Arc::from(item.coord.entity());
            let scope: Arc<str> = Arc::from(item.coord.scope());
            let coord =
                Coordinate::new(entity.as_ref(), scope.as_ref()).map_err(StoreError::Coordinate)?;

            self.subscribers.broadcast(&Notification {
                event_id,
                correlation_id: item.options.correlation_id.unwrap_or(event_id),
                causation_id,
                coord,
                kind: item.kind,
                sequence: global_seq,
            });
        }
        Ok(())
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
