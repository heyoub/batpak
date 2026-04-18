use crate::coordinate::Region;
use crate::store::cold_start::persist_with_parent_fsync;
use crate::store::index::{IndexEntry, StoreIndex};
use crate::store::{RestartPolicy, Store, StoreError};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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
    /// Global sequence one-past-last-delivered. A subsequent poll returns
    /// events strictly after this position when `started` is true.
    pub position: u64,
    /// Whether the cursor has delivered at least one event. A fresh
    /// cursor starts at position 0 with `started = false` so that
    /// global_sequence 0 (a legitimate value) is not skipped.
    pub started: bool,
    /// Process-boot monotonic clock value at the time of the last save.
    /// Reserved for monotonic-clock integration; `None` when not wired.
    pub process_boot_ns: Option<u64>,
}

impl CursorCheckpoint {
    fn from_checkpoint(position: u64, started: bool) -> Self {
        Self {
            position,
            started,
            process_boot_ns: None,
        }
    }
}

fn cursor_checkpoint_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("cursors")
}

fn cursor_checkpoint_path(data_dir: &Path, id: &str) -> PathBuf {
    cursor_checkpoint_dir(data_dir).join(format!("{id}.ckpt"))
}

/// Cursor: pull-based event consumption with guaranteed delivery.
///
/// Cursor durability — a cursor with a `checkpoint_id` persists its
/// position durably via tempfile + parent-directory fsync after every
/// successful batch. On process restart, constructing a cursor with the
/// same `id` resumes from the persisted position. Cursors without an id
/// (the default) are process-local only.
///
/// Delivery semantics:
///
/// * **Without `checkpoint_id`:** at-least-once within process lifetime.
///   Events delivered before a process crash are re-delivered on next
///   start because the in-memory cursor position is lost.
/// * **With `checkpoint_id`:** at-least-once across restarts. A crash
///   between delivery and checkpoint save causes the latest batch to be
///   re-delivered — callers must treat their handler as idempotent.
///
/// Neither mode is exactly-once: that guarantee requires coordinating
/// the cursor checkpoint with the downstream side-effect in a single
/// atomic write, which this type does not attempt.
pub struct Cursor {
    region: Region,
    position: u64, // tracks global_sequence — next poll starts after this
    started: bool, // false until first event consumed (global_sequence 0 is valid)
    index: Arc<StoreIndex>,
    /// Optional durable-checkpoint id. When set, the cursor was
    /// constructed with a data directory and a checkpoint identifier so
    /// its position can be persisted via `save_checkpoint`.
    durable: Option<CursorDurableBinding>,
}

#[derive(Clone, Debug)]
struct CursorDurableBinding {
    data_dir: PathBuf,
    id: String,
}

impl Cursor {
    pub(crate) fn new(region: Region, index: Arc<StoreIndex>) -> Self {
        Self {
            region,
            position: 0,
            started: false,
            index,
            durable: None,
        }
    }

