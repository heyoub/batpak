use crate::coordinate::Region;
use crate::store::cold_start::persist_with_parent_fsync;
use crate::store::delivery::observation::CheckpointId;
use crate::store::index::{IndexEntry, StoreIndex};
use crate::store::{RestartPolicy, Store, StoreError};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;

/// Durable cursor checkpoint.
///
/// Written atomically to `{data_dir}/cursors/{id}.ckpt` via tempfile +
/// parent-directory fsync after every successful batch so a cursor with a
/// `checkpoint_id` resumes from the durable position after a process
/// restart. `process_boot_ns` reserves space for monotonic-clock
/// cross-checks without wiring any clock dependency today — set to
/// `None` when that wiring is not required.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CursorCheckpoint {
    /// Global sequence of the last delivered event.
    ///
    /// When `started` is true, a subsequent poll returns events strictly
    /// after this position.
    pub position: u64,
    /// Whether the cursor has delivered at least one event. A fresh
    /// cursor starts at position 0 with `started = false` so that
    /// global_sequence 0 (a legitimate value) is not skipped.
    pub started: bool,
    /// Process-boot monotonic clock value at the time of the last save.
    /// Reserved for monotonic-clock integration; `None` when not wired.
    pub process_boot_ns: Option<u64>,
    /// Stable identity of the region this checkpoint belongs to.
    ///
    /// Old checkpoints may deserialize with `None`; startup treats that as
    /// a mismatch and fails closed instead of silently resuming an
    /// unscoped checkpoint against an arbitrary region.
    #[serde(default)]
    pub region_identity: Option<String>,
}

impl CursorCheckpoint {
    fn from_checkpoint(position: u64, started: bool, region_identity: String) -> Self {
        Self {
            position,
            started,
            process_boot_ns: None,
            region_identity: Some(region_identity),
        }
    }
}

fn cursor_checkpoint_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("cursors")
}

fn cursor_checkpoint_path(data_dir: &Path, id: &str) -> PathBuf {
    cursor_checkpoint_dir(data_dir).join(format!("{id}.ckpt"))
}

/// Cursor: pull-based event consumption with ordered replay.
///
/// [`Store::cursor_guaranteed`] exposes the process-local surface of this
/// type: ordered, at-least-once pull replay from the in-memory index.
/// Checkpoint-backed cursors are the internal mechanism used by
/// `cursor_worker` and typed reactors when `checkpoint_id` is set.
/// Resume succeeds only if the saved checkpoint loads and its region
/// identity matches the cursor's exact [`Region`].
///
/// Delivery semantics:
///
/// * **Without `checkpoint_id`:** at-least-once within process lifetime.
///   Events delivered before a process crash are re-delivered on next
///   start because the in-memory cursor position is lost.
/// * **With `checkpoint_id`:** at-least-once across restarts. A crash
///   between delivery and checkpoint save causes the latest batch to be
///   re-delivered — callers must treat their handler as idempotent.
///   The checkpoint is also bound to the cursor's exact [`Region`];
///   reusing the same id for a different logical consumer fails closed.
///
/// Neither mode is exactly-once: that guarantee requires coordinating
/// the cursor checkpoint with the downstream side-effect in a single
/// atomic write, which this type does not attempt.
pub struct Cursor {
    region: Region,
    position: u64, // tracks global_sequence — next poll starts after this
    started: bool, // false until first event consumed (global_sequence 0 is valid)
    index: Arc<StoreIndex>,
    gap_buffer: Option<GapBuffer>,
    /// Optional durable-checkpoint id. When set, the cursor was
    /// constructed with a data directory and a checkpoint identifier so
    /// its position can be persisted via `save_checkpoint`.
    durable: Option<CursorDurableBinding>,
}

#[derive(Clone, Debug)]
struct CursorDurableBinding {
    data_dir: PathBuf,
    id: CheckpointId,
}

/// Configuration for in-memory write-to-deliver gap observation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CursorGapConfig {
    /// When `true`, the cursor retains write-to-deliver gaps in memory.
    pub enabled: bool,
    /// Maximum number of retained gap observations before the oldest
    /// entry is dropped.
    pub buffer_capacity: usize,
}

/// A substrate-detectable write-to-deliver gap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GapObservation {
    /// First sequence the cursor expected to deliver next.
    pub expected_sequence: u64,
    /// First visible delivered sequence after the skipped interval.
    pub delivered_sequence: u64,
    /// Half-open cancelled visibility ranges `[start, end)` intersecting the
    /// skipped interval.
    pub cancelled_ranges: Vec<(u64, u64)>,
}

