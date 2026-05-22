use super::IndexEntry;
use parking_lot::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Gated publish boundary for reader visibility.
///
/// `allocated` advances when sequences are reserved (writer-only).
/// `visible` is the exclusive upper bound readers filter against:
/// an entry is visible iff `entry.global_sequence < visible`.
///
/// Invariant: `visible <= allocated` (enforced by `debug_assert` in `publish`).
pub(crate) struct SequenceGate {
    /// Next sequence to be assigned. Only the writer thread advances this.
    allocated: AtomicU64,
    /// Exclusive upper bound for reader visibility. Entries with
    /// `global_sequence < visible` are returned by read methods.
    visible: AtomicU64,
    /// Currently active visibility fence token, or 0 when no fence is active.
    active_fence: AtomicU64,
    /// Lowest sequence staged into the active fence, or `u64::MAX` if the
    /// fence has not yet staged any entries.
    active_fence_start: AtomicU64,
    /// Exclusive upper bound of the highest sequence staged into the active fence.
    active_fence_end: AtomicU64,
    /// Monotonic token allocator for visibility fences.
    next_fence_token: AtomicU64,
    /// Permanently hidden fence ranges cancelled in the current runtime.
    /// Stored as an immutable `Arc` snapshot so that readers pay only a
    /// refcount bump instead of cloning the whole vec on every query.
    cancelled_ranges: RwLock<Arc<Vec<(u64, u64)>>>,
}

#[derive(Clone, Debug)]
pub(crate) struct VisibilitySnapshot {
    visible: u64,
    cancelled_ranges: Arc<Vec<(u64, u64)>>,
}

impl VisibilitySnapshot {
    pub(crate) fn is_visible(&self, sequence: u64) -> bool {
        if sequence >= self.visible {
            return false;
        }
        !self
            .cancelled_ranges
            .iter()
            .any(|(start, end)| sequence >= *start && sequence < *end)
    }

    pub(crate) fn visible_upper_bound(&self) -> u64 {
        self.visible
    }
}

pub(super) fn extend_visible_entries<'a, I>(
    out: &mut Vec<IndexEntry>,
    entries: I,
    visibility: &VisibilitySnapshot,
) where
    I: IntoIterator<Item = &'a Arc<IndexEntry>>,
{
    for entry in entries {
        if visibility.is_visible(entry.global_sequence) {
            out.push(entry.as_ref().clone());
        }
    }
}

impl SequenceGate {
    fn insert_cancelled_range(ranges: &mut Vec<(u64, u64)>, start: u64, end: u64) {
        if start >= end {
            return;
        }
        ranges.push((start, end));
        ranges.sort_by_key(|(range_start, _)| *range_start);

        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
        for (range_start, range_end) in ranges.drain(..) {
            if let Some((_, merged_end)) = merged.last_mut() {
                if range_start <= *merged_end {
                    *merged_end = (*merged_end).max(range_end);
                    continue;
                }
            }
            merged.push((range_start, range_end));
        }
        *ranges = merged;
    }

    pub(crate) fn new() -> Self {
        Self {
            allocated: AtomicU64::new(0),
            visible: AtomicU64::new(0),
            active_fence: AtomicU64::new(0),
            active_fence_start: AtomicU64::new(u64::MAX),
            active_fence_end: AtomicU64::new(0),
            next_fence_token: AtomicU64::new(1),
            cancelled_ranges: RwLock::new(Arc::new(Vec::new())),
        }
    }

    /// Reserve `n` sequences. Returns first in `[first, first + n)`.
    pub(crate) fn reserve(&self, n: u64) -> u64 {
        self.allocated.fetch_add(n, Ordering::AcqRel)
    }

    /// Advance visibility so readers see entries with `global_sequence < up_to`.
    pub(crate) fn publish(
        &self,
        up_to: u64,
        operation: &'static str,
    ) -> Result<(), crate::store::StoreError> {
        let allocated = self.allocated.load(Ordering::Acquire);
        let visible = self.visible.load(Ordering::Acquire);
        if up_to > allocated || up_to < visible {
            return Err(crate::store::StoreError::SequenceGateViolation {
                operation,
                requested: up_to,
                allocated,
                visible,
            });
        }
        self.visible.store(up_to, Ordering::Release);
        Ok(())
    }

    /// Current visibility watermark (exclusive upper bound).
    pub(crate) fn visible(&self) -> u64 {
        self.visible.load(Ordering::Acquire)
    }

