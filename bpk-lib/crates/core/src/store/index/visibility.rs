use super::IndexEntry;
use crate::store::hidden_ranges::CancelledVisibilityRanges;
use parking_lot::{Condvar, Mutex, RwLock};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub(super) type CancelledRange = (u64, u64);
pub(super) type CancelledRanges = Vec<CancelledRange>;
pub(super) type LaneCancelledRanges = BTreeMap<u32, CancelledRanges>;

#[derive(Clone, Debug)]
struct PublishedVisibility {
    visible: u64,
    lane_visible: BTreeMap<u32, u64>,
}

/// Gated publish boundary for reader visibility.
///
/// `allocated` advances when sequences are reserved (writer-only).
/// `visible` is the exclusive upper bound readers filter against:
/// an entry is visible iff `entry.global_sequence < visible`.
///
/// Invariant: `visible <= allocated`, and `visible` advances monotonically.
/// Enforced at runtime in [`SequenceGate::publish`], which returns
/// [`StoreError::SequenceGateViolation`](crate::store::StoreError) when an
/// `up_to` would exceed `allocated` or regress below the current `visible` —
/// a fail-closed `Result`, not a `debug_assert` (a regression must be a hard
/// error in release, not a debug-only check).
pub(crate) struct SequenceGate {
    /// Next sequence to be assigned. Only the writer thread advances this.
    allocated: AtomicU64,
    /// Coherent global + per-lane reader visibility bounds.
    published: RwLock<Arc<PublishedVisibility>>,
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
    cancelled_ranges: RwLock<Arc<CancelledRanges>>,
    /// Per-lane permanently hidden fence ranges over the global sequence axis.
    lane_cancelled_ranges: RwLock<Arc<LaneCancelledRanges>>,
    /// Monotonic counter bumped on every successful visibility advance. A reader
    /// snapshots it BEFORE querying, then parks only if it is unchanged — closing
    /// the lost-wakeup window between "query found nothing" and "park" without
    /// nesting the index lock under the wakeup mutex.
    visibility_epoch: AtomicU64,
    /// Edge-trigger for readers blocked waiting for new visible entries. The mutex
    /// guards nothing but the condvar handshake: a publisher bumps `visibility_epoch`
    /// then `notify_all` under this mutex, so it cannot notify until a check-then-park
    /// reader has actually parked (the standard condvar discipline). Replaces a
    /// 1 ms poll-sleep spin in the cursor pull path.
    visibility_wakeup: (Mutex<()>, Condvar),
}

#[derive(Clone, Debug)]
pub(crate) struct VisibilitySnapshot {
    published: Arc<PublishedVisibility>,
    cancelled_ranges: Arc<CancelledRanges>,
    lane_cancelled_ranges: Arc<LaneCancelledRanges>,
}

impl VisibilitySnapshot {
    pub(crate) fn is_visible(&self, sequence: u64) -> bool {
        if sequence >= self.published.visible {
            return false;
        }
        !range_contains(&self.cancelled_ranges, sequence)
    }

    pub(crate) fn is_visible_on_lane(&self, sequence: u64, lane: u32) -> bool {
        let visible = self.published.lane_visible.get(&lane).copied().unwrap_or(0);
        if sequence >= visible {
            return false;
        }
        if range_contains(&self.cancelled_ranges, sequence) {
            return false;
        }
        !self
            .lane_cancelled_ranges
            .get(&lane)
            .is_some_and(|ranges| range_contains(ranges, sequence))
    }

    pub(crate) fn visible_upper_bound(&self) -> u64 {
        self.published.visible
    }
}