#[derive(Clone, Debug)]
struct GapBuffer {
    capacity: usize,
    observations: VecDeque<GapObservation>,
}

impl GapBuffer {
    fn new(capacity: usize) -> Option<Self> {
        (capacity > 0).then(|| Self {
            capacity,
            observations: VecDeque::with_capacity(capacity),
        })
    }

    fn push(&mut self, observation: GapObservation) {
        if self.observations.len() == self.capacity {
            self.observations.pop_front();
        }
        self.observations.push_back(observation);
    }

    fn take_all(&mut self) -> Vec<GapObservation> {
        self.observations.drain(..).collect()
    }
}

impl Cursor {
    pub(crate) fn new(region: Region, index: Arc<StoreIndex>) -> Self {
        Self {
            region,
            position: 0,
            started: false,
            index,
            gap_buffer: None,
            durable: None,
        }
    }

    fn new_bound_checkpoint(
        region: Region,
        index: Arc<StoreIndex>,
        data_dir: &Path,
        id: &str,
    ) -> Self {
        Self {
            region,
            position: 0,
            started: false,
            index,
            gap_buffer: None,
            durable: Some(CursorDurableBinding {
                data_dir: data_dir.to_path_buf(),
                id: CheckpointId::new(id),
            }),
        }
    }

    /// Construct a cursor bound to a durable checkpoint id. On
    /// construction the persisted position (if any) is loaded and the
    /// cursor resumes from it. Missing checkpoints yield a fresh cursor
    /// at position 0; a corrupt checkpoint fails closed with
    /// [`StoreError::CursorCheckpointCorrupt`].
    pub(crate) fn new_with_checkpoint(
        region: Region,
        index: Arc<StoreIndex>,
        data_dir: &Path,
        id: &str,
    ) -> Result<Self, StoreError> {
        let mut cursor = Self::new_bound_checkpoint(region, index, data_dir, id);
        match Self::load_checkpoint(data_dir, id) {
            Ok(Some(ckpt)) => {
                let expected_region = cursor.region.checkpoint_identity();
                if ckpt.region_identity.as_deref() != Some(expected_region.as_str()) {
                    return Err(StoreError::CursorCheckpointRegionMismatch {
                        path: cursor_checkpoint_path(data_dir, id),
                        stored: ckpt.region_identity,
                        expected: expected_region,
                    });
                }
                cursor.position = ckpt.position;
                cursor.started = ckpt.started;
            }
            Ok(None) => {}
            Err(error) => {
                if error.kind() == std::io::ErrorKind::InvalidData {
                    return Err(StoreError::CursorCheckpointCorrupt {
                        path: cursor_checkpoint_path(data_dir, id),
                        reason: error.to_string(),
                    });
                }
                return Err(StoreError::Io(error));
            }
        }
        Ok(cursor)
    }

    /// Poll for the next matching event at or after our current position.
    pub fn poll(&mut self) -> Option<IndexEntry> {
        let hits = self
            .index
            .query_hits_after(&self.region, self.position, self.started, 1);
        if let Some(hit) = hits.into_iter().next() {
            let expected_sequence = if self.started {
                self.position.saturating_add(1)
            } else {
                0
            };
            self.record_gap(expected_sequence, hit.global_sequence);
            self.position = hit.global_sequence;
            self.started = true;
            self.index.upgrade_hit(hit)
        } else {
            None
        }
    }

    /// Poll for up to max matching events.
    pub fn poll_batch(&mut self, max: usize) -> Vec<IndexEntry> {
        let hits = self
            .index
            .query_hits_after(&self.region, self.position, self.started, max);
        if hits.is_empty() {
            return Vec::new();
        }
        self.record_gaps_for_hits(&hits);
        self.started = true;
        self.position = hits[hits.len() - 1].global_sequence;
        hits.into_iter()
            .filter_map(|hit| self.index.upgrade_hit(hit))
            .collect()
    }

    /// Configure this cursor's in-memory write-to-deliver gap observation.
    #[must_use]
    pub fn with_gap_config(mut self, config: CursorGapConfig) -> Self {
        self.gap_buffer = if config.enabled {
            GapBuffer::new(config.buffer_capacity)
        } else {
            None
        };
        self
    }