    /// Current allocator position (next sequence to be assigned).
    pub(crate) fn allocated(&self) -> u64 {
        self.allocated.load(Ordering::Acquire)
    }

    /// Advance allocator by 1. Used by `insert()` for the single-event path.
    pub(crate) fn advance(&self) {
        self.allocated.fetch_add(1, Ordering::Release);
    }

    /// Set the allocator to a specific value during checkpoint restore.
    pub(crate) fn restore_allocator(&self, value: u64) {
        self.allocated.store(value, Ordering::Release);
    }

    /// Reset both counters to 0 (used by `clear()` during rebuild/compaction).
    pub(crate) fn clear(&self) {
        self.allocated.store(0, Ordering::Release);
        self.visible.store(0, Ordering::Release);
        self.active_fence.store(0, Ordering::Release);
        self.active_fence_start.store(u64::MAX, Ordering::Release);
        self.active_fence_end.store(0, Ordering::Release);
        self.next_fence_token.store(1, Ordering::Release);
        *self.cancelled_ranges.write() = Arc::new(Vec::new());
    }

    pub(crate) fn begin_fence(&self) -> Result<u64, crate::store::StoreError> {
        let token = self.next_fence_token.fetch_add(1, Ordering::AcqRel);
        match self
            .active_fence
            .compare_exchange(0, token, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => {
                self.active_fence_start.store(u64::MAX, Ordering::Release);
                self.active_fence_end.store(0, Ordering::Release);
                Ok(token)
            }
            Err(_) => Err(crate::store::StoreError::VisibilityFenceActive),
        }
    }

    pub(crate) fn active_fence_token(&self) -> Option<u64> {
        let token = self.active_fence.load(Ordering::Acquire);
        (token != 0).then_some(token)
    }

    pub(crate) fn note_fence_progress(
        &self,
        token: u64,
        start: u64,
        end: u64,
    ) -> Result<(), crate::store::StoreError> {
        if self.active_fence.load(Ordering::Acquire) != token {
            return Err(crate::store::StoreError::VisibilityFenceNotActive);
        }
        let _start_update =
            self.active_fence_start
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    Some(current.min(start))
                });
        let _end_update =
            self.active_fence_end
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    Some(current.max(end))
                });
        Ok(())
    }

    pub(crate) fn finish_fence(
        &self,
        token: u64,
        publish_to: Option<u64>,
    ) -> Result<(), crate::store::StoreError> {
        if self.active_fence.load(Ordering::Acquire) != token {
            return Err(crate::store::StoreError::VisibilityFenceNotActive);
        }
        if let Some(up_to) = publish_to {
            self.publish(up_to, "finish_visibility_fence")?;
        }
        self.active_fence.store(0, Ordering::Release);
        self.active_fence_start.store(u64::MAX, Ordering::Release);
        self.active_fence_end.store(0, Ordering::Release);
        Ok(())
    }

    pub(crate) fn cancel_fence(&self, token: u64) -> Result<(), crate::store::StoreError> {
        if self.active_fence.load(Ordering::Acquire) != token {
            return Err(crate::store::StoreError::VisibilityFenceNotActive);
        }
        let start = self.active_fence_start.load(Ordering::Acquire);
        let end = self.active_fence_end.load(Ordering::Acquire);
        if start != u64::MAX && start < end {
            let mut guard = self.cancelled_ranges.write();
            let mut ranges = (**guard).clone();
            Self::insert_cancelled_range(&mut ranges, start, end);
            *guard = Arc::new(ranges);
        }
        self.active_fence.store(0, Ordering::Release);
        self.active_fence_start.store(u64::MAX, Ordering::Release);
        self.active_fence_end.store(0, Ordering::Release);
        Ok(())
    }

    pub(crate) fn snapshot(&self) -> VisibilitySnapshot {
        VisibilitySnapshot {
            visible: self.visible.load(Ordering::Acquire),
            cancelled_ranges: Arc::clone(&self.cancelled_ranges.read()),
        }
    }

    pub(crate) fn cancelled_ranges_snapshot(&self) -> Vec<(u64, u64)> {
        self.cancelled_ranges.read().as_ref().clone()
    }

    pub(crate) fn restore_cancelled_ranges(&self, ranges: Vec<(u64, u64)>) {
        let mut built = Vec::new();
        for (start, end) in ranges {
            Self::insert_cancelled_range(&mut built, start, end);
        }
        *self.cancelled_ranges.write() = Arc::new(built);
    }
}
