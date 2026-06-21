use crate::store::config::duration_micros;
use crate::store::stats::{
    FrontierView, HlcPoint, LaneFrontierView, WatermarkKind, WatermarkSnapshot,
};
use crate::store::{Clock, StoreError, SystemClock};
use parking_lot::{Condvar, Mutex, MutexGuard};
use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub(crate) struct WatermarkAdvanceHandle {
    state: Arc<Mutex<WatermarkState>>,
    cv: Arc<Condvar>,
    poison: Arc<AtomicBool>,
}

pub(crate) struct WatermarkGuard<'a> {
    guard: MutexGuard<'a, WatermarkState>,
    cv: &'a Condvar,
}

#[derive(Clone, Copy, Debug)]
struct LaneWatermarks {
    accepted_hlc: HlcPoint,
    written_hlc: HlcPoint,
    durable_hlc: HlcPoint,
    visible_hlc: HlcPoint,
    applied_hlc: HlcPoint,
    emitted_hlc: HlcPoint,
}

impl Default for LaneWatermarks {
    fn default() -> Self {
        Self {
            accepted_hlc: HlcPoint::ORIGIN,
            written_hlc: HlcPoint::ORIGIN,
            durable_hlc: HlcPoint::ORIGIN,
            visible_hlc: HlcPoint::ORIGIN,
            applied_hlc: HlcPoint::ORIGIN,
            emitted_hlc: HlcPoint::ORIGIN,
        }
    }
}

impl LaneWatermarks {
    fn for_bootstrap(point: HlcPoint) -> Self {
        Self {
            accepted_hlc: point,
            written_hlc: point,
            durable_hlc: point,
            visible_hlc: point,
            applied_hlc: point,
            emitted_hlc: point,
        }
    }

    fn for_bootstrap_split(durable_point: HlcPoint, visible_point: HlcPoint) -> Self {
        Self {
            accepted_hlc: durable_point,
            written_hlc: durable_point,
            durable_hlc: durable_point,
            visible_hlc: visible_point,
            applied_hlc: visible_point,
            emitted_hlc: visible_point,
        }
    }

    fn current(&self, watermark: WatermarkKind) -> HlcPoint {
        match watermark {
            WatermarkKind::Accepted => self.accepted_hlc,
            WatermarkKind::Written => self.written_hlc,
            WatermarkKind::Durable => self.durable_hlc,
            WatermarkKind::Applied => self.applied_hlc,
            WatermarkKind::Visible => self.visible_hlc,
            WatermarkKind::Emitted => self.emitted_hlc,
        }
    }

    fn view(self, lane: u32) -> LaneFrontierView {
        LaneFrontierView {
            lane,
            accepted_hlc: self.accepted_hlc,
            written_hlc: self.written_hlc,
            durable_hlc: self.durable_hlc,
            visible_hlc: self.visible_hlc,
            applied_hlc: self.applied_hlc,
            emitted_hlc: self.emitted_hlc,
            visible_minus_durable_seq: (self.visible_hlc.global_sequence as i64)
                - (self.durable_hlc.global_sequence as i64),
        }
    }
}

impl WatermarkAdvanceHandle {
    fn new(state: WatermarkState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
            cv: Arc::new(Condvar::new()),
            poison: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn lock(&self) -> WatermarkGuard<'_> {
        WatermarkGuard {
            guard: self.state.lock(),
            cv: &self.cv,
        }
    }

    pub(crate) fn mark_writer_crashed(&self) {
        // Hold the state lock across the poison store + notify. A waiter that has
        // observed `poison == false` but not yet parked on the condvar would
        // otherwise miss this wake-up (lost-wakeup), and only learn the writer
        // died when its full timeout elapses. Taking the lock serializes against
        // the wait-loop's check-then-park, matching the discipline the normal
        // watermark-advance/notify path already uses (audit R7).
        //
        // Safe from deadlock: the writer thread unwinds (dropping any held
        // guard, which parking_lot releases without poisoning) before the panic
        // handler calls this, so the lock is free here.
        let _guard = self.state.lock();
        self.poison.store(true, Ordering::Release);
        self.cv.notify_all();
    }

    pub(crate) fn wait_for_durable(
        &self,
        point: HlcPoint,
        timeout: Duration,
    ) -> Result<(), StoreError> {
        self.wait_for_watermark(WatermarkKind::Durable, point, timeout)
    }

    pub(crate) fn wait_for_accepted(
        &self,
        point: HlcPoint,
        timeout: Duration,
    ) -> Result<(), StoreError> {
        self.wait_for_watermark(WatermarkKind::Accepted, point, timeout)
    }