    /// Drain the currently retained write-to-deliver gaps.
    pub fn take_gaps(&mut self) -> Vec<GapObservation> {
        match self.gap_buffer.as_mut() {
            Some(buffer) => buffer.take_all(),
            None => Vec::new(),
        }
    }

    pub(crate) fn checkpoint(&self) -> (u64, bool) {
        (self.position, self.started)
    }

    pub(crate) fn restore_checkpoint(&mut self, position: u64, started: bool) {
        self.position = position;
        self.started = started;
    }

    /// Persist the cursor's current position to its bound durable
    /// checkpoint. No-op if the cursor was constructed without an id.
    pub(crate) fn persist_current(&self) -> std::io::Result<()> {
        let Some(binding) = &self.durable else {
            return Ok(());
        };
        let ckpt = CursorCheckpoint::from_checkpoint(
            self.position,
            self.started,
            self.region.checkpoint_identity(),
        );
        Self::save_checkpoint(&binding.data_dir, binding.id.as_str(), &ckpt)
    }

    /// Load a persisted cursor checkpoint, or `Ok(None)` if none exists.
    ///
    /// # Errors
    /// Returns an I/O error if the checkpoint file exists but cannot be
    /// read. A decoding error yields `io::ErrorKind::InvalidData` so
    /// durable-resume callers can fail closed instead of silently
    /// rewinding to position 0.
    pub fn load_checkpoint(data_dir: &Path, id: &str) -> std::io::Result<Option<CursorCheckpoint>> {
        let path = cursor_checkpoint_path(data_dir, id);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        match rmp_serde::from_slice::<CursorCheckpoint>(&bytes) {
            Ok(ckpt) => Ok(Some(ckpt)),
            Err(error) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("cursor checkpoint decode failed: {error}"),
            )),
        }
    }

    /// Persist a cursor checkpoint atomically with a parent-directory
    /// fsync. The cursor-directory is created lazily if it does not
    /// already exist.
    ///
    /// # Errors
    /// Returns any I/O error from temp-file creation, write, fsync, or
    /// rename. Encoding errors are surfaced as `io::Error` with kind
    /// `Other`.
    pub fn save_checkpoint(
        data_dir: &Path,
        id: &str,
        ckpt: &CursorCheckpoint,
    ) -> std::io::Result<()> {
        let dir = cursor_checkpoint_dir(data_dir);
        std::fs::create_dir_all(&dir)?;
        let bytes =
            rmp_serde::to_vec_named(ckpt).map_err(|e| std::io::Error::other(e.to_string()))?;
        let final_path = cursor_checkpoint_path(data_dir, id);

        let mut tmp = NamedTempFile::new_in(&dir)?;
        {
            use std::io::Write;
            tmp.write_all(&bytes)?;
            tmp.flush()?;
        }
        // Fsync the temp contents before rename; `persist_with_parent_fsync`
        // does a defensive fsync too, but doing it here keeps the
        // durability boundary explicit.
        tmp.as_file().sync_all()?;
        persist_with_parent_fsync(tmp, &final_path)?;
        Ok(())
    }

    fn record_gaps_for_hits(&mut self, hits: &[crate::store::index::QueryHit]) {
        let mut expected_sequence = if self.started {
            self.position.saturating_add(1)
        } else {
            0
        };
        for hit in hits {
            self.record_gap(expected_sequence, hit.global_sequence);
            expected_sequence = hit.global_sequence.saturating_add(1);
        }
    }

    fn record_gap(&mut self, expected_sequence: u64, delivered_sequence: u64) {
        let Some(buffer) = self.gap_buffer.as_mut() else {
            return;
        };
        if delivered_sequence <= expected_sequence {
            return;
        }
        let cancelled_ranges = self
            .index
            .cancelled_visibility_ranges()
            .into_iter()
            .filter_map(|(start, end)| {
                let overlap_start = start.max(expected_sequence);
                let overlap_end = end.min(delivered_sequence);
                (overlap_start < overlap_end).then_some((overlap_start, overlap_end))
            })
            .collect::<Vec<_>>();
        if cancelled_ranges.is_empty() {
            return;
        }
        buffer.push(GapObservation {
            expected_sequence,
            delivered_sequence,
            cancelled_ranges,
        });
    }
}

