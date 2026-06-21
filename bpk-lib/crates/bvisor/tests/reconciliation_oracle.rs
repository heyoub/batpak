// The reconciliation matrix lives behind `dangerous-test-hooks`; without the
// feature the whole file is empty.
#![cfg(feature = "dangerous-test-hooks")]
//! GAUNTLET bvisor C1 — the startup-reconciliation oracle (G13).
//!
//! Mirrors batpak's `recovery_oracle.rs`: sweep `(crash_boundary × seed)`,
//! classify each in-flight boundary as EXACTLY one of
//! {`Completed` | `RolledBack` | `CanonicalRefusal`}, and fail closed on any
//! ILLEGAL recovered state (`LostCommittedArtifact`, `UndeadBoundary`,
//! `LiveOrphanAfterRollback`, `NonCanonicalReopen`). The same `(seed, boundary)`
//! recovers the IDENTICAL classification + FNV digest (determinism).
//!
//! The reconciler reads ONLY the persisted 0xE crash state — it never consults a
//! backend self-report, the same "reopen and classify independently" separation
//! batpak's recovery matrix enforces.
//!
//! Allow-free: NO `#![allow(..)]` of any kind (unlike `recovery_oracle.rs`).
//!
//! RED fixture (`--cfg gauntlet_red_fixture`): asserts an illegal
//! `UndeadBoundary` — that a crash with a committed artifact but no sealed report
//! reconciles to `Completed`. The real reconciler returns `CanonicalRefusal`
//! (the sacred window forbids a silent Completed), so the assertion is false and
//! the red half FAILS. (NOT registered in `gate_registry.rs` yet.)
//!
//! Replay a seed with `BVISOR_SEED=N cargo test -p bvisor
//! --features dangerous-test-hooks --test reconciliation_oracle`.

use bvisor::__sim::{reconciliation_replay_seed, run_reconciliation_matrix, ReconCell, ReconClass};

/// Every cell must classify as one of the three legal outcomes.
fn assert_legal_classification(cell: &ReconCell) -> Result<(), String> {
    if matches!(
        cell.class,
        ReconClass::Completed | ReconClass::RolledBack | ReconClass::CanonicalRefusal
    ) {
        Ok(())
    } else {
        Err(format!(
            "ILLEGAL RECONCILED STATE: crash boundary `{}` classified as {:?}",
            cell.boundary, cell.class
        ))
    }
}

/// GREEN: the full crash-boundary matrix must reconcile to a LEGAL state in
/// EVERY cell, and the SAME seed must sweep to the IDENTICAL classification +
/// digest set (determinism). The legality oracle inside
/// `run_reconciliation_matrix` fail-closes on any illegal recovered state.
#[test]
fn reconciliation_matrix_is_legal_and_deterministic() -> Result<(), String> {
    let seed = reconciliation_replay_seed(0x0B13_DEAD_BEEF);

    let first = run_reconciliation_matrix(seed)?;
    let second = run_reconciliation_matrix(seed)?;

    assert!(
        first.len() >= 4,
        "the matrix must cover all four crash boundaries (>= 4 cells), got {}",
        first.len()
    );
    assert_eq!(
        first, second,
        "PROPERTY: identical seed (0x{seed:X}) must sweep to the identical classification + \
         digest set (replay with BVISOR_SEED={seed})"
    );
    for cell in &first {
        assert_legal_classification(cell)?;
    }
    Ok(())
}

/// GREEN: the sacred window NEVER loses a committed artifact. A crash with a
/// committed artifact but no sealed report reconciles to `CanonicalRefusal`
/// (a typed refusal), never a silent `Completed` or a `RolledBack` that drops it.
#[test]
fn sacred_window_is_a_typed_refusal_never_a_loss() -> Result<(), String> {
    let cells = run_reconciliation_matrix(0x0B13_0001)?;
    let sacred = cells
        .iter()
        .find(|c| c.boundary == "artifact-committed-pre-report")
        .ok_or_else(|| "the matrix must include the sacred-window cell".to_string())?;
    if sacred.class == ReconClass::CanonicalRefusal {
        Ok(())
    } else {
        Err(format!(
            "SACRED RULE: committed-artifact-without-report must be CanonicalRefusal, got {:?}",
            sacred.class
        ))
    }
}

/// Distinct seeds should (almost surely) diverge in the per-cell digest set —
/// the discriminating signal that the matrix actually varies with the seed
/// (the orphan set is seeded) rather than collapsing to a fixed trace.
#[test]
fn reconciliation_matrix_diverges_across_seeds() -> Result<(), String> {
    let a = run_reconciliation_matrix(0x0B13_0001)?;
    let b = run_reconciliation_matrix(0x0B13_0002)?;
    let da: Vec<u64> = a.iter().map(|c| c.digest).collect();
    let db: Vec<u64> = b.iter().map(|c| c.digest).collect();
    assert_ne!(
        da, db,
        "PROPERTY: distinct seeds should (almost surely) sweep to divergent per-cell digests"
    );
    Ok(())
}

/// ProductionFlip RED branch (mirrors `recovery_oracle.rs:98-109`): under
/// `--cfg gauntlet_red_fixture` it asserts the illegal `UndeadBoundary` outcome —
/// that the sacred-window cell reconciles to `Completed`. The real reconciler
/// returns `CanonicalRefusal`, so the assertion is false and the red half FAILS,
/// proving the oracle detects an undead boundary. (NOT registered yet.)
#[cfg(gauntlet_red_fixture)]
#[test]
fn reconciliation_red_fixture_undead_boundary_must_fail() -> Result<(), String> {
    let cells = run_reconciliation_matrix(0x0B13_0003)?;
    let sacred = cells
        .iter()
        .find(|c| c.boundary == "artifact-committed-pre-report")
        .ok_or_else(|| "the matrix must include the sacred-window cell".to_string())?;
    assert_eq!(
        sacred.class,
        ReconClass::Completed,
        "RED FIXTURE: asserts the (illegal) UndeadBoundary outcome (Completed with no sealed \
         report); MUST fail because the sacred window reconciles to a typed CanonicalRefusal"
    );
    Ok(())
}
