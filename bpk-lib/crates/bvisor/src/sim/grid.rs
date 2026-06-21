//! The G1–G13 proof grid driver.
//!
//! Each gate `Gn` drives `admit → plan → run` against the [`SimBackend`] in a
//! lying mode and asserts the harness-owned [`GroundTruth`] oracle CATCHES the
//! lie. The actual `#[test]` functions live in `tests/grid.rs` (with the
//! ProductionFlip `#[cfg(gauntlet_red_fixture)]` red branch); this module is the
//! reusable driver so the test file stays thin and allow-free.
//!
//! THE MONSTER NEVER GRADES ITSELF: [`run_gate`] runs the backend, snapshots the
//! harness-owned GroundTruth, seals the report independently, and diffs — the
//! same separation batpak's recovery matrix enforces.
//!
//! Coverage:
//! - G1–G7 ENFORCEMENT (read / net / quarantine / spawn / orphan / fd / commit):
//!   each is a [`Lie`] the oracle must surface from the GroundTruth diff.
//! - G8 always-seals-terminal-report, G9 no-dropped-denied-attempt,
//!   G10 admission-honesty (no over-claimed depth), G11 crash-mid-boundary.
//! - G12 policy-mutation-caught is a MUTATION-LANE requirement (not a runtime
//!   test) — represented here as a scenario marker so the grid enumerates all 13.
//! - G13 terminal-classification-on-recovery is proven by the reconciliation
//!   oracle (`reconciliation_matrix`) — marked here, exercised there.

use crate::contract::backend::Backend;
use crate::contract::plan::{BoundarySpec, Budgets, EvidenceRequirements, PlanError, Workload};
use crate::contract::registry::{BackendRegistry, BoundaryPlanner, BoundaryRunner};
use crate::sim::backend::{run_seals, LieMode, OneShotLiar, SimBackend};
use crate::sim::ground_truth::{GroundTruth, GroundTruthDiff, Lie};
use crate::sim::{fold, FNV_OFFSET};
use std::sync::Arc;

/// Digest token folded for the marker gates (G12/G13) proven outside the runtime
/// oracle, so their grid cells still contribute a stable, distinguishable digest.
const MARKER_TOKEN: u64 = 0x6A_84_C2_12;

/// How a gate is proven, so the grid enumerates all 13 honestly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateKind {
    /// An enforcement/honesty lie the runtime oracle catches via the diff.
    OracleCatch(Lie),
    /// Proven by the mutation lane (G12), not a runtime diff.
    MutationLane,
    /// Proven by the reconciliation oracle (G13), exercised elsewhere.
    Reconciliation,
}

/// One gate of the proof grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateScenario {
    /// The gate id, `"G1"`..`"G13"`.
    pub gate: &'static str,
    /// One-line statement of the property the gate proves.
    pub property: &'static str,
    /// How the gate is proven.
    pub kind: GateKind,
}

/// Every gate G1–G13, in order. The oracle-catch gates are driven by [`run_gate`];
/// G12/G13 are marked so the enumeration is complete and auditable.
pub const GATE_SCENARIOS: [GateScenario; 13] = [
    GateScenario {
        gate: "G1",
        property: "no secret read when filesystem read is enforced/denied",
        kind: GateKind::OracleCatch(Lie::ClaimEnforcedButAllowRead),
    },
    GateScenario {
        gate: "G2",
        property: "no network when network is denied",
        kind: GateKind::OracleCatch(Lie::ClaimEnforcedButAllowNet),
    },
    GateScenario {
        gate: "G3",
        property: "writes confined to quarantine",
        kind: GateKind::OracleCatch(Lie::WriteEscapesQuarantine),
    },
    GateScenario {
        gate: "G4",
        property: "no child spawn when spawn is denied",
        kind: GateKind::OracleCatch(Lie::SpawnDespiteDeny),
    },
    GateScenario {
        gate: "G5",
        property: "no orphan retention hidden from the report",
        kind: GateKind::OracleCatch(Lie::DropOrphanFromReport),
    },
    GateScenario {
        gate: "G6",
        property: "no inherited-fd escape into a child/proxy",
        kind: GateKind::OracleCatch(Lie::ProxyInheritedFd),
    },
    GateScenario {
        gate: "G7",
        property: "no implicit commit reported as uncommitted",
        kind: GateKind::OracleCatch(Lie::AutoCommitButReportFalse),
    },
    GateScenario {
        gate: "G8",
        property: "always seals a terminal report",
        kind: GateKind::OracleCatch(Lie::SkipSealing),
    },
    GateScenario {
        gate: "G9",
        property: "no dropped denied attempt",
        kind: GateKind::OracleCatch(Lie::DropDeniedAttempt),
    },
    GateScenario {
        gate: "G10",
        property: "admission honesty: no over-claimed enforcement depth",
        kind: GateKind::OracleCatch(Lie::MisreportEnforcementDepth),
    },
    GateScenario {
        gate: "G11",
        property: "crash mid-boundary never reports a false terminal",
        kind: GateKind::OracleCatch(Lie::CrashMidBoundary),
    },
    GateScenario {
        gate: "G12",
        property: "policy mutation caught (mutation lane requirement)",
        kind: GateKind::MutationLane,
    },
    GateScenario {
        gate: "G13",
        property: "terminal classification on recovery (reconciliation oracle)",
        kind: GateKind::Reconciliation,
    },
];