/// Outcome returned by a cursor worker batch handler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CursorWorkerAction {
    /// Continue polling future batches. The worker commits the current
    /// cursor position as the new checkpoint.
    Continue,
    /// Stop the worker cleanly, committing the current cursor position.
    /// Use when the handler has successfully processed events in the
    /// current batch and now wants to wind down — e.g. a supervisor
    /// received a shutdown request.
    Stop,
    /// Stop the worker and **roll the cursor back to the last committed
    /// checkpoint** (i.e. the position before the current batch was
    /// polled). Use when the handler encountered a failure mid-batch
    /// and the events in the current batch must be re-delivered on the
    /// next poll. Replaces the older panic-to-rollback pattern: a typed
    /// reactor surfaces errors via its own channel and returns this
    /// action to keep the cursor honest without triggering
    /// `catch_unwind`.
    StopWithRollback,
}

/// Optional callback invoked once when the cursor-worker exhausts its
/// restart budget. Supplied by the reactor runner so it can stash a
/// `ReactorError::RestartBudgetExhausted` in its error slot before the
/// worker exits.
pub(crate) type RestartBudgetExhaustedCallback = Box<dyn FnOnce() + Send + 'static>;

/// G5: callback invoked when a **durable** cursor checkpoint write fails.
/// Only fires for cursors constructed with `checkpoint_id: Some(_)`;
/// in-memory cursors never call this because they perform no file
/// write. The callback receives the checkpoint `id` and the underlying
/// `io::Error` so the reactor runner can stash a
/// `ReactorError::Store(StoreError::CheckpointWriteFailed { .. })` in
/// its error slot before the worker exits. No silent downgrade of
/// durable-resume — a persist failure escalates to a hard error.
pub(crate) type CheckpointFailureCallback = Box<dyn FnMut(&str, std::io::Error) + Send + 'static>;

fn checkpoint_write_failed(id: &str, error: &std::io::Error) -> StoreError {
    StoreError::CheckpointWriteFailed {
        id: id.to_string(),
        source: std::io::Error::new(error.kind(), error.to_string()),
    }
}

fn build_worker_cursor(
    region: &Region,
    index: &Arc<StoreIndex>,
    data_dir: &Path,
    checkpoint_id: Option<&CheckpointId>,
    load_saved_checkpoint: bool,
) -> Result<Cursor, StoreError> {
    match checkpoint_id {
        Some(id) if load_saved_checkpoint => {
            Cursor::new_with_checkpoint(region.clone(), Arc::clone(index), data_dir, id.as_str())
        }
        Some(id) => Ok(Cursor::new_bound_checkpoint(
            region.clone(),
            Arc::clone(index),
            data_dir,
            id.as_str(),
        )),
        None => Ok(Cursor::new(region.clone(), Arc::clone(index))),
    }
}

/// Configuration for a supervised cursor worker thread.
///
/// This struct is `#[non_exhaustive]` — external callers should
/// construct it via [`CursorWorkerConfig::default`] and then update the
/// public fields they care about. New optional fields (e.g.
/// durable-checkpoint id, runner callbacks) are added here without
/// bumping the major version; the `non_exhaustive` attribute keeps that
/// a source-compat extension.
#[non_exhaustive]
pub struct CursorWorkerConfig {
    /// Maximum number of matching events to hand to the handler at once.
    pub batch_size: usize,
    /// Sleep duration when no matching events are currently available.
    pub idle_sleep: Duration,
    /// Panic restart policy for the worker loop. Governs only panics
    /// escaping the handler via `catch_unwind` — explicit `Err` returns
    /// and `StopWithRollback` actions stop the worker immediately.
    pub restart: RestartPolicy,
    /// Optional durable-checkpoint id. When `Some`, the worker's cursor
    /// is constructed with `new_with_checkpoint`, resumes from the
    /// persisted position on startup, and writes a fresh checkpoint
    /// after every successful poll batch. The checkpoint is bound to the
    /// exact `Region` this worker is consuming; reusing the same id for a
    /// different region fails closed at startup. If checkpoint persist
    /// fails, the worker stops and the failure surfaces through
    /// [`CursorWorkerHandle::join`] / [`CursorWorkerHandle::stop_and_join`]
    /// as [`StoreError::CheckpointWriteFailed`]. Startup checkpoint load
    /// and validation failures (`CursorCheckpointCorrupt`,
    /// `CursorCheckpointRegionMismatch`, or checkpoint read I/O errors)
    /// also stop the worker before the first batch and are observed from
    /// `join()` / `stop_and_join()`.
    pub checkpoint_id: Option<CheckpointId>,
    /// Optional cursor-owned gap observation config. Disabled by default.
    pub gap_observation: CursorGapConfig,
    /// Optional callback fired once when the restart budget is
    /// exhausted, before the worker exits. Used by the reactor runner
    /// to populate its error slot with `RestartBudgetExhausted`.
    pub(crate) on_restart_budget_exhausted: Option<RestartBudgetExhaustedCallback>,
    /// G5: optional callback fired when a durable checkpoint write
    /// fails. Used by the reactor runner to populate its error slot
    /// with `StoreError::CheckpointWriteFailed`. In-memory cursors
    /// never invoke this (no file write); it is strictly a durable-
    /// resume safeguard.
    pub(crate) on_checkpoint_failure: Option<CheckpointFailureCallback>,
}

