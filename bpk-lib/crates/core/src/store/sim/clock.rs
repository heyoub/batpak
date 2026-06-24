//! Logical simulation clock.
//!
//! [`SimClock`] implements [`Clock`] but reports *logical* time: the value the
//! scheduler advances explicitly, never the host wall clock. Two simulation
//! runs that advance the clock by the same logical deltas observe identical
//! timestamps, which is what makes UUIDv7 wall bits, receipts, and freshness
//! comparisons deterministic under simulation.
//!
//! Time starts at a fixed logical epoch (`EPOCH_US`) so digests do not depend
//! on when the test ran. The clock never regresses: [`SimClock::advance_us`]
//! only moves it forward, mirroring the production
//! [`super::super::platform::clock::MonotonicClock`] guarantee.

use crate::store::platform::clock::Clock;
use std::sync::atomic::{AtomicI64, Ordering};

/// Fixed logical start, microseconds since Unix epoch. Chosen as a round,
/// far-from-zero value (`2_000_000_000_000_000` µs ≈ year 2033) so wall-bit
/// derived identity never collides with the zero/uninitialized sentinel.
const EPOCH_US: i64 = 2_000_000_000_000_000;

/// Deterministic logical [`Clock`] for the simulator.
///
/// Reports *logical* time: the value the scheduler advances explicitly, never
/// the host wall clock. Two runs that advance by the same logical deltas
/// observe identical timestamps — the basis for deterministic UUIDv7 wall bits,
/// receipts, and freshness comparisons under simulation.
///
/// Shared state lives in atomics so every backend that holds the clock (writer
/// body, reader, freshness check) observes the same logical timeline. Time
/// starts at a fixed logical epoch and never regresses. This is the canonical
/// deterministic `Clock`; downstream simulators (e.g. `bvisor`) construct it
/// rather than re-implementing the trait.
#[derive(Debug)]
pub struct SimClock {
    /// Current logical microseconds since Unix epoch (non-decreasing).
    now_us: AtomicI64,
    /// Logical monotonic nanoseconds since simulation start (non-decreasing).
    mono_ns: AtomicI64,
}

impl SimClock {
    /// Create a clock parked at the fixed logical epoch.
    #[must_use]
    pub fn new() -> Self {
        Self {
            now_us: AtomicI64::new(EPOCH_US),
            mono_ns: AtomicI64::new(0),
        }
    }

    /// Advance logical time by `delta_us` microseconds (and the monotonic
    /// stream by the equivalent nanoseconds). Saturating; never regresses
    /// (negative deltas are clamped to zero). Returns the new `now_us` so
    /// callers can record it in an op-trace.
    pub fn advance_us(&self, delta_us: i64) -> i64 {
        let delta = delta_us.max(0);
        self.mono_ns
            .fetch_add(delta.saturating_mul(1000), Ordering::AcqRel);
        // fetch_add then return the post-increment value.
        self.now_us.fetch_add(delta, Ordering::AcqRel) + delta
    }
}

impl Default for SimClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SimClock {
    fn now_us(&self) -> i64 {
        self.now_us.load(Ordering::Acquire)
    }

    fn now_wall_ns(&self) -> i64 {
        self.now_us.load(Ordering::Acquire).saturating_mul(1000)
    }

    fn now_mono_ns(&self) -> i64 {
        self.mono_ns.load(Ordering::Acquire)
    }

    fn process_boot_ns(&self) -> u64 {
        // A fixed, non-zero logical boot marker (ASCII "SIMCLOCK"). Stable
        // across a run so cached monotonic comparisons stay consistent;
        // distinct from the production wall-derived marker so a sim value is
        // never mistaken for a real one.
        0x53_49_4D_43_4C_4F_43_4B
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_starts_at_epoch_and_only_advances() {
        let c = SimClock::new();
        assert_eq!(c.now_us(), EPOCH_US, "PROPERTY: clock parks at fixed epoch");
        let after = c.advance_us(250);
        assert_eq!(
            after,
            EPOCH_US + 250,
            "PROPERTY: advance returns new now_us"
        );
        assert_eq!(c.now_us(), EPOCH_US + 250);
        // Negative deltas are clamped to zero (never regress).
        let held = c.advance_us(-1000);
        assert_eq!(held, EPOCH_US + 250, "PROPERTY: clock never regresses");
    }

    #[test]
    fn monotonic_stream_tracks_microseconds() {
        let c = SimClock::new();
        c.advance_us(3);
        assert_eq!(
            c.now_mono_ns(),
            3000,
            "PROPERTY: logical monotonic ns tracks logical microseconds"
        );
        assert_eq!(c.now_wall_ns(), c.now_us() * 1000);
    }
}
