use crate::coordinate::Region;
use crate::store::index::{IndexEntry, StoreIndex};
use crate::store::{RestartPolicy, Store, StoreError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Cursor: pull-based event consumption with guaranteed delivery.
/// Reads from index, not channels. Cannot lose events.
pub struct Cursor {
    region: Region,
    position: u64, // tracks global_sequence — next poll starts after this
    started: bool, // false until first event consumed (global_sequence 0 is valid)
    index: Arc<StoreIndex>,
}

impl Cursor {
    pub(crate) fn new(region: Region, index: Arc<StoreIndex>) -> Self {
        Self {
            region,
            position: 0,
            started: false,
            index,
        }
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
}

/// Outcome returned by a cursor worker batch handler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CursorWorkerAction {
    /// Continue polling future batches.
    Continue,
    /// Stop the worker cleanly after the current handler returns.
    Stop,
}

/// Configuration for a supervised cursor worker thread.
#[derive(Clone, Debug)]
pub struct CursorWorkerConfig {
    /// Maximum number of matching events to hand to the handler at once.
    pub batch_size: usize,
    /// Sleep duration when no matching events are currently available.
    pub idle_sleep: Duration,
    /// Panic restart policy for the worker loop.
    pub restart: RestartPolicy,
}

impl Default for CursorWorkerConfig {
    fn default() -> Self {
        Self {
            batch_size: 64,
            idle_sleep: Duration::from_millis(10),
            restart: RestartPolicy::Once,
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

    /// Join the worker thread and surface thread panics as store errors.
    ///
    /// # Errors
    /// Returns [`StoreError::WriterCrashed`] if the worker thread panicked
    /// before it could exit cleanly.
    pub fn join(mut self) -> Result<(), StoreError> {
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

        let join = std::thread::Builder::new()
            .name("batpak-cursor-worker".into())
            .spawn(move || {
                let mut cursor = store.cursor_guaranteed(&region);
                let mut committed = cursor.checkpoint();
                let mut restarts = 0u32;
                let mut window_start = Instant::now();

                while !stop_thread.load(Ordering::Acquire) {
                    let batch = cursor.poll_batch(config.batch_size);
                    if batch.is_empty() {
                        std::thread::sleep(config.idle_sleep);
                        continue;
                    }

                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        handler(&batch, &store)
                    }));

                    match result {
                        Ok(CursorWorkerAction::Continue) => {
                            committed = cursor.checkpoint();
                        }
                        Ok(CursorWorkerAction::Stop) => {
                            committed = cursor.checkpoint();
                            stop_thread.store(true, Ordering::Release);
                        }
                        Err(_) => {
                            let budget_ok = match &config.restart {
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
                                stop_thread.store(true, Ordering::Release);
                                continue;
                            }

                            tracing::warn!(
                                "cursor worker panicked; restarting from last checkpoint"
                            );
                            cursor = store.cursor_guaranteed(&region);
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