/// A legality violation in one grid cell: the oracle FAILED to catch a lie the
/// monster told (or caught one the honest control did not tell).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateViolation {
    /// The monster told the lie but the oracle did not catch it (vacuous gate).
    LieUncaught {
        /// The gate whose lie escaped.
        gate: &'static str,
        /// The lie the monster told.
        lie: Lie,
        /// What the diff actually caught (for the error message).
        diff: String,
    },
    /// The honest control was flagged as lying (a false positive).
    HonestFlagged {
        /// What the diff caught against the honest control.
        diff: String,
    },
    /// Planning failed unexpectedly for a scenario.
    PlanFailed {
        /// The gate.
        gate: &'static str,
        /// The plan error.
        detail: String,
    },
}

impl std::fmt::Display for GateViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LieUncaught { gate, lie, diff } => write!(
                f,
                "{gate}: VACUOUS — monster told {lie:?} but the oracle did not catch it (diff: {diff})"
            ),
            Self::HonestFlagged { diff } => {
                write!(f, "honest control flagged as lying (diff: {diff})")
            }
            Self::PlanFailed { gate, detail } => write!(f, "{gate}: plan failed: {detail}"),
        }
    }
}

/// The result of driving one gate cell: the lie told, the oracle diff, and a
/// determinism digest folding both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateOutcome {
    /// The gate id.
    pub gate: &'static str,
    /// The lie the monster was set to tell (`None` for the honest control).
    pub lie: Option<Lie>,
    /// Whether the oracle caught the expected lie.
    pub caught: bool,
    /// FNV digest folding the gate token, lie token, and caught flag.
    pub digest: u64,
}

/// A spec that requests every dangerous capability/control so the monster has a
/// surface to lie about, admitted against its deep support matrix.
fn dangerous_spec() -> BoundarySpec {
    use crate::contract::capability::{
        Capability, EnvPolicy, FdPolicy, FsAccess, FsConfinement, NetPolicy, PathSet, SpawnPolicy,
    };
    use crate::contract::host_control::{
        CommitDurability, HostControl, KillGuarantee, KillTarget, PathView, StdStreams,
    };
    BoundarySpec {
        workload: Workload::Process {
            exe: "sim:workload".to_string(),
            args: Vec::new(),
        },
        capabilities: vec![
            Capability::Filesystem {
                access: FsAccess::Read,
                scope: PathSet::empty(),
                recursive: true,
                confinement: FsConfinement::DeclaredRootsOnly,
            },
            Capability::Network {
                policy: NetPolicy::DenyAll,
            },
            Capability::ChildSpawn {
                policy: SpawnPolicy::Deny,
            },
            Capability::Environment {
                policy: EnvPolicy::EmptyExcept(Vec::new()),
            },
            Capability::InheritedFds {
                policy: FdPolicy::None,
            },
        ],
        controls: vec![
            HostControl::LaunchWorkload,
            HostControl::CaptureStreams {
                streams: StdStreams::capture_out_err(),
            },
            HostControl::TempRoot {
                visibility: PathView::PrivateToBoundary,
            },
            HostControl::CommitArtifact {
                durability: CommitDurability::Atomic,
            },
            HostControl::Kill {
                target: KillTarget::RunTree,
                guarantee: KillGuarantee::Atomic,
            },
            HostControl::ListOutputs,
        ],
        budgets: Budgets::default(),
        evidence: EvidenceRequirements::default(),
    }
}

