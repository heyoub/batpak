// The SimBackend monster + GroundTruth oracle live behind `dangerous-test-hooks`;
// without the feature the whole file is empty.
#![cfg(feature = "dangerous-test-hooks")]
//! GAUNTLET bvisor C1 — the G1–G13 proof grid.
//!
//! Each gate `Gn` drives `admit → plan → run` against the [`SimBackend`] in a
//! lying mode and asserts the harness-owned [`GroundTruth`] oracle CATCHES the
//! lie. THE MONSTER NEVER GRADES ITSELF: the oracle diffs the harness-owned
//! GroundTruth (what ACTUALLY happened) against the backend's self-reported
//! [`BoundaryReport`] — the same separation batpak's recovery matrix enforces by
//! reopening a real store and classifying independently.
//!
//! Allow-free: every assertion is `Result`-returning or `assert!(cond, ..)`;
//! there is NO `#![allow(..)]` of any kind (unlike `recovery_oracle.rs`, whose
//! `#![allow(clippy::panic, unwrap_used)]` is deliberately NOT copied here).
//!
//! RED fixture (`--cfg gauntlet_red_fixture`): flips the expectation to "lie
//! uncaught", which is FALSE for a biting oracle, so the red half FAILS —
//! mirroring `recovery_oracle.rs:98-109`. Registered as the blocking
//! ProductionFlip gate `bvisor-grid` in `gate_registry.rs`; its red half is
//! proven by the `gauntlet-red-fixtures-bite` lane (`cargo xtask
//! prove-gates-bite`), which builds it under the cfg and asserts it FAILS.
//!
//! Replay a seed with `BVISOR_SEED=N cargo test -p bvisor
//! --features dangerous-test-hooks --test grid`.

use bvisor::__sim::{run_gate, GateScenario, GATE_SCENARIOS};

/// GREEN: every oracle-catch gate must BITE — the honest control passes clean
/// and the lying monster is caught. The two marker gates (G12 mutation lane,
/// G13 reconciliation) pass as no-op markers so the enumeration is complete.
#[test]
fn grid_g1_through_g13_bites() -> Result<(), String> {
    assert_eq!(GATE_SCENARIOS.len(), 13, "the grid must enumerate G1..G13");

    for scenario in GATE_SCENARIOS {
        let outcome = run_gate(scenario)
            .map_err(|v| format!("gate {} must bite, not run vacuous: {v}", scenario.gate))?;
        assert!(
            outcome.caught,
            "gate {} failed to catch its lie (vacuous gate)",
            scenario.gate
        );
    }
    Ok(())
}

/// GREEN: the grid is DETERMINISTIC — re-running each gate yields the identical
/// outcome + determinism digest (no hidden nondeterminism in the monster's PRNG
/// or the oracle diff).
#[test]
fn grid_is_deterministic() -> Result<(), String> {
    for scenario in GATE_SCENARIOS {
        let a = run_gate(scenario).map_err(|v| v.to_string())?;
        let b = run_gate(scenario).map_err(|v| v.to_string())?;
        assert_eq!(
            a, b,
            "PROPERTY: gate {} must run to an identical outcome + digest",
            scenario.gate
        );
    }
    Ok(())
}

/// ProductionFlip RED branch (mirrors `recovery_oracle.rs:98-109`): under
/// `--cfg gauntlet_red_fixture` it asserts the (illegal) "lie uncaught" outcome —
/// that the oracle does NOT catch the spawn-despite-deny lie. A biting oracle
/// ALWAYS catches it, so this assertion is false and the red half FAILS, proving
/// the grid is anti-vacuous. Registered as the blocking ProductionFlip gate
/// `bvisor-grid` in `gate_registry.rs`.
#[cfg(gauntlet_red_fixture)]
#[test]
fn grid_red_fixture_lie_must_escape() -> Result<(), String> {
    use bvisor::__sim::{GateKind, GateOutcome};

    let g4 = GATE_SCENARIOS
        .iter()
        .copied()
        .find(|s| s.gate == "G4")
        .ok_or_else(|| "the grid must include G4 (no-spawn-when-denied)".to_string())?;

    // The GateKind is checked only so the red fixture stays pinned to a real
    // oracle-catch gate (not a marker), so a future refactor cannot make the red
    // branch vacuously about a no-op marker gate.
    assert!(
        matches!(g4.kind, GateKind::OracleCatch(_)),
        "the red fixture must target an oracle-catch gate, got {:?}",
        g4.kind
    );

    let outcome: GateOutcome = run_gate(g4).map_err(|v| v.to_string())?;
    assert!(
        !outcome.caught,
        "RED FIXTURE: asserts the (illegal) lie-uncaught outcome on G4; MUST fail because the \
         oracle always catches a spawn-despite-deny lie"
    );
    Ok(())
}

/// Re-export sanity: the scenario list is addressable from the public test
/// surface and each gate id is unique.
#[test]
fn grid_gate_ids_are_unique() {
    let mut ids: Vec<&str> = GATE_SCENARIOS
        .iter()
        .map(|s: &GateScenario| s.gate)
        .collect();
    ids.sort_unstable();
    let unique = {
        let mut u = ids.clone();
        u.dedup();
        u
    };
    assert_eq!(ids, unique, "every grid gate id must be unique");
}