    pub(crate) fn wait_for_written(
        &self,
        point: HlcPoint,
        timeout: Duration,
    ) -> Result<(), StoreError> {
        self.wait_for_watermark(WatermarkKind::Written, point, timeout)
    }

    pub(crate) fn wait_for_applied(
        &self,
        point: HlcPoint,
        timeout: Duration,
    ) -> Result<(), StoreError> {
        self.wait_for_watermark(WatermarkKind::Applied, point, timeout)
    }

    pub(crate) fn wait_for_visible(
        &self,
        point: HlcPoint,
        timeout: Duration,
    ) -> Result<(), StoreError> {
        self.wait_for_watermark(WatermarkKind::Visible, point, timeout)
    }

    pub(crate) fn wait_for_emitted(
        &self,
        point: HlcPoint,
        timeout: Duration,
    ) -> Result<(), StoreError> {
        self.wait_for_watermark(WatermarkKind::Emitted, point, timeout)
    }

    pub(crate) fn wait_for_accepted_on_lane(
        &self,
        lane: u32,
        point: HlcPoint,
        timeout: Duration,
    ) -> Result<(), StoreError> {
        self.wait_for_watermark_on_lane(WatermarkKind::Accepted, lane, point, timeout)
    }

    pub(crate) fn wait_for_written_on_lane(
        &self,
        lane: u32,
        point: HlcPoint,
        timeout: Duration,
    ) -> Result<(), StoreError> {
        self.wait_for_watermark_on_lane(WatermarkKind::Written, lane, point, timeout)
    }

    pub(crate) fn wait_for_durable_on_lane(
        &self,
        lane: u32,
        point: HlcPoint,
        timeout: Duration,
    ) -> Result<(), StoreError> {
        self.wait_for_watermark_on_lane(WatermarkKind::Durable, lane, point, timeout)
    }

    pub(crate) fn wait_for_applied_on_lane(
        &self,
        lane: u32,
        point: HlcPoint,
        timeout: Duration,
    ) -> Result<(), StoreError> {
        self.wait_for_watermark_on_lane(WatermarkKind::Applied, lane, point, timeout)
    }

    pub(crate) fn wait_for_visible_on_lane(
        &self,
        lane: u32,
        point: HlcPoint,
        timeout: Duration,
    ) -> Result<(), StoreError> {
        self.wait_for_watermark_on_lane(WatermarkKind::Visible, lane, point, timeout)
    }

    pub(crate) fn wait_for_emitted_on_lane(
        &self,
        lane: u32,
        point: HlcPoint,
        timeout: Duration,
    ) -> Result<(), StoreError> {
        self.wait_for_watermark_on_lane(WatermarkKind::Emitted, lane, point, timeout)
    }

    fn wait_for_watermark(
        &self,
        watermark: WatermarkKind,
        point: HlcPoint,
        timeout: Duration,
    ) -> Result<(), StoreError> {
        let started = Instant::now();
        let mut guard = self.state.lock();
        loop {
            if self.poison.load(Ordering::Acquire) {
                return Err(StoreError::WriterCrashed);
            }
            if watermark.current(guard.snapshot()).covers_sequence(point) {
                tracing::trace!(
                    target: "batpak::frontier_wait",
                    ?watermark,
                    target = ?point,
                    waited_us = duration_micros(started.elapsed()),
                    "frontier wait satisfied",
                );
                return Ok(());
            }

            let elapsed = started.elapsed();
            if elapsed >= timeout {
                tracing::trace!(
                    target: "batpak::frontier_wait",
                    ?watermark,
                    target = ?point,
                    waited_us = duration_micros(elapsed),
                    timed_out = true,
                    "frontier wait timed out",
                );
                return Err(StoreError::WaitTimeout {
                    watermark,
                    target: point,
                    waited_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                });
            }
            let remaining = timeout.saturating_sub(elapsed);
            if remaining.is_zero() {
                tracing::trace!(
                    target: "batpak::frontier_wait",
                    ?watermark,
                    target = ?point,
                    waited_us = duration_micros(elapsed),
                    timed_out = true,
                    "frontier wait timed out",
                );
                return Err(StoreError::WaitTimeout {
                    watermark,
                    target: point,
                    waited_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                });
            }

            let _wait_result = self.cv.wait_for(&mut guard, remaining);
        }
    }

