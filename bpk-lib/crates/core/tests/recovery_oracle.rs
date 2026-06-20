// justifies: INV-RECOVERY-ORACLE-LEGAL; the B3 recovery-oracle matrix uses expect/panic as the assertion style when a recovered state falls outside the legal {CommittedPrefix | RolledBack | CanonicalRefusal} set or is illegal, per crates/core/tests/recovery_oracle.rs
#![allow(clippy::panic, clippy::unwrap_used)]
// The sim filesystem, fault injector, crash hook, and __sim entry points live
// behind `dangerous-test-hooks`; without the feature the whole file is empty.
#![cfg(feature = "dangerous-test-hooks")]
//! GAUNTLET B3 — the recovery legality oracle across the FULL hostile-fs matrix.
//!
//! justifies: INV-RECOVERY-ORACLE-LEGAL — a real `Store` opened over the
//! fault-injecting `SimFs` backend (plus the durability-boundary fault injector),
//! driven through the real `append`/`append_batch`/`sync` API, crashed under EACH
//! hostile-fs fault mode SimFs can model — honest-disk crash, lying-disk
//! fsync-drop, and crash-before-fsync at each durability boundary (single-append
//! frame, batch-commit marker, post-fsync-before-publish, segment-rotation
//! create) — and reopened over the persisted (truncated) tree must recover a
//! state that is EXACTLY one of {CommittedPrefix | RolledBack | CanonicalRefusal}
//! and LEGAL: a prefix of the appended op-log (no invented/undead events), with
//! an intact hash chain; HONEST-disk modes never lose an acknowledged-durable
//! commit; LYING-disk modes may lose a dropped commit but must STILL be legal
//! (prefix, no undead, intact chain); a typed corruption refusal is legal, an
//! untyped failure/panic is not. The SAME `(seed, mode)` recovers the IDENTICAL
//! classification + digest (determinism). See `crates/core/src/store/sim/`.
//!
//! This is the genuine composition: the simulation drives the REAL `Store`, not
//! the sim backends directly. Each cell opens a real `Store`, drives the seeded
//! op stream, induces the cell's fault, crashes via `SimFs::crash`, and reopens.
//!
//! Requires `--features dangerous-test-hooks`. Replay a specific seed with
//! `BATPAK_SEED=N cargo nextest run -p batpak --features dangerous-test-hooks
//! -E 'test(recovery_oracle_matrix_is_legal_and_deterministic)'`.

use batpak::__sim::{MatrixCell, RecoveredClass};

/// Every cell of the matrix must classify as one of the three legal outcomes.
fn assert_legal_classification(cell: &MatrixCell) {
    assert!(
        matches!(
            cell.class,
            RecoveredClass::CommittedPrefix
                | RecoveredClass::RolledBack
                | RecoveredClass::CanonicalRefusal
        ),
        "ILLEGAL RECOVERED STATE: cell `{}` classified as {:?}; the only legal outcomes are \
         CommittedPrefix, RolledBack, or CanonicalRefusal",
        cell.mode,
        cell.class
    );
}

/// GREEN (every-PR, dangerous-test-hooks lane): the full hostile-fs fault matrix
/// — honest-disk crash, lying-disk fsync-drop, and crash-before-fsync at every
/// durability boundary — must recover a LEGAL state in EVERY cell, and the SAME
/// seed must sweep the matrix to the IDENTICAL classification + digest set
/// (determinism). The legality oracle inside `run_recovery_matrix` fail-closes on
/// any illegal recovered state (lost-durable commit on an honest disk, undead
/// event in any mode, broken hash chain, non-canonical reopen).
///
/// RED fixture (`--cfg gauntlet_red_fixture`): asserts the ILLEGAL
/// lost-after-sync outcome on the HONEST-disk cell (`recovered_visible <
/// durable_acked`). The real honest-disk recovery path NEVER loses an
/// acknowledged-durable commit, so this assertion is false and the red half
/// FAILS — proving the oracle actually detects a lost-durable-commit rather than
/// passing vacuously.
#[test]
fn recovery_oracle_matrix_is_legal_and_deterministic() {
    let seed = batpak::__sim::matrix_replay_seed(0x0B3_DEAD_BEEF);
    let steps = 96;

    let first: Vec<MatrixCell> =
        batpak::__sim::run_recovery_matrix(seed, steps).unwrap_or_else(|v| {
            panic!("B3 recovery matrix must be legal on the real recovery path: {v}")
        });
    let second: Vec<MatrixCell> =
        batpak::__sim::run_recovery_matrix(seed, steps).unwrap_or_else(|v| {
            panic!("B3 recovery matrix must be legal on the real recovery path: {v}")
        });

    assert!(
        first.len() >= 6,
        "the matrix must cover honest-disk, lying-disk, and the four crash-before-fsync \
         boundaries (>= 6 cells), got {}",
        first.len()
    );

    assert_eq!(
        first, second,
        "PROPERTY: identical seed (0x{seed:X}) must sweep the matrix to the identical \
         classification + digest set (replay with BATPAK_SEED={seed})"
    );

    for cell in &first {
        assert_legal_classification(cell);
    }

    // RED fixture: assert the ILLEGAL lost-after-sync outcome on the honest-disk
    // cell. The real honest-disk path never loses an acknowledged-durable commit
    // (recovered_visible >= durable_acked always holds), so this assertion is
    // false and the red half FAILS under `--cfg gauntlet_red_fixture`.
    #[cfg(gauntlet_red_fixture)]
    {
        let honest = first
            .iter()
            .find(|c| c.mode.starts_with("honest-disk"))
            .expect("the matrix must include an honest-disk cell");
        assert!(
            honest.recovered_visible < honest.durable_acked,
            "RED FIXTURE: asserts the (illegal) lost-after-sync outcome on the honest-disk cell; \
             MUST fail because an honored-sync commit is required to survive a crash"
        );
    }

    // GREEN: the honest-disk no-loss rule, pinned per honest-disk cell. (Lying
    // disk is deliberately excluded: a dropped fsync may legally lose a commit.)
    #[cfg(not(gauntlet_red_fixture))]
    for cell in &first {
        if cell.mode.starts_with("honest-disk") || cell.mode.starts_with("crash-before-fsync") {
            assert!(
                cell.recovered_visible >= cell.durable_acked,
                "SACRED RULE: honest-disk cell `{}` lost an acknowledged-durable commit \
                 ({} visible < {} durable)",
                cell.mode,
                cell.recovered_visible,
                cell.durable_acked
            );
        }
    }
}

/// Distinct seeds should (almost surely) diverge in the per-cell digest set — the
/// discriminating signal that the matrix actually varies with the seed rather
/// than collapsing to a fixed trace.
#[test]
fn recovery_oracle_matrix_diverges_across_seeds() {
    let a = batpak::__sim::run_recovery_matrix(0x0B3_0001, 96).expect("legal matrix");
    let b = batpak::__sim::run_recovery_matrix(0x0B3_0002, 96).expect("legal matrix");
    let da: Vec<u64> = a.iter().map(|c| c.digest).collect();
    let db: Vec<u64> = b.iter().map(|c| c.digest).collect();
    assert_ne!(
        da, db,
        "PROPERTY: distinct seeds should (almost surely) sweep to divergent per-cell digests"
    );
}
