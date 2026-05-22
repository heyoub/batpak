use crate::coordinate::Region;
use crate::store::delivery::canal::{Canal, CanalBatch, CanalClosed};
use crate::store::delivery::observation::CheckpointId;
use crate::store::index::{IndexEntry, StoreIndex};
use crate::store::StoreError;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

mod checkpoint;
mod gap;
mod worker;

pub use checkpoint::CursorCheckpoint;
use checkpoint::{cursor_checkpoint_path, CursorDurableBinding};
use gap::GapBuffer;
pub use gap::{CursorGapConfig, GapObservation};
pub use worker::{CursorWorkerAction, CursorWorkerConfig, CursorWorkerHandle};

/// Cursor: pull-based event consumption with ordered replay.
///
/// [`crate::store::Store::cursor_guaranteed`] exposes the process-local surface of this
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
        id: CheckpointId,
    ) -> Self {
        Self {
            region,
            position: 0,
            started: false,
            index,
            gap_buffer: None,
            durable: Some(CursorDurableBinding {
                data_dir: data_dir.to_path_buf(),
                id,
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
        id: &CheckpointId,
    ) -> Result<Self, StoreError> {
        let mut cursor = Self::new_bound_checkpoint(region, index, data_dir, id.clone());
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
        self.gap_buffer = match config {
            CursorGapConfig::Disabled => None,
            CursorGapConfig::Enabled { capacity } => Some(GapBuffer::new_nonzero(capacity)),
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
        Self::save_checkpoint(&binding.data_dir, &binding.id, &ckpt)
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

impl Canal for Cursor {
    type Item = IndexEntry;
    type Error = CanalClosed;

    fn pull_batch(
        &mut self,
        max: usize,
        deadline: Duration,
    ) -> Result<CanalBatch<Self::Item>, Self::Error> {
        if max == 0 {
            return Ok(CanalBatch::Empty);
        }
        let start = Instant::now();
        loop {
            let batch = self.poll_batch(max);
            match batch.len() {
                0 => {
                    if start.elapsed() >= deadline {
                        return Ok(CanalBatch::Empty);
                    }
                    let remaining = deadline.saturating_sub(start.elapsed());
                    std::thread::sleep(remaining.min(Duration::from_millis(1)));
                }
                1 => {
                    let mut batch = batch;
                    return Ok(CanalBatch::One(batch.remove(0)));
                }
                _ => return Ok(CanalBatch::Many(batch)),
            }
        }
    }
}