fn range_contains(ranges: &[(u64, u64)], sequence: u64) -> bool {
    ranges
        .iter()
        .any(|(start, end)| sequence >= *start && sequence < *end)
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
            published: RwLock::new(Arc::new(PublishedVisibility {
                visible: 0,
                lane_visible: BTreeMap::new(),
            })),
            active_fence: AtomicU64::new(0),
            active_fence_start: AtomicU64::new(u64::MAX),
            active_fence_end: AtomicU64::new(0),
            next_fence_token: AtomicU64::new(1),
            cancelled_ranges: RwLock::new(Arc::new(Vec::new())),
            lane_cancelled_ranges: RwLock::new(Arc::new(BTreeMap::new())),
            visibility_epoch: AtomicU64::new(0),
            visibility_wakeup: (Mutex::new(()), Condvar::new()),
        }
    }

    /// The current visibility epoch. A reader snapshots this BEFORE a query and
    /// passes it to [`Self::park_for_visibility_change`] so a publish racing the
    /// query is never missed.
    pub(crate) fn visibility_epoch(&self) -> u64 {
        self.visibility_epoch.load(Ordering::Acquire)
    }

    /// Signal every parked reader that visibility advanced. Bumps the epoch FIRST
    /// (so a reader re-checking the epoch under the wakeup mutex sees the change),
    /// then notifies under the mutex (so the notify cannot precede a concurrent
    /// check-then-park). Called only AFTER the `published` write guard is released,
    /// so the wakeup mutex never nests under the visibility lock.
    fn signal_visibility_change(&self) {
        self.visibility_epoch.fetch_add(1, Ordering::Release);
        let _guard = self.visibility_wakeup.0.lock();
        self.visibility_wakeup.1.notify_all();
    }

    /// Park until visibility advances past the caller's snapshot epoch, or `timeout`
    /// elapses (the deadline safety net — a missed wakeup degrades to the caller's
    /// timeout, never a hang). If a publish already advanced the epoch since the
    /// snapshot, returns immediately without parking (lost-wakeup guard).
    pub(crate) fn park_for_visibility_change(&self, since_epoch: u64, timeout: Duration) {
        let mut guard = self.visibility_wakeup.0.lock();
        if self.visibility_epoch.load(Ordering::Acquire) == since_epoch {
            let _ = self.visibility_wakeup.1.wait_for(&mut guard, timeout);
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
        {
            let allocated = self.allocated.load(Ordering::Acquire);
            let mut published = self.published.write();
            let current = published.as_ref();
            let visible = current.visible;
            if up_to > allocated || up_to < visible {
                return Err(crate::store::StoreError::SequenceGateViolation {
                    operation,
                    requested: up_to,
                    allocated,
                    visible,
                });
            }
            *published = Arc::new(PublishedVisibility {
                visible: up_to,
                lane_visible: current.lane_visible.clone(),
            });
        }
        // Wake any cursor parked on the visibility edge (after the write guard drops).
        self.signal_visibility_change();
        Ok(())
    }

    pub(crate) fn publish_on_lanes(
        &self,
        global_up_to: u64,
        lanes: impl IntoIterator<Item = (u32, u64)>,
        operation: &'static str,
    ) -> Result<(), crate::store::StoreError> {
        let allocated = self.allocated.load(Ordering::Acquire);
        let lanes: Vec<(u32, u64)> = lanes.into_iter().collect();
        {
            let mut published = self.published.write();
            let current = published.as_ref();
            if global_up_to > allocated || global_up_to < current.visible {
                return Err(crate::store::StoreError::SequenceGateViolation {
                    operation,
                    requested: global_up_to,
                    allocated,
                    visible: current.visible,
                });
            }
            for (lane, up_to) in lanes.iter().copied() {
                if up_to > allocated {
                    return Err(crate::store::StoreError::SequenceGateViolation {
                        operation,
                        requested: up_to,
                        allocated,
                        visible: current.lane_visible.get(&lane).copied().unwrap_or(0),
                    });
                }
                let current_lane = current.lane_visible.get(&lane).copied().unwrap_or(0);
                if up_to < current_lane {
                    return Err(crate::store::StoreError::SequenceGateViolation {
                        operation,
                        requested: up_to,
                        allocated,
                        visible: current_lane,
                    });
                }
            }
            let mut lane_visible = current.lane_visible.clone();
            for (lane, up_to) in lanes {
                lane_visible.insert(lane, up_to);
            }
            *published = Arc::new(PublishedVisibility {
                visible: global_up_to,
                lane_visible,
            });
        }
        // Wake any cursor parked on the visibility edge (after the write guard drops).
        self.signal_visibility_change();
        Ok(())
    }

    /// Current visibility watermark (exclusive upper bound).
    pub(crate) fn visible(&self) -> u64 {
        self.published.read().visible
    }

    pub(crate) fn lane_visible_snapshot(&self) -> BTreeMap<u32, u64> {
        self.published.read().lane_visible.clone()
    }

    pub(crate) fn restore_lane_visible(&self, lanes: BTreeMap<u32, u64>) {
        let mut published = self.published.write();
        let current = published.as_ref();
        *published = Arc::new(PublishedVisibility {
            visible: current.visible,
            lane_visible: lanes,
        });
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
        *self.published.write() = Arc::new(PublishedVisibility {
            visible: 0,
            lane_visible: BTreeMap::new(),
        });
        self.active_fence.store(0, Ordering::Release);
        self.active_fence_start.store(u64::MAX, Ordering::Release);
        self.active_fence_end.store(0, Ordering::Release);
        self.next_fence_token.store(1, Ordering::Release);
        *self.cancelled_ranges.write() = Arc::new(Vec::new());
        *self.lane_cancelled_ranges.write() = Arc::new(BTreeMap::new());
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

    pub(crate) fn active_fence_range(&self) -> Option<(u64, u64)> {
        if self.active_fence.load(Ordering::Acquire) == 0 {
            return None;
        }
        let start = self.active_fence_start.load(Ordering::Acquire);
        let end = self.active_fence_end.load(Ordering::Acquire);
        (start != u64::MAX && start < end).then_some((start, end))
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

    pub(crate) fn finish_fence_on_lanes(
        &self,
        token: u64,
        publish_to: Option<u64>,
        lanes: impl IntoIterator<Item = (u32, u64)>,
    ) -> Result<(), crate::store::StoreError> {
        if self.active_fence.load(Ordering::Acquire) != token {
            return Err(crate::store::StoreError::VisibilityFenceNotActive);
        }
        if let Some(up_to) = publish_to {
            self.publish_on_lanes(up_to, lanes, "finish_visibility_fence")?;
        }
        self.active_fence.store(0, Ordering::Release);
        self.active_fence_start.store(u64::MAX, Ordering::Release);
        self.active_fence_end.store(0, Ordering::Release);
        Ok(())
    }

    pub(crate) fn cancel_fence(
        &self,
        token: u64,
        lane_ranges: LaneCancelledRanges,
    ) -> Result<(), crate::store::StoreError> {
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
        if !lane_ranges.is_empty() {
            let mut guard = self.lane_cancelled_ranges.write();
            let mut ranges_by_lane = (**guard).clone();
            for (lane, ranges) in lane_ranges {
                let lane_ranges = ranges_by_lane.entry(lane).or_default();
                for (start, end) in ranges {
                    Self::insert_cancelled_range(lane_ranges, start, end);
                }
            }
            *guard = Arc::new(ranges_by_lane);
        }
        self.active_fence.store(0, Ordering::Release);
        self.active_fence_start.store(u64::MAX, Ordering::Release);
        self.active_fence_end.store(0, Ordering::Release);
        Ok(())
    }

    pub(crate) fn snapshot(&self) -> VisibilitySnapshot {
        VisibilitySnapshot {
            published: Arc::clone(&self.published.read()),
            cancelled_ranges: Arc::clone(&self.cancelled_ranges.read()),
            lane_cancelled_ranges: Arc::clone(&self.lane_cancelled_ranges.read()),
        }
    }

    pub(crate) fn cancelled_ranges_snapshot(&self) -> CancelledVisibilityRanges {
        CancelledVisibilityRanges {
            global: self.cancelled_ranges.read().as_ref().clone(),
            lanes: self.lane_cancelled_ranges.read().as_ref().clone(),
        }
    }

    pub(crate) fn restore_cancelled_ranges(&self, ranges: CancelledVisibilityRanges) {
        let mut built = Vec::new();
        for (start, end) in ranges.global {
            Self::insert_cancelled_range(&mut built, start, end);
        }
        *self.cancelled_ranges.write() = Arc::new(built);

        let mut built_lanes = BTreeMap::new();
        for (lane, ranges) in ranges.lanes {
            let lane_ranges = built_lanes.entry(lane).or_insert_with(Vec::new);
            for (start, end) in ranges {
                Self::insert_cancelled_range(lane_ranges, start, end);
            }
        }
        *self.lane_cancelled_ranges.write() = Arc::new(built_lanes);
    }
}

#[cfg(test)]
mod tests {
    use super::SequenceGate;
    use std::collections::BTreeMap;

    #[test]
    fn publish_on_lanes_preserves_restored_lane_visibility() {
        let gate = SequenceGate::new();
        gate.reserve(16);
        gate.restore_lane_visible(BTreeMap::from([(7, 4)]));

        gate.publish_on_lanes(5, [(1, 3)], "test_publish_on_lanes")
            .expect("PROPERTY: valid publish should advance visibility");

        let lanes = gate.lane_visible_snapshot();
        assert_eq!(
            lanes.get(&7).copied(),
            Some(4),
            "PROPERTY: publishing one lane must not drop lane visibility restored by compaction/bootstrap"
        );
        assert_eq!(
            lanes.get(&1).copied(),
            Some(3),
            "PROPERTY: publishing one lane must install that lane's visibility bound"
        );
    }

    #[test]
    fn restore_lane_visibility_preserves_global_visibility() {
        let gate = SequenceGate::new();
        gate.reserve(16);
        gate.publish(6, "test_publish")
            .expect("PROPERTY: valid publish should advance global visibility");

        gate.restore_lane_visible(BTreeMap::from([(2, 5)]));

        assert_eq!(
            gate.visible(),
            6,
            "PROPERTY: restoring lane visibility must not regress or drop global visibility"
        );
        assert_eq!(
            gate.lane_visible_snapshot().get(&2).copied(),
            Some(5),
            "PROPERTY: restoring lane visibility must install the restored lane bound"
        );
    }

    // The wakeup proof for the cursor's poll-spin replacement: a reader parked on
    // `park_for_visibility_change` is woken by a `publish` BEFORE its (generous)
    // timeout — proving the edge-trigger fires, not the deadline fallback. Mirrors
    // the sanctioned watermark condvar proof (writer.rs::dangerous_notify_all_*).
    #[test]
    fn park_wakes_on_publish_before_timeout() {
        use std::sync::mpsc;
        use std::sync::Arc;
        use std::time::Duration;

        let gate = Arc::new(SequenceGate::new());
        gate.reserve(8);

        let waiter_gate = Arc::clone(&gate);
        let (ready_tx, ready_rx) = mpsc::channel();
        let (woke_tx, woke_rx) = mpsc::channel();
        let waiter = std::thread::Builder::new()
            .name("visibility-park-proof".to_string())
            .spawn(move || {
                // Snapshot the epoch, signal readiness, THEN park. A publish that
                // races between the snapshot and the park is caught by the epoch
                // guard (park returns immediately) — either way the wake is prompt.
                let epoch = waiter_gate.visibility_epoch();
                ready_tx.send(()).expect("signal waiter readiness");
                waiter_gate.park_for_visibility_change(epoch, Duration::from_secs(5));
                woke_tx.send(()).expect("signal waiter woke");
            })
            .expect("spawn park waiter");

        ready_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("waiter snapshotted its epoch and is about to park");
        gate.publish(1, "park-proof")
            .expect("publish advances visibility");

        // If the edge-trigger works, the waiter wakes in ~ms. If it were broken, the
        // waiter would stay parked until its 5s timeout, so a 2s wait distinguishes a
        // real wakeup from the deadline fallback.
        woke_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("PROPERTY: publish must wake a parked reader before its timeout");
        waiter.join().expect("park waiter joins");
    }
}
