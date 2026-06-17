use crate::store::config::duration_micros;
use crate::store::stats::{FrontierView, HlcPoint, WatermarkKind, WatermarkSnapshot};
use crate::store::{Clock, StoreError, SystemClock};
use parking_lot::{Condvar, Mutex, MutexGuard};
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
            if watermark.current(guard.snapshot()) >= point {
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
        Self {
            accepted_hlc: point,
            written_hlc: point,
            durable_hlc: point,
            visible_hlc: point,
            applied_hlc: point,
            emitted_hlc: point,
            pending_write_start_mono_ns: None,
            clock,
        }
    }

    pub(crate) fn reset_to_bootstrap(&mut self, point: HlcPoint) {
        let clock = Arc::clone(&self.clock);
        *self = Self::for_bootstrap(point, clock);
    }

    pub(crate) fn advance_accepted(&mut self, point: HlcPoint) {
        if point > self.accepted_hlc {
            self.accepted_hlc = point;
            if self.pending_write_start_mono_ns.is_none() {
                self.pending_write_start_mono_ns = Some(self.clock.now_mono_ns());
            }
        }
    }

    pub(crate) fn advance_written(&mut self, point: HlcPoint) {
        self.written_hlc = self.written_hlc.max(point);
    }

    pub(crate) fn advance_durable(&mut self, point: HlcPoint) {
        self.durable_hlc = self.durable_hlc.max(point);
        if self.durable_hlc == self.accepted_hlc {
            self.pending_write_start_mono_ns = None;
        }
    }

    pub(crate) fn advance_durable_to_accepted(&mut self) {
        self.advance_durable(self.accepted_hlc);
    }

    pub(crate) fn advance_visible(&mut self, point: HlcPoint) {
        self.visible_hlc = self.visible_hlc.max(point);
    }

    pub(crate) fn advance_emitted(&mut self, point: HlcPoint) {
        self.emitted_hlc = self.emitted_hlc.max(point);
    }

    pub(crate) fn advance_visible_and_emitted(&mut self, point: HlcPoint) {
        self.advance_visible(point);
        self.advance_emitted(point);
    }

    pub(crate) fn set_applied(&mut self, point: HlcPoint) {
        self.applied_hlc = point;
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

    pub(crate) fn snapshot_view(&self) -> FrontierView {
        debug_assert!(
            self.accepted_hlc >= self.written_hlc,
            "accepted must be >= written: {:?} vs {:?}",
            self.accepted_hlc,
            self.written_hlc
        );
        debug_assert!(
            self.written_hlc >= self.durable_hlc,
            "written must be >= durable: {:?} vs {:?}",
            self.written_hlc,
            self.durable_hlc
        );
        debug_assert!(
            self.accepted_hlc >= self.visible_hlc,
            "accepted must be >= visible: {:?} vs {:?}",
            self.accepted_hlc,
            self.visible_hlc
        );
        debug_assert!(
            self.visible_hlc >= self.applied_hlc,
            "visible must be >= applied: {:?} vs {:?}",
            self.visible_hlc,
            self.applied_hlc
        );
        debug_assert!(
            self.emitted_hlc >= self.visible_hlc,
            "emitted must be >= visible: {:?} vs {:?}",
            self.emitted_hlc,
            self.visible_hlc
        );

        FrontierView {
            accepted_hlc: self.accepted_hlc,
            written_hlc: self.written_hlc,
            durable_hlc: self.durable_hlc,
            visible_hlc: self.visible_hlc,
            applied_hlc: self.applied_hlc,
            emitted_hlc: self.emitted_hlc,
            visible_minus_durable_seq: (self.visible_hlc.global_sequence as i64)
                - (self.durable_hlc.global_sequence as i64),
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