/// Drive one gate cell: admit → plan → run the monster fixed to `mode`, snapshot
/// the harness-owned GroundTruth, seal the report independently, and diff.
///
/// Returns the snapshot of the harness-owned [`GroundTruth`] plus the
/// [`GroundTruthDiff`] (the oracle's verdict), so the caller can assert the
/// expected lie was caught. The GroundTruth is read FROM THE BACKEND HANDLE the
/// HARNESS owns, never from the report — the monster never grades itself.
///
/// # Errors
/// Returns a [`GateViolation::PlanFailed`] if admission fails unexpectedly.
pub fn drive(
    gate: &'static str,
    mode: LieMode,
) -> Result<(GroundTruth, GroundTruthDiff), GateViolation> {
    // The harness keeps a TYPED handle to the monster, distinct from the trait
    // object the registry/runner drives it through, so it can read the
    // independent GroundTruth without downcasting.
    let backend = Arc::new(SimBackend::new(Box::new(OneShotLiar::new(mode))));
    let id = backend.id();
    let mut registry = BackendRegistry::new();
    registry.register(Arc::clone(&backend) as Arc<dyn crate::contract::backend::Backend>);

    let planner = BoundaryPlanner::new(&registry);
    let runner = BoundaryRunner::new(&registry);

    let spec = dangerous_spec();
    let plan = planner
        .plan(&spec, &id)
        .map_err(|e: PlanError| GateViolation::PlanFailed {
            gate,
            detail: e.to_string(),
        })?;

    // The runner SEALS the body independently. For the seal-suppressing modes
    // (G8/G11) the harness models the missing seal via `run_seals` rather than a
    // sealed-but-empty body.
    let sealed = run_seals(mode);
    let body = runner.run(&plan).ok().map(|report| report.body);

    // Snapshot the INDEPENDENT GroundTruth from the HARNESS-OWNED handle.
    let truth = backend.ground_truth();
    let report_for_diff = if sealed { body.as_ref() } else { None };
    let diff = truth.diff(report_for_diff, sealed);
    Ok((truth, diff))
}

/// Run one gate to its [`GateOutcome`], failing closed if the lie escapes the
/// oracle (a vacuous gate) or the honest control is flagged.
///
/// # Errors
/// Returns a [`GateViolation`] if the gate is vacuous, a false positive fires,
/// or planning fails.
pub fn run_gate(scenario: GateScenario) -> Result<GateOutcome, GateViolation> {
    let lie = match scenario.kind {
        GateKind::OracleCatch(lie) => lie,
        // G12/G13 are proven elsewhere; their grid cell is a no-op pass that
        // records the marker (so the enumeration is complete + deterministic).
        GateKind::MutationLane | GateKind::Reconciliation => {
            return Ok(GateOutcome {
                gate: scenario.gate,
                lie: None,
                caught: true,
                digest: fold(fold(FNV_OFFSET, scenario.gate.len() as u64), MARKER_TOKEN),
            });
        }
    };

    // 1) The honest control MUST NOT be flagged (no false positive).
    let (_truth, honest_diff) = drive(scenario.gate, LieMode::Honest)?;
    if !honest_diff.is_clean() {
        return Err(GateViolation::HonestFlagged {
            diff: honest_diff.to_string(),
        });
    }

    // 2) The lying monster MUST be caught (no vacuous gate).
    let (_truth, diff) = drive(scenario.gate, LieMode::Lie(lie))?;
    if !diff.caught(lie) {
        return Err(GateViolation::LieUncaught {
            gate: scenario.gate,
            lie,
            diff: diff.to_string(),
        });
    }

    let digest = fold(
        fold(fold(FNV_OFFSET, scenario.gate.len() as u64), lie as u64),
        u64::from(diff.caught(lie)),
    );
    Ok(GateOutcome {
        gate: scenario.gate,
        lie: Some(lie),
        caught: true,
        digest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_enumerates_thirteen_distinct_gates() {
        let gates: std::collections::BTreeSet<&str> =
            GATE_SCENARIOS.iter().map(|s| s.gate).collect();
        assert_eq!(gates.len(), 13, "G1..G13 must be distinct");
    }

    #[test]
    fn every_oracle_gate_bites() -> Result<(), String> {
        for scenario in GATE_SCENARIOS {
            let outcome =
                run_gate(scenario).map_err(|v| format!("gate {} must bite: {v}", scenario.gate))?;
            assert!(outcome.caught, "gate {} must catch its lie", scenario.gate);
        }
        Ok(())
    }
}
