use super::{Cursor, CursorGapConfig};
use crate::coordinate::Region;
use crate::store::delivery::canal::CanalHandle;
use crate::store::delivery::observation::{AtLeastOnce, CheckpointId};
use crate::store::index::{IndexEntry, StoreIndex};
use crate::store::{RestartPolicy, Store, StoreError};
use parking_lot::Mutex;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

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

fn stringify_panic_payload(panic_info: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = panic_info.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic_info.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
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
            Cursor::new_with_checkpoint(region.clone(), Arc::clone(index), data_dir, id)
        }
        Some(id) => Ok(Cursor::new_bound_checkpoint(
            region.clone(),
            Arc::clone(index),
            data_dir,
            id.clone(),
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
    join: Option<Box<dyn crate::store::platform::spawn::SimJoin>>,
    error_slot: Arc<Mutex<Option<StoreError>>>,
}

impl CursorWorkerHandle {
    fn finish_join(&mut self) -> Result<(), StoreError> {
        if let Some(join) = self.join.take() {
            join.join().map_err(|_| StoreError::WriterCrashed)?;
        }
        let mut guard = self.error_slot.lock();
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

impl CanalHandle for CursorWorkerHandle {
    fn stop(&self) {
        CursorWorkerHandle::stop(self);
    }

    fn join(self: Box<Self>) -> Result<(), StoreError> {
        (*self).join()
    }

    fn stop_and_join(self: Box<Self>) -> Result<(), StoreError> {
        (*self).stop_and_join()
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
    /// The handler's third parameter is `Some(&AtLeastOnce)` for workers
    /// configured with a durable `checkpoint_id`, and `None` for in-memory
    /// workers.
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
        handler: F,
    ) -> Result<CursorWorkerHandle, StoreError>
    where
        F: FnMut(
                &[IndexEntry],
                &Store<crate::store::Open>,
                Option<&AtLeastOnce>,
            ) -> CursorWorkerAction
            + Send
            + 'static,
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
        let at_least_once = checkpoint_id
            .as_ref()
            .map(|id| AtLeastOnce::from_cursor_callback(id.as_str()))
            .transpose()
            .map_err(StoreError::CheckpointId)?;

        let loop_ctx = CursorWorkerLoop {
            store,
            region,
            stop: stop_thread,
            error_slot: error_slot_thread,
            batch_size,
            idle_sleep,
            restart,
            checkpoint_id,
            gap_observation,
            on_restart_budget_exhausted,
            on_checkpoint_failure,
            at_least_once,
        };
        let join = self
            .config
            .spawner()
            .spawn(
                "batpak-cursor-worker".to_string(),
                None,
                Box::new(move || loop_ctx.run(handler)),
            )
            .map_err(StoreError::Io)?;

        Ok(CursorWorkerHandle {
            stop,
            join: Some(join),
            error_slot,
        })
    }
}

/// Owned state for the background cursor-worker loop, extracted from
/// `Store::cursor_worker` so the spawned body is a single `loop_ctx.run()`
/// call. Behavior is identical to the previous inline closure; this is purely
/// a structural extraction to keep `cursor_worker` under its complexity pin.
struct CursorWorkerLoop {
    store: Arc<Store>,
    region: Region,
    stop: Arc<AtomicBool>,
    error_slot: Arc<Mutex<Option<StoreError>>>,
    batch_size: usize,
    idle_sleep: Duration,
    restart: RestartPolicy,
    checkpoint_id: Option<CheckpointId>,
    gap_observation: CursorGapConfig,
    on_restart_budget_exhausted: Option<RestartBudgetExhaustedCallback>,
    on_checkpoint_failure: Option<CheckpointFailureCallback>,
    at_least_once: Option<AtLeastOnce>,
}

/// Control flow returned by per-batch handling: keep looping or stop.
enum Step {
    Continue,
    Stop,
}

/// Mutable per-run state for the cursor-worker loop. Held separately from the
/// immutable [`CursorWorkerLoop`] config so the loop helpers can borrow it
/// mutably without re-borrowing the config.
struct CursorRunState {
    cursor: Cursor,
    committed: (u64, bool),
    restarts: u32,
    window_start_ns: i64,
}

impl CursorWorkerLoop {
    /// Record the first error into the worker's error slot (later errors are
    /// dropped — the first failure is the one that stops the worker).
    fn record_first_error(&self, error: StoreError) {
        let mut guard = self.error_slot.lock();
        if guard.is_none() {
            *guard = Some(error);
        }
    }

    /// Signal the worker to stop on the next loop check.
    fn signal_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    fn run<F>(mut self, mut handler: F)
    where
        F: FnMut(
                &[IndexEntry],
                &Store<crate::store::Open>,
                Option<&AtLeastOnce>,
            ) -> CursorWorkerAction
            + Send
            + 'static,
    {
        let mut cursor = match build_worker_cursor(
            &self.region,
            &self.store.index,
            &self.store.config.data_dir,
            self.checkpoint_id.as_ref(),
            true,
        ) {
            Ok(cursor) => cursor,
            Err(error) => {
                self.record_first_error(error);
                self.signal_stop();
                return;
            }
        };
        cursor = cursor.with_gap_config(self.gap_observation);
        let committed = cursor.checkpoint();
        let mut state = CursorRunState {
            cursor,
            committed,
            restarts: 0,
            window_start_ns: self.store.runtime.now_mono_ns(),
        };

        while !self.stop.load(Ordering::Acquire) {
            let batch = state.cursor.poll_batch(self.batch_size);
            if batch.is_empty() {
                std::thread::sleep(self.idle_sleep);
                continue;
            }

            let at_least_once = self.at_least_once.as_ref();
            let store = &self.store;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                handler(&batch, store, at_least_once)
            }));

            let step = match result {
                Ok(action) => self.handle_action(action, &mut state),
                Err(panic_info) => self.handle_panic(panic_info.as_ref(), &mut state),
            };
            if matches!(step, Step::Stop) {
                self.signal_stop();
            }
        }
    }

    /// Apply a successful handler [`CursorWorkerAction`] to the run state.
    fn handle_action(&mut self, action: CursorWorkerAction, state: &mut CursorRunState) -> Step {
        match action {
            CursorWorkerAction::Continue => {
                let next_checkpoint = state.cursor.checkpoint();
                if let Err(error) = state.cursor.persist_current() {
                    // G5: durable-cursor persist failure is a hard error. For
                    // in-memory cursors `persist_current` is a no-op, so this
                    // branch is only reached when `checkpoint_id: Some(_)` was
                    // supplied. Stash via callback (so the reactor runner
                    // surfaces CheckpointWriteFailed) AND roll the cursor back
                    // to the last committed position so the batch is
                    // re-delivered on restart. No silent downgrade.
                    self.report_persist_failure(
                        error,
                        "no \
                         failure callback wired — stopping worker to avoid \
                         silent durable-resume regression",
                    );
                    state
                        .cursor
                        .restore_checkpoint(state.committed.0, state.committed.1);
                    return Step::Stop;
                }
                state.committed = next_checkpoint;
                Step::Continue
            }
            CursorWorkerAction::Stop => {
                let final_checkpoint = state.cursor.checkpoint();
                if let Err(error) = state.cursor.persist_current() {
                    // G5: same hard-error contract on clean stop — a durable
                    // cursor that cannot persist must surface the error rather
                    // than silently lose progress.
                    self.report_persist_failure(error, "no failure callback wired (clean stop)");
                } else {
                    state.committed = final_checkpoint;
                }
                Step::Stop
            }
            CursorWorkerAction::StopWithRollback => {
                // Roll the cursor back to the last committed checkpoint so the
                // events in the current batch will be re-delivered to a future
                // reader (or to a restart after recovery). Do NOT persist here:
                // the in-memory rollback reverts to `committed`, which is
                // already the durable position (if any).
                state
                    .cursor
                    .restore_checkpoint(state.committed.0, state.committed.1);
                Step::Stop
            }
        }
    }

    /// G5: stash a durable-checkpoint persist failure and fire the failure
    /// callback (or log when none is wired). In-memory cursors never reach
    /// here because `persist_current` is a no-op for them.
    fn report_persist_failure(&mut self, error: std::io::Error, no_callback_note: &str) {
        let Some(id) = self.checkpoint_id.as_ref() else {
            debug_assert!(
                false,
                "in-memory cursor checkpoint persist failure is unreachable"
            );
            return;
        };
        {
            let mut guard = self.error_slot.lock();
            if guard.is_none() {
                *guard = Some(checkpoint_write_failed(id.as_str(), &error));
            }
        }
        if let Some(cb) = self.on_checkpoint_failure.as_mut() {
            cb(id.as_str(), error);
        } else {
            tracing::error!(
                cursor_id = %id.as_str(),
                "durable cursor checkpoint persist failed; {no_callback_note}"
            );
        }
    }

    /// Handle a panic that escaped the user handler: consume a restart slot if
    /// the policy allows, then rebuild the cursor from the last checkpoint.
    fn handle_panic(
        &mut self,
        panic_info: &(dyn std::any::Any + Send),
        state: &mut CursorRunState,
    ) -> Step {
        let panic_msg = stringify_panic_payload(panic_info);
        if !self.restart_budget_ok(state) {
            tracing::error!(
                "cursor worker restart budget exhausted; stopping worker. \
                 Last panic: {panic_msg}"
            );
            // D1: fire the reactor-runner callback so its error slot receives
            // `ReactorError::RestartBudgetExhausted` before we exit. Taking
            // the Option ensures the callback fires at most once even if the
            // exhaustion path is re-entered via future refactors.
            if let Some(cb) = self.on_restart_budget_exhausted.take() {
                cb();
            }
            return Step::Stop;
        }

        tracing::warn!(
            "cursor worker panicked; restarting from last checkpoint. \
             Panic: {panic_msg}"
        );
        let cursor = match build_worker_cursor(
            &self.region,
            &self.store.index,
            &self.store.config.data_dir,
            self.checkpoint_id.as_ref(),
            false,
        ) {
            Ok(cursor) => cursor,
            Err(error) => {
                self.record_first_error(error);
                return Step::Stop;
            }
        };
        state.cursor = cursor.with_gap_config(self.gap_observation);
        state
            .cursor
            .restore_checkpoint(state.committed.0, state.committed.1);
        Step::Continue
    }

    /// Whether the restart policy permits one more restart, consuming a slot.
    fn restart_budget_ok(&self, state: &mut CursorRunState) -> bool {
        match &self.restart {
            RestartPolicy::Once => {
                if state.restarts >= 1 {
                    false
                } else {
                    state.restarts += 1;
                    true
                }
            }
            RestartPolicy::Bounded {
                max_restarts,
                within_ms,
            } => {
                let elapsed_ms = self
                    .store
                    .runtime
                    .now_mono_ns()
                    .saturating_sub(state.window_start_ns)
                    .max(0)
                    / 1_000_000;
                if elapsed_ms > i64::try_from(*within_ms).unwrap_or(i64::MAX) {
                    state.restarts = 0;
                    state.window_start_ns = self.store.runtime.now_mono_ns();
                }
                if state.restarts >= *max_restarts {
                    false
                } else {
                    state.restarts += 1;
                    true
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::stringify_panic_payload;
    use std::any::Any;

    #[test]
    fn stringify_panic_payload_preserves_static_str_payload() {
        let payload: Box<dyn Any + Send> = Box::new("cursor panic detail");

        assert_eq!(
            stringify_panic_payload(payload.as_ref()),
            "cursor panic detail",
            "PROPERTY: cursor worker panic evidence should include literal panic payloads"
        );
    }

    #[test]
    fn stringify_panic_payload_preserves_string_payload() {
        let payload: Box<dyn Any + Send> = Box::new(String::from("owned cursor panic detail"));

        assert_eq!(
            stringify_panic_payload(payload.as_ref()),
            "owned cursor panic detail",
            "PROPERTY: cursor worker panic evidence should include owned String panic payloads"
        );
    }

    #[test]
    fn stringify_panic_payload_falls_back_for_non_string_payload() {
        let payload: Box<dyn Any + Send> = Box::new(17_u32);

        assert_eq!(
            stringify_panic_payload(payload.as_ref()),
            "unknown panic",
            "PROPERTY: cursor worker panic evidence should remain well-formed for opaque payloads"
        );
    }
}