    fn wait_for_watermark_on_lane(
        &self,
        watermark: WatermarkKind,
        lane: u32,
        point: HlcPoint,
        timeout: Duration,
    ) -> Result<(), StoreError> {
        let started = Instant::now();
        let mut guard = self.state.lock();
        loop {
            if self.poison.load(Ordering::Acquire) {
                return Err(StoreError::WriterCrashed);
            }
            if guard
                .lane_watermark(lane)
                .current(watermark)
                .covers_sequence(point)
            {
                tracing::trace!(
                    target: "batpak::frontier_wait",
                    ?watermark,
                    lane,
                    target = ?point,
                    waited_us = duration_micros(started.elapsed()),
                    "lane frontier wait satisfied",
                );
                return Ok(());
            }

            let elapsed = started.elapsed();
            if elapsed >= timeout {
                tracing::trace!(
                    target: "batpak::frontier_wait",
                    ?watermark,
                    lane,
                    target = ?point,
                    waited_us = duration_micros(elapsed),
                    timed_out = true,
                    "lane frontier wait timed out",
                );
                return Err(StoreError::WaitTimeout {
                    watermark,
                    target: point,
                    waited_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                });
            }
            let remaining = timeout.saturating_sub(elapsed);
            if remaining.is_zero() {
                return Err(StoreError::WaitTimeout {
                    watermark,
                    target: point,
                    waited_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                });
            }

            let _wait_result = self.cv.wait_for(&mut guard, remaining);
        }
    }

    #[cfg(any(test, feature = "dangerous-test-hooks"))]
    pub(crate) fn dangerous_notify_all(&self) {
        self.cv.notify_all();
    }

    #[cfg(test)]
    pub(crate) fn dangerous_wait_for_notification(&self, timeout: Duration) -> bool {
        let mut guard = self.state.lock();
        self.cv.wait_for(&mut guard, timeout).timed_out()
    }
}

impl Deref for WatermarkGuard<'_> {
    type Target = WatermarkState;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl DerefMut for WatermarkGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

impl Drop for WatermarkGuard<'_> {
    fn drop(&mut self) {
        self.cv.notify_all();
    }
}

/// Internal mutable frontier state. All snapshots take this single mutex once.
pub(crate) struct WatermarkState {
    accepted_hlc: HlcPoint,
    written_hlc: HlcPoint,
    durable_hlc: HlcPoint,
    visible_hlc: HlcPoint,
    applied_hlc: HlcPoint,
    emitted_hlc: HlcPoint,
    lanes: BTreeMap<u32, LaneWatermarks>,
    pending_write_start_mono_ns: Option<i64>,
    clock: Arc<dyn Clock>,
}

impl Default for WatermarkState {
    fn default() -> Self {
        Self {
            accepted_hlc: HlcPoint::ORIGIN,
            written_hlc: HlcPoint::ORIGIN,
            durable_hlc: HlcPoint::ORIGIN,
            visible_hlc: HlcPoint::ORIGIN,
            applied_hlc: HlcPoint::ORIGIN,
            emitted_hlc: HlcPoint::ORIGIN,
            lanes: BTreeMap::new(),
            pending_write_start_mono_ns: None,
            clock: Arc::new(SystemClock::new()),
        }
    }
}

impl WatermarkState {
    pub(crate) fn handle(clock: Arc<dyn Clock>) -> WatermarkAdvanceHandle {
        WatermarkAdvanceHandle::new(Self::new(clock))
    }

    pub(crate) fn bootstrap_handle(
        point: HlcPoint,
        clock: Arc<dyn Clock>,
    ) -> WatermarkAdvanceHandle {
        WatermarkAdvanceHandle::new(Self::for_bootstrap(point, clock))
    }

