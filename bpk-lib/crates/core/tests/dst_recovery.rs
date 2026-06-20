// justifies: INV-DST-RECOVERY-LEGAL; the DST recovery composition uses expect/panic as the assertion style when a recovered state is illegal or nondeterministic, per crates/core/tests/dst_recovery.rs
#![allow(clippy::panic, clippy::unwrap_used)]
// The sim filesystem, crash hook, and __sim entry points live behind
// `dangerous-test-hooks`; without the feature the whole file is empty.
#![cfg(feature = "dangerous-test-hooks")]
//! GAUNTLET B2 — deterministic-simulation crash-recovery over the REAL Store.
//!
//! justifies: INV-DST-RECOVERY-LEGAL — a real `Store` opened over the
//! fault-injecting `SimFs` backend, driven through the real public API
//! (`append`/`append_batch`/`sync`), crashed at the durability boundary, and
//! reopened over the persisted (truncated) tree must recover a LEGAL state: a
//! prefix of the appended op-log (no invented/undead events) that contains every
//! acknowledged-durable commit, with an intact hash chain — and the SAME seed
//! must recover the SAME state (determinism). See `crates/core/src/store/sim/`.
//!
//! This is the genuine composition the gauntlet asks for: the simulation drives
//! the REAL `Store`, not the sim backends directly. `Store::open` performs its
//! segment create + fsync durability I/O through the `StoreFs` seam, the crash is
//! induced by abandoning the writer without a clean shutdown and truncating the
//! unsynced tail via `SimFs::crash`, and the reopen cold-starts over the real
//! truncated files.
//!
//! Requires `--features dangerous-test-hooks`. Replay a specific run with
//! `BATPAK_SEED=N cargo nextest run -p batpak --features dangerous-test-hooks
//! -E 'test(dst_recovery_is_legal_and_deterministic)'`.

/// GREEN (every-PR, dangerous-test-hooks lane): a real `Store` composed over
/// `SimFs`, crashed and reopened, must recover a LEGAL state, and the SAME seed
/// must recover IDENTICALLY (determinism).
///
/// RED fixture (`--cfg gauntlet_red_fixture`): asserts the recovered state lost
/// an acknowledged-durable commit (`recovered_visible < durable_acked`). That is
/// FALSE against the real recovery path (which never loses an honored-sync
/// commit), so the red half FAILS — proving the oracle actually detects a
/// lost-durable-commit / nondeterminism rather than passing vacuously.
#[test]
fn dst_recovery_is_legal_and_deterministic() {
    let seed = batpak::__sim::recovery_replay_seed(0x0B2_DEAD_BEEF);
    let steps = 96;

    // Two runs from the same seed must recover byte-identically. The outcome type
    // is named explicitly so its public surface is test-anchored.
    let first: batpak::__sim::RecoveryOutcomePublic =
        batpak::__sim::run_seeded_recovery(seed, steps).unwrap_or_else(|v| {
            panic!("DST recovery must be legal on the real recovery path: {v}")
        });
    let second: batpak::__sim::RecoveryOutcomePublic =
        batpak::__sim::run_seeded_recovery(seed, steps).unwrap_or_else(|v| {
            panic!("DST recovery must be legal on the real recovery path: {v}")
        });

    assert_eq!(
        first, second,
        "PROPERTY: identical seed (0x{seed:X}) must recover the identical state + digest \
         (replay with BATPAK_SEED={seed})"
    );

    // RED fixture: assert the ILLEGAL lost-durable-commit outcome. The real path
    // never loses an acknowledged-durable commit, so this assertion is false and
    // the red half FAILS under `--cfg gauntlet_red_fixture`.
    #[cfg(gauntlet_red_fixture)]
    assert!(
        first.recovered_visible < first.durable_acked,
        "RED FIXTURE: asserts the (illegal) lost-after-sync outcome; MUST fail because an \
         honored-sync commit is required to survive a crash"
    );

    // GREEN: the legality oracle inside run_seeded_recovery already fail-closes on
    // any illegal state (lost durable commit, undead event, broken hash chain,
    // non-canonical reopen). Here we additionally pin the no-loss inequality.
    #[cfg(not(gauntlet_red_fixture))]
    assert!(
        first.recovered_visible >= first.durable_acked,
        "SACRED RULE: every acknowledged-durable commit must survive the crash; recovered \
         {} visible < {} durable would be a lost-after-sync commit",
        first.recovered_visible,
        first.durable_acked
    );
}

/// Distinct seeds should (almost surely) diverge in the recovered digest — the
/// discriminating signal that the simulation actually varies with the seed
/// rather than collapsing to a fixed trace.
#[test]
fn dst_recovery_diverges_across_seeds() {
    let a = batpak::__sim::run_seeded_recovery(0x0B2_0001, 96).expect("legal recovery");
    let b = batpak::__sim::run_seeded_recovery(0x0B2_0002, 96).expect("legal recovery");
    assert_ne!(
        a.digest, b.digest,
        "PROPERTY: distinct seeds should (almost surely) recover divergent digests"
    );
}
