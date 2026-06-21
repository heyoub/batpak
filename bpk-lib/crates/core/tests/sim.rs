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