    /// Construct a cursor bound to a durable checkpoint id. On
    /// construction the persisted position (if any) is loaded and the
    /// cursor resumes from it. Missing or malformed checkpoint files
    /// yield a fresh cursor at position 0 — a corrupt checkpoint never
    /// blocks delivery, it only loses progress.
    pub(crate) fn new_with_checkpoint(
        region: Region,
        index: Arc<StoreIndex>,
        data_dir: &Path,
        id: &str,
    ) -> Self {
        let mut cursor = Self {
            region,
            position: 0,
            started: false,
            index,
            durable: Some(CursorDurableBinding {
                data_dir: data_dir.to_path_buf(),
                id: id.to_owned(),
            }),
        };
        match Self::load_checkpoint(data_dir, id) {
            Ok(Some(ckpt)) => {
                cursor.position = ckpt.position;
                cursor.started = ckpt.started;
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    cursor_id = %id,
                    error = %error,
                    "failed to load cursor checkpoint — starting fresh"
                );
            }
        }
        cursor
    }

    /// Poll for the next matching event at or after our current position.
    pub fn poll(&mut self) -> Option<IndexEntry> {
        let hits = self
            .index
            .query_hits_after(&self.region, self.position, self.started, 1);
        if let Some(hit) = hits.into_iter().next() {
            self.position = hit.global_sequence;
            self.started = true;
            Some(self.index.upgrade_hit(hit))
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
        self.started = true;
        self.position = hits
            .last()
            .expect("non-empty vec has a last element")
            .global_sequence;
        hits.into_iter()
            .map(|hit| self.index.upgrade_hit(hit))
            .collect()
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
        let ckpt = CursorCheckpoint::from_checkpoint(self.position, self.started);
        Self::save_checkpoint(&binding.data_dir, &binding.id, &ckpt)
    }

    /// Load a persisted cursor checkpoint, or `Ok(None)` if none exists.
    ///
    /// # Errors
    /// Returns an I/O error if the checkpoint file exists but cannot be
    /// read. A decoding error yields `Ok(None)` — a corrupt checkpoint
    /// is treated as a missing one because it never makes progress to
    /// force a user-visible failure out of it.
    pub fn load_checkpoint(data_dir: &Path, id: &str) -> std::io::Result<Option<CursorCheckpoint>> {
        let path = cursor_checkpoint_path(data_dir, id);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        match rmp_serde::from_slice::<CursorCheckpoint>(&bytes) {
            Ok(ckpt) => Ok(Some(ckpt)),
            Err(error) => {
                tracing::warn!(
                    cursor_id = %id,
                    error = %error,
                    "cursor checkpoint decode failed — treating as missing"
                );
                Ok(None)
            }
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
    /// after every successful poll batch.
    pub checkpoint_id: Option<String>,
    /// Optional callback fired once when the restart budget is
    /// exhausted, before the worker exits. Used by the reactor runner
    /// to populate its error slot with `RestartBudgetExhausted`.
    pub(crate) on_restart_budget_exhausted: Option<RestartBudgetExhaustedCallback>,
}

impl Clone for CursorWorkerConfig {
    // The callback is not cloneable — it is a one-shot `FnOnce`. Cloning
    // a `CursorWorkerConfig` drops the callback rather than duplicating
    // it, which matches every existing caller's expectation: the clone
    // is for configuration reuse, not for spawning a second worker.
    fn clone(&self) -> Self {
        Self {
            batch_size: self.batch_size,
            idle_sleep: self.idle_sleep,
            restart: self.restart.clone(),
            checkpoint_id: self.checkpoint_id.clone(),
            on_restart_budget_exhausted: None,
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
            .field(
                "on_restart_budget_exhausted",
                &self.on_restart_budget_exhausted.is_some(),
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
            on_restart_budget_exhausted: None,
        }
    }
}

/// Handle for a background cursor worker.
pub struct CursorWorkerHandle {
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl CursorWorkerHandle {
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
    /// panicked before it could exit cleanly.
    pub fn join(mut self) -> Result<(), StoreError> {
        if let Some(join) = self.join.take() {
            join.join().map_err(|_| StoreError::WriterCrashed)?;
        }
        Ok(())
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
    /// panicked before it could exit cleanly.
    pub fn stop_and_join(mut self) -> Result<(), StoreError> {
        self.stop();
        if let Some(join) = self.join.take() {
            join.join().map_err(|_| StoreError::WriterCrashed)?;
        }
        Ok(())
    }
}

impl Drop for CursorWorkerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

impl Store<crate::store::Open> {
    /// Spawn a supervised cursor worker that processes guaranteed-delivery batches.
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
        let CursorWorkerConfig {
            batch_size,
            idle_sleep,
            restart,
            checkpoint_id,
            on_restart_budget_exhausted,
        } = config;

        let join = std::thread::Builder::new()
            .name("batpak-cursor-worker".into())
            .spawn(move || {
                let mut cursor = match checkpoint_id.clone() {
                    Some(id) => Cursor::new_with_checkpoint(
                        region.clone(),
                        Arc::clone(&store.index),
                        &store.config.data_dir,
                        &id,
                    ),
                    None => store.cursor_guaranteed(&region),
                };
                let mut committed = cursor.checkpoint();
                let mut restarts = 0u32;
                let mut window_start = Instant::now();
                let mut budget_callback = on_restart_budget_exhausted;

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
                            committed = cursor.checkpoint();
                            if let Err(error) = cursor.persist_current() {
                                tracing::error!(
                                    error = %error,
                                    "cursor checkpoint persist failed — \
                                     continuing in-memory; a crash before \
                                     the next successful persist will \
                                     re-deliver this batch"
                                );
                            }
                        }
                        Ok(CursorWorkerAction::Stop) => {
                            committed = cursor.checkpoint();
                            if let Err(error) = cursor.persist_current() {
                                tracing::error!(
                                    error = %error,
                                    "cursor checkpoint persist failed on clean stop"
                                );
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
                            cursor = match checkpoint_id.clone() {
                                Some(id) => Cursor::new_with_checkpoint(
                                    region.clone(),
                                    Arc::clone(&store.index),
                                    &store.config.data_dir,
                                    &id,
                                ),
                                None => store.cursor_guaranteed(&region),
                            };
                            cursor.restore_checkpoint(committed.0, committed.1);
                        }
                    }
                }
            })
            .map_err(StoreError::Io)?;

        Ok(CursorWorkerHandle {
            stop,
            join: Some(join),
        })
    }
}