    pub(crate) fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            ..Self::default()
        }
    }

    pub(crate) fn for_bootstrap(point: HlcPoint, clock: Arc<dyn Clock>) -> Self {
        let mut lanes = BTreeMap::new();
        lanes.insert(0, LaneWatermarks::for_bootstrap(point));
        Self {
            accepted_hlc: point,
            written_hlc: point,
            durable_hlc: point,
            visible_hlc: point,
            applied_hlc: point,
            emitted_hlc: point,
            lanes,
            pending_write_start_mono_ns: None,
            clock,
        }
    }

    pub(crate) fn reset_to_bootstrap_lanes(
        &mut self,
        global_durable_point: HlcPoint,
        global_visible_point: HlcPoint,
        // Physical durability is the global fsync point; lane durability is the
        // highest lane-local written/hidden point covered by that physical point.
        lane_durable_points: impl IntoIterator<Item = (u32, HlcPoint)>,
        // Visibility is a logical per-lane publish cursor over global_sequence.
        lane_visible_points: impl IntoIterator<Item = (u32, HlcPoint)>,
    ) {
        let mut lane_durable_points: BTreeMap<u32, HlcPoint> =
            lane_durable_points.into_iter().collect();
        let lane_visible_points: BTreeMap<u32, HlcPoint> =
            lane_visible_points.into_iter().collect();
        for (lane, point) in &lane_visible_points {
            let durable = lane_durable_points.entry(*lane).or_insert(HlcPoint::ORIGIN);
            *durable = (*durable).max_by_sequence(*point);
        }
        *self = Self {
            accepted_hlc: global_durable_point,
            written_hlc: global_durable_point,
            durable_hlc: global_durable_point,
            visible_hlc: global_visible_point,
            applied_hlc: global_visible_point,
            emitted_hlc: global_visible_point,
            lanes: BTreeMap::new(),
            pending_write_start_mono_ns: None,
            clock: Arc::clone(&self.clock),
        };
        for (lane, durable_point) in lane_durable_points {
            let visible_point = lane_visible_points
                .get(&lane)
                .copied()
                .unwrap_or(HlcPoint::ORIGIN);
            self.lanes.insert(
                lane,
                LaneWatermarks::for_bootstrap_split(durable_point, visible_point),
            );
        }
        if self.lanes.is_empty() {
            self.lanes
                .insert(0, LaneWatermarks::for_bootstrap(HlcPoint::ORIGIN));
        }
    }

    pub(crate) fn advance_accepted_on_lane(&mut self, lane: u32, point: HlcPoint) {
        if !self.accepted_hlc.covers_sequence(point) {
            self.accepted_hlc = point;
            if self.pending_write_start_mono_ns.is_none() {
                self.pending_write_start_mono_ns = Some(self.clock.now_mono_ns());
            }
        }
        let lane = self.lane_watermark_mut(lane);
        lane.accepted_hlc = lane.accepted_hlc.max_by_sequence(point);
    }

    pub(crate) fn advance_written_on_lane(&mut self, lane: u32, point: HlcPoint) {
        self.written_hlc = self.written_hlc.max_by_sequence(point);
        let lane = self.lane_watermark_mut(lane);
        lane.written_hlc = lane.written_hlc.max_by_sequence(point);
    }

    pub(crate) fn advance_durable(&mut self, point: HlcPoint) {
        self.durable_hlc = self.durable_hlc.max_by_sequence(point);
        self.advance_lane_durability_to_physical();
        if self.durable_hlc.covers_sequence(self.accepted_hlc) {
            self.pending_write_start_mono_ns = None;
        }
    }

    pub(crate) fn advance_durable_to_accepted(&mut self) {
        self.advance_durable(self.accepted_hlc);
    }

    pub(crate) fn advance_visible_on_lane(&mut self, lane: u32, point: HlcPoint) {
        self.visible_hlc = self.visible_hlc.max_by_sequence(point);
        let lane = self.lane_watermark_mut(lane);
        lane.visible_hlc = lane.visible_hlc.max_by_sequence(point);
    }

    pub(crate) fn advance_emitted_on_lane(&mut self, lane: u32, point: HlcPoint) {
        self.emitted_hlc = self.emitted_hlc.max_by_sequence(point);
        let lane = self.lane_watermark_mut(lane);
        lane.emitted_hlc = lane.emitted_hlc.max_by_sequence(point);
    }

    pub(crate) fn advance_visible_and_emitted(&mut self, point: HlcPoint) {
        self.visible_hlc = self.visible_hlc.max_by_sequence(point);
        self.emitted_hlc = self.emitted_hlc.max_by_sequence(point);
    }

    pub(crate) fn advance_visible_and_emitted_on_lane(&mut self, lane: u32, point: HlcPoint) {
        self.advance_visible_on_lane(lane, point);
        self.advance_emitted_on_lane(lane, point);
    }

    pub(crate) fn set_applied(&mut self, point: HlcPoint) {
        self.applied_hlc = point;
    }

    pub(crate) fn set_applied_on_lane(&mut self, lane: u32, point: HlcPoint) {
        let lane = self.lane_watermark_mut(lane);
        lane.applied_hlc = point;
    }

    pub(crate) fn snapshot(&self) -> WatermarkSnapshot {
        WatermarkSnapshot {
            accepted_hlc: self.accepted_hlc,
            written_hlc: self.written_hlc,
            durable_hlc: self.durable_hlc,
            visible_hlc: self.visible_hlc,
            applied_hlc: self.applied_hlc,
            emitted_hlc: self.emitted_hlc,
            oldest_pending_write_age_ms: self
                .pending_write_start_mono_ns
                .map(|start| elapsed_ms_since(self.clock.now_mono_ns(), start)),
        }
    }

    fn lane_watermark(&self, lane: u32) -> LaneWatermarks {
        self.lanes.get(&lane).copied().unwrap_or_default()
    }

    fn lane_watermark_mut(&mut self, lane: u32) -> &mut LaneWatermarks {
        self.lanes.entry(lane).or_default()
    }

    fn advance_lane_durability_to_physical(&mut self) {
        let physical = self.durable_hlc;
        for lane in self.lanes.values_mut() {
            if physical.covers_sequence(lane.written_hlc) {
                lane.durable_hlc = lane.durable_hlc.max_by_sequence(lane.written_hlc);
            }
        }
    }

    fn lane_views(&self) -> Vec<LaneFrontierView> {
        self.lanes
            .iter()
            .map(|(lane, watermarks)| watermarks.view(*lane))
            .collect()
    }

    pub(crate) fn snapshot_view(&self) -> FrontierView {
        debug_assert!(
            self.accepted_hlc.covers_sequence(self.written_hlc),
            "accepted must be >= written: {:?} vs {:?}",
            self.accepted_hlc,
            self.written_hlc
        );
        debug_assert!(
            self.written_hlc.covers_sequence(self.durable_hlc),
            "written must be >= durable: {:?} vs {:?}",
            self.written_hlc,
            self.durable_hlc
        );
        debug_assert!(
            self.accepted_hlc.covers_sequence(self.visible_hlc),
            "accepted must be >= visible: {:?} vs {:?}",
            self.accepted_hlc,
            self.visible_hlc
        );
        debug_assert!(
            self.visible_hlc.covers_sequence(self.applied_hlc),
            "visible must be >= applied: {:?} vs {:?}",
            self.visible_hlc,
            self.applied_hlc
        );
        debug_assert!(
            self.emitted_hlc.covers_sequence(self.visible_hlc),
            "emitted must be >= visible: {:?} vs {:?}",
            self.emitted_hlc,
            self.visible_hlc
        );
        for (lane, watermarks) in &self.lanes {
            debug_assert!(
                watermarks
                    .accepted_hlc
                    .covers_sequence(watermarks.written_hlc),
                "lane {lane} accepted must be >= written: {:?} vs {:?}",
                watermarks.accepted_hlc,
                watermarks.written_hlc
            );
            debug_assert!(
                watermarks
                    .written_hlc
                    .covers_sequence(watermarks.durable_hlc),
                "lane {lane} written must be >= durable: {:?} vs {:?}",
                watermarks.written_hlc,
                watermarks.durable_hlc
            );
            debug_assert!(
                self.durable_hlc.covers_sequence(watermarks.durable_hlc),
                "lane {lane} durable must not exceed physical durable: {:?} vs {:?}",
                watermarks.durable_hlc,
                self.durable_hlc
            );
            debug_assert!(
                watermarks
                    .accepted_hlc
                    .covers_sequence(watermarks.visible_hlc),
                "lane {lane} accepted must be >= visible: {:?} vs {:?}",
                watermarks.accepted_hlc,
                watermarks.visible_hlc
            );
            debug_assert!(
                watermarks
                    .visible_hlc
                    .covers_sequence(watermarks.applied_hlc),
                "lane {lane} visible must be >= applied: {:?} vs {:?}",
                watermarks.visible_hlc,
                watermarks.applied_hlc
            );
            debug_assert!(
                watermarks
                    .emitted_hlc
                    .covers_sequence(watermarks.visible_hlc),
                "lane {lane} emitted must be >= visible: {:?} vs {:?}",
                watermarks.emitted_hlc,
                watermarks.visible_hlc
            );
        }

        FrontierView {
            accepted_hlc: self.accepted_hlc,
            written_hlc: self.written_hlc,
            durable_hlc: self.durable_hlc,
            visible_hlc: self.visible_hlc,
            applied_hlc: self.applied_hlc,
            emitted_hlc: self.emitted_hlc,
            visible_minus_durable_seq: (self.visible_hlc.global_sequence as i64)
                - (self.durable_hlc.global_sequence as i64),
            lanes: self.lane_views(),
            oldest_pending_write_age_ms: self
                .pending_write_start_mono_ns
                .map(|start| elapsed_ms_since(self.clock.now_mono_ns(), start)),
        }
    }
}

pub(super) fn elapsed_ms_since(now_ns: i64, then_ns: i64) -> u64 {
    let elapsed_ns = now_ns.saturating_sub(then_ns).max(0);
    u64::try_from(elapsed_ns / 1_000_000).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "watermark_tests.rs"]
mod tests;