impl Clone for CursorWorkerConfig {
    // The callbacks are not cloneable — they hold `FnOnce` / `FnMut`
    // closures that capture writer-side state. Cloning a
    // `CursorWorkerConfig` drops the callbacks rather than duplicating
    // them, which matches every existing caller's expectation: the
    // clone is for configuration reuse, not for spawning a second
    // worker.
    fn clone(&self) -> Self {
        Self {
            batch_size: self.batch_size,
            idle_sleep: self.idle_sleep,
            restart: self.restart.clone(),
            checkpoint_id: self.checkpoint_id.clone(),
            gap_observation: self.gap_observation,
            on_restart_budget_exhausted: None,
            on_checkpoint_failure: None,
        }
    }
}

impl std::fmt::Debug for CursorWorkerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CursorWorkerConfig")
            .field("batch_size", &self.batch_size)
            .field("idle_sleep", &self.idle_sleep)
            .field("restart", &self.restart)
            .field("checkpoint_id", &self.checkpoint_id)
            .field("gap_observation", &self.gap_observation)
            .field(
                "on_restart_budget_exhausted",
                &self.on_restart_budget_exhausted.is_some(),
            )
            .field(
                "on_checkpoint_failure",
                &self.on_checkpoint_failure.is_some(),
            )
            .finish()
    }
}

impl Default for CursorWorkerConfig {
    fn default() -> Self {
        Self {
            batch_size: 64,
            idle_sleep: Duration::from_millis(10),
            restart: RestartPolicy::Once,
            checkpoint_id: None,
            gap_observation: CursorGapConfig::default(),
            on_restart_budget_exhausted: None,
            on_checkpoint_failure: None,
        }
    }
}

/// Handle for a background cursor worker.
pub struct CursorWorkerHandle {
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
    error_slot: Arc<Mutex<Option<StoreError>>>,
}

impl CursorWorkerHandle {
    fn finish_join(&mut self) -> Result<(), StoreError> {
        if let Some(join) = self.join.take() {
            join.join().map_err(|_| StoreError::WriterCrashed)?;
        }
        let mut guard = self
            .error_slot
            .lock()
            .map_err(|_| StoreError::WriterCrashed)?;
        guard.take().map_or(Ok(()), Err)
    }

    /// Request a clean stop. The worker exits after the current iteration.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    /// Wait passively for the worker thread to exit.
    ///
    /// `join` does NOT signal stop — it blocks until the worker exits
    /// on its own (e.g. the handler returned `Stop` / `StopWithRollback`,
    /// the restart budget was exhausted, or a sibling called `stop()`).
    /// Pair with [`stop`](Self::stop) or use
    /// [`stop_and_join`](Self::stop_and_join) when you want to signal
    /// and wait in one call.
    ///
    /// # Errors
    /// Returns [`StoreError::WriterCrashed`] if the worker thread
    /// panicked before it could exit cleanly, or
    /// [`StoreError::CheckpointWriteFailed`] if a durable worker
    /// (`checkpoint_id: Some(_)`) failed to persist its checkpoint
    /// before exit. Startup checkpoint load and validation failures
    /// (`CursorCheckpointCorrupt`, `CursorCheckpointRegionMismatch`, or
    /// checkpoint read I/O errors) also surface here because worker
    /// startup is asynchronous.
    pub fn join(mut self) -> Result<(), StoreError> {
        self.finish_join()
    }

