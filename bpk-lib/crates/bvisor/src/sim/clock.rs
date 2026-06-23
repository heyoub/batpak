//! A bvisor-local deterministic [`Clock`] for the simulated execution path.
//!
//! FORCED duplicate, not an oversight: batpak core's `SimClock`
//! (`store/sim/clock.rs`) is `pub(crate)` and unreachable from bvisor — but the
//! `Clock` TRAIT (`batpak::store::Clock`) IS public. So bvisor implements the public
//! trait here to get deterministic, replayable simulated time without reaching into
//! core internals. Semantics mirror core's `SimClock`: time only moves when the
//! harness advances it, so two runs that advance by the same deltas observe identical
//! timings.
//!
//! TODO(core-export): promote core `SimClock` (or a deterministic `Clock`
//! constructor) to `pub` and delete this duplicate.

use batpak::store::Clock;
use std::sync::atomic::{AtomicI64, Ordering};

/// A monotonic, manually-advanced clock. Logical time starts at zero and never
/// regresses; [`SimExecClock::advance_us`] is the sole way it moves. Used to witness
/// the wall-time budget dimension deterministically from the public [`Clock`] seam.
#[derive(Debug, Default)]
pub(crate) struct SimExecClock {
    /// Monotonic nanoseconds since simulation start (non-decreasing).
    mono_ns: AtomicI64,
}

impl SimExecClock {
    /// A clock at logical time zero.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Advance simulated time by `delta_us` microseconds (saturating, non-decreasing).
    pub(crate) fn advance_us(&self, delta_us: u64) {
        let delta_ns = i64::try_from(delta_us.saturating_mul(1_000)).unwrap_or(i64::MAX);
        self.mono_ns.fetch_add(delta_ns, Ordering::SeqCst);
    }
}

impl Clock for SimExecClock {
    fn now_us(&self) -> i64 {
        self.mono_ns.load(Ordering::SeqCst) / 1_000
    }

    fn now_wall_ns(&self) -> i64 {
        self.mono_ns.load(Ordering::SeqCst)
    }

    fn now_mono_ns(&self) -> i64 {
        self.mono_ns.load(Ordering::SeqCst)
    }

    fn process_boot_ns(&self) -> u64 {
        0
    }
}

#[cfg(test)]
mod clock_tests {
    use super::SimExecClock;
    use batpak::store::Clock;

    #[test]
    fn advance_moves_monotonic_time_deterministically() {
        let clock = SimExecClock::new();
        assert_eq!(clock.now_mono_ns(), 0);
        let start = clock.now_mono_ns();
        clock.advance_us(40);
        let elapsed_us = (clock.now_mono_ns() - start) / 1_000;
        assert_eq!(elapsed_us, 40, "the delta equals the advance — replayable");
    }
}
