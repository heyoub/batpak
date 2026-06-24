//! Deterministic-simulation determinism witness (GAUNT-SIM-2c).
//!
//! Runs the seeded simulation workload twice from the same seed and asserts the
//! op-trace digests match. Gated on `dangerous-test-hooks`; the simulation
//! runtime is compiled out of a default build. Replay a specific run with
//! `BATPAK_SEED=N cargo nextest run -p batpak --features dangerous-test-hooks
//! -E 'test(sim_is_deterministic)'`.
#![cfg(feature = "dangerous-test-hooks")]

/// The same seed must produce a byte-identical op-trace digest across two
/// independent runs of the seeded workload — the core determinism contract of
/// the simulation runtime.
#[test]
fn sim_is_deterministic() -> Result<(), String> {
    // Default seed is fixed; BATPAK_SEED=N overrides it for replay.
    let seed = batpak::__sim::replay_seed(0xDEAD_BEEF);
    let steps = 256;

    // A tripped invariant returns Err here and fails the test cleanly (no
    // panic!), surfacing the seed-tagged violation for BATPAK_SEED replay.
    let first = batpak::__sim::run_seeded_workload(seed, steps)?;
    let second = batpak::__sim::run_seeded_workload(seed, steps)?;

    assert_eq!(
        first, second,
        "PROPERTY: the seeded simulation workload is deterministic — identical \
         seed (0x{seed:X}) must yield identical op-trace digests (replay with \
         BATPAK_SEED={seed})"
    );
    Ok(())
}

/// The canonical deterministic `Clock` is reachable from the public surface
/// (`batpak::store::SimClock`) and behaves as a logical, non-regressing clock:
/// two clocks advanced by the same deltas observe identical timestamps, and
/// negative deltas are clamped. This is the seam downstream simulators (e.g.
/// `bvisor`) construct instead of re-implementing the `Clock` trait.
#[test]
fn sim_clock_is_public_and_deterministic() {
    use batpak::store::{Clock, SimClock};

    let a = SimClock::new();
    let b = SimClock::default();

    // Same construction → same starting timestamp.
    assert_eq!(
        a.now_us(),
        b.now_us(),
        "PROPERTY: two freshly constructed SimClocks start at the same logical epoch"
    );

    // Advancing both by identical deltas keeps them in lockstep — the
    // determinism property that makes UUIDv7 wall bits / freshness replayable.
    let returned = a.advance_us(250);
    b.advance_us(250);
    assert_eq!(
        a.now_us(),
        b.now_us(),
        "PROPERTY: identical advances yield identical logical time"
    );
    assert_eq!(
        returned,
        a.now_us(),
        "PROPERTY: advance_us returns the new logical now_us"
    );
    assert_eq!(
        a.now_mono_ns(),
        250 * 1_000,
        "PROPERTY: the monotonic stream tracks the advanced microseconds"
    );

    // Negative deltas are clamped — the clock never regresses.
    let before = a.now_us();
    let held = a.advance_us(-1_000);
    assert_eq!(
        held, before,
        "PROPERTY: a negative advance is clamped and the clock never regresses"
    );
}