    /// Signal stop, then wait for the worker thread to exit.
    ///
    /// This is the previous `join` behaviour, renamed so callers that
    /// want a passive wait (e.g. tests exercising natural shutdown) can
    /// use [`join`](Self::join) without having to race `sleep`
    /// before-join.
    ///
    /// # Errors
    /// Returns [`StoreError::WriterCrashed`] if the worker thread
    /// panicked before it could exit cleanly, or
    /// [`StoreError::CheckpointWriteFailed`] if a durable worker
    /// (`checkpoint_id: Some(_)`) failed to persist its checkpoint
    /// before exit. Startup checkpoint load and validation failures
    /// (`CursorCheckpointCorrupt`, `CursorCheckpointRegionMismatch`, or
    /// checkpoint read I/O errors) also surface here because worker
    /// startup is asynchronous.
    pub fn stop_and_join(mut self) -> Result<(), StoreError> {
        self.stop();
        self.finish_join()
    }
}

impl Drop for CursorWorkerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

impl Store<crate::store::Open> {
    /// Spawn a supervised cursor worker that processes ordered pull batches,
    /// with durable at-least-once semantics when `checkpoint_id` is set.
    /// A durable worker either persists its cursor position after each
    /// successful batch or stops and reports
    /// [`StoreError::CheckpointWriteFailed`] through the returned handle's
    /// `join` path; it does not silently degrade to process-local resume.
    /// Startup checkpoint load and validation failures are also reported
    /// through the returned handle because the worker initializes
    /// asynchronously on its own thread.
    ///
    /// # Errors
    /// Returns [`StoreError::Io`] if the background worker thread cannot be
    /// spawned.
    pub fn cursor_worker<F>(
        self: &Arc<Self>,
        region: &Region,
        config: CursorWorkerConfig,
        mut handler: F,
    ) -> Result<CursorWorkerHandle, StoreError>
    where
        F: FnMut(&[IndexEntry], &Store<crate::store::Open>) -> CursorWorkerAction + Send + 'static,
    {
        let store = Arc::clone(self);
        let region = region.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let error_slot = Arc::new(Mutex::new(None));
        let error_slot_thread = Arc::clone(&error_slot);
        let CursorWorkerConfig {
            batch_size,
            idle_sleep,
            restart,
            checkpoint_id,
            gap_observation,
            on_restart_budget_exhausted,
            on_checkpoint_failure,
        } = config;

        let join = std::thread::Builder::new()
            .name("batpak-cursor-worker".into())
            .spawn(move || {
                let mut cursor = match build_worker_cursor(
                    &region,
                    &store.index,
                    &store.config.data_dir,
                    checkpoint_id.as_ref(),
                    true,
                ) {
                    Ok(cursor) => cursor,
                    Err(error) => {
                        if let Ok(mut guard) = error_slot_thread.lock() {
                            if guard.is_none() {
                                *guard = Some(error);
                            }
                        }
                        stop_thread.store(true, Ordering::Release);
                        return;
                    }
                };
                cursor = cursor.with_gap_config(gap_observation);
                let mut committed = cursor.checkpoint();
                let mut restarts = 0u32;
                let mut window_start = Instant::now();
                let mut budget_callback = on_restart_budget_exhausted;
                let checkpoint_error_slot = Arc::clone(&error_slot_thread);
                // G5: the durable-persist failure callback. For in-memory
                // cursors this slot stays `None` and the persist path is a
                // no-op; for durable cursors this callback stashes a
                // `ReactorError::Store(StoreError::CheckpointWriteFailed)`
                // in the reactor runner's error slot before we stop.
                let mut checkpoint_failure_callback = on_checkpoint_failure;

                while !stop_thread.load(Ordering::Acquire) {
                    let batch = cursor.poll_batch(batch_size);
                    if batch.is_empty() {
                        std::thread::sleep(idle_sleep);
                        continue;
                    }

                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        handler(&batch, &store)
                    }));

                    match result {
                        Ok(CursorWorkerAction::Continue) => {
                            let next_checkpoint = cursor.checkpoint();
                            if let Err(error) = cursor.persist_current() {
                                // G5: durable-cursor persist failure is a
                                // hard error. For in-memory cursors
                                // `persist_current` is a no-op so this
                                // branch is only reached when
                                // `checkpoint_id: Some(_)` was supplied.
                                // Stash via callback (so the reactor
                                // runner surfaces CheckpointWriteFailed
                                // through its join handle) AND roll the
                                // cursor back to the last committed
                                // position so the batch is re-delivered
                                // on restart. No silent downgrade.
                                let Some(id) = checkpoint_id.as_ref() else {
                                    debug_assert!(
                                        false,
                                        "in-memory cursor checkpoint persist failure is unreachable"
                                    );
                                    stop_thread.store(true, Ordering::Release);
                                    continue;
                                };
                                if let Ok(mut guard) = checkpoint_error_slot.lock() {
                                    if guard.is_none() {
                                        *guard = Some(checkpoint_write_failed(id.as_str(), &error));
                                    }
                                }
                                if let Some(cb) = checkpoint_failure_callback.as_mut() {
                                    cb(id.as_str(), error);
                                } else {
                                    tracing::error!(
                                        cursor_id = %id.as_str(),
                                        "durable cursor checkpoint persist failed; no \
                                         failure callback wired — stopping worker to \
                                         avoid silent durable-resume regression"
                                    );
                                }
                                cursor.restore_checkpoint(committed.0, committed.1);
                                stop_thread.store(true, Ordering::Release);
                                continue;
                            }
                            committed = next_checkpoint;
                        }
                        Ok(CursorWorkerAction::Stop) => {
                            let final_checkpoint = cursor.checkpoint();
                            if let Err(error) = cursor.persist_current() {
                                // G5: same hard-error contract on clean
                                // stop — a durable cursor that cannot
                                // persist must surface the error rather
                                // than silently lose progress.
                                let Some(id) = checkpoint_id.as_ref() else {
                                    debug_assert!(
                                        false,
                                        "in-memory cursor checkpoint persist failure is unreachable"
                                    );
                                    stop_thread.store(true, Ordering::Release);
                                    continue;
                                };
                                if let Ok(mut guard) = checkpoint_error_slot.lock() {
                                    if guard.is_none() {
                                        *guard = Some(checkpoint_write_failed(id.as_str(), &error));
                                    }
                                }
                                if let Some(cb) = checkpoint_failure_callback.as_mut() {
                                    cb(id.as_str(), error);
                                } else {
                                    tracing::error!(
                                        cursor_id = %id.as_str(),
                                        "durable cursor checkpoint persist failed on \
                                         clean stop; no failure callback wired"
                                    );
                                }
                            } else {
                                committed = final_checkpoint;
                            }
                            stop_thread.store(true, Ordering::Release);
                        }
                        Ok(CursorWorkerAction::StopWithRollback) => {
                            // Roll the cursor back to the last committed
                            // checkpoint so the events in the current
                            // batch will be re-delivered to a future
                            // reader (or to a restart after recovery).
                            // Do NOT persist here: the in-memory
                            // rollback reverts to `committed`, which is
                            // already the durable position (if any).
                            cursor.restore_checkpoint(committed.0, committed.1);
                            stop_thread.store(true, Ordering::Release);
                        }
                        Err(_) => {
                            let budget_ok = match &restart {
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
                                    if window_start.elapsed() > Duration::from_millis(*within_ms) {
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
                                    "cursor worker restart budget exhausted; stopping worker"
                                );
                                // D1: fire the reactor-runner callback
                                // so its error slot receives
                                // `ReactorError::RestartBudgetExhausted`
                                // before we exit. Taking the Option
                                // ensures the callback fires at most
                                // once even if the exhaustion path is
                                // re-entered via future refactors.
                                if let Some(cb) = budget_callback.take() {
                                    cb();
                                }
                                stop_thread.store(true, Ordering::Release);
                                continue;
                            }

                            tracing::warn!(
                                "cursor worker panicked; restarting from last checkpoint"
                            );
                            cursor = match build_worker_cursor(
                                &region,
                                &store.index,
                                &store.config.data_dir,
                                checkpoint_id.as_ref(),
                                false,
                            ) {
                                Ok(cursor) => cursor,
                                Err(error) => {
                                    if let Ok(mut guard) = error_slot_thread.lock() {
                                        if guard.is_none() {
                                            *guard = Some(error);
                                        }
                                    }
                                    stop_thread.store(true, Ordering::Release);
                                    continue;
                                }
                            };
                            cursor = cursor.with_gap_config(gap_observation);
                            cursor.restore_checkpoint(committed.0, committed.1);
                        }
                    }
                }
            })
            .map_err(StoreError::Io)?;

        Ok(CursorWorkerHandle {
            stop,
            join: Some(join),
            error_slot,
        })
    }
}
