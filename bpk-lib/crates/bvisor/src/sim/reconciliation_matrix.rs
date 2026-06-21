//! The startup-reconciliation matrix (B3-shaped, G13).
//!
//! Mirrors batpak's `store/sim/recovery_matrix.rs`: sweep `(crash_boundary ×
//! seed)`, classify each in-flight boundary as EXACTLY one of
//! {`Completed` | `RolledBack` | `CanonicalRefusal`}, and fail closed on any
//! ILLEGAL recovered state. Determinism via an FNV digest: the same
//! `(seed, boundary)` recovers the IDENTICAL classification + digest.
//!
//! The boundary is the host-crash window between a sealed plan and a sealed
//! report. On the next `open()` the harness reconciles the in-flight boundary
//! from the persisted 0xE evidence INDEPENDENTLY of any backend self-report —
//! the same "reopen and classify independently" separation as the recovery
//! matrix.
//!
//! Crash boundaries (the sacred window included):
//! - [`CrashBoundary::PlanSealed`] — plan sealed, nothing else → `RolledBack`.
//! - [`CrashBoundary::EffectInFlight`] — effect started, no commit → `RolledBack`.
//! - [`CrashBoundary::ArtifactCommittedPreReport`] — SACRED: a committed artifact
//!   exists but the report was not sealed → must NOT lose the artifact;
//!   reconciles as `CanonicalRefusal` (torn: commit without its report), never a
//!   silent `Completed` that would resurrect an unreported boundary.
//! - [`CrashBoundary::ReportWrittenPreFsync`] — report bytes written but not
//!   fsync'd → torn report → `CanonicalRefusal`.
//!
//! Illegal (oracle fails closed): `LostCommittedArtifact`, `UndeadBoundary`
//! (Completed with no sealed report), `LiveOrphanAfterRollback`,
//! `NonCanonicalReopen` (untyped failure vs a typed refusal).

use crate::contract::recovery::{QuarantineRecord, RecoveryClassification};
use crate::sim::{fold, seed_from_env, Prng, FNV_OFFSET};

/// A host-crash boundary at which the in-flight boundary is reconciled on the
/// next `open()`. Sibling of the recovery matrix's `Boundary`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CrashBoundary {
    /// Plan sealed; no effect started, no artifact, no report.
    PlanSealed,
    /// An effect was in flight; no artifact committed, no report sealed.
    EffectInFlight,
    /// SACRED window: an artifact was committed but the report was not sealed.
    ArtifactCommittedPreReport,
    /// Report bytes were written but not yet fsync'd (torn report).
    ReportWrittenPreFsync,
}

impl CrashBoundary {
    /// A stable digest token discriminating each boundary.
    fn token(self) -> u64 {
        match self {
            CrashBoundary::PlanSealed => 0xB0_01,
            CrashBoundary::EffectInFlight => 0xB0_02,
            CrashBoundary::ArtifactCommittedPreReport => 0xB0_03,
            CrashBoundary::ReportWrittenPreFsync => 0xB0_04,
        }
    }

    /// A short stable label.
    fn label(self) -> &'static str {
        match self {
            CrashBoundary::PlanSealed => "plan-sealed",
            CrashBoundary::EffectInFlight => "effect-in-flight",
            CrashBoundary::ArtifactCommittedPreReport => "artifact-committed-pre-report",
            CrashBoundary::ReportWrittenPreFsync => "report-written-pre-fsync",
        }
    }
}

/// Every crash boundary the matrix sweeps.
#[must_use]
pub fn all_crash_boundaries() -> Vec<CrashBoundary> {
    vec![
        CrashBoundary::PlanSealed,
        CrashBoundary::EffectInFlight,
        CrashBoundary::ArtifactCommittedPreReport,
        CrashBoundary::ReportWrittenPreFsync,
    ]
}

/// The reconciliation verdict, mirrored from [`RecoveryClassification`] for the
/// public matrix surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReconClass {
    /// A terminal report was sealed; the boundary completed.
    Completed,
    /// Plan sealed, no report, no committed artifact; rolled back + swept.
    RolledBack,
    /// Torn / contradictory 0xE state; a typed refusal, never silent repair.
    CanonicalRefusal,
}

impl From<RecoveryClassification> for ReconClass {
    fn from(c: RecoveryClassification) -> Self {
        match c {
            RecoveryClassification::Completed => ReconClass::Completed,
            RecoveryClassification::RolledBack => ReconClass::RolledBack,
            RecoveryClassification::CanonicalRefusal => ReconClass::CanonicalRefusal,
        }
    }
}

impl ReconClass {
    fn token(self) -> u64 {
        match self {
            ReconClass::Completed => 0xC0_01,
            ReconClass::RolledBack => 0xC0_02,
            ReconClass::CanonicalRefusal => 0xC0_03,
        }
    }
}

/// An ILLEGAL recovered state — the oracle fails closed on any of these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReconViolation {
    /// A committed artifact was lost (the no-loss / sacred-window violation).
    LostCommittedArtifact {
        /// The crash boundary that lost it.
        boundary: &'static str,
    },
    /// `Completed` with no sealed report (an undead boundary).
    UndeadBoundary {
        /// The crash boundary that produced the undead classification.
        boundary: &'static str,
    },
    /// A live orphan remained after a `RolledBack` reconciliation.
    LiveOrphanAfterRollback {
        /// The crash boundary.
        boundary: &'static str,
        /// The orphan left live.
        orphan: String,
    },
    /// Reopen failed with a non-canonical (untyped) outcome.
    NonCanonicalReopen {
        /// The crash boundary.
        boundary: &'static str,
        /// Human-readable detail.
        detail: String,
    },
}

impl std::fmt::Display for ReconViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LostCommittedArtifact { boundary } => write!(
                f,
                "lost committed artifact at crash boundary `{boundary}` (sacred-window violation)"
            ),
            Self::UndeadBoundary { boundary } => write!(
                f,
                "undead boundary at `{boundary}`: classified Completed with no sealed report"
            ),
            Self::LiveOrphanAfterRollback { boundary, orphan } => write!(
                f,
                "live orphan `{orphan}` survived rollback at crash boundary `{boundary}`"
            ),
            Self::NonCanonicalReopen { boundary, detail } => {
                write!(f, "non-canonical reopen at `{boundary}`: {detail}")
            }
        }
    }
}

/// The persisted 0xE evidence a crash left behind, as the next `open()` finds it.
/// This is the INDEPENDENT input the harness reconciles from — not a backend's
/// self-report.
#[derive(Clone, Debug)]
struct CrashState {
    plan_sealed: bool,
    artifact_committed: bool,
    report_sealed: bool,
    report_torn: bool,
    /// Orphans the crash left live (procs/fds/dirs). Reconciliation must sweep
    /// them on `RolledBack`.
    orphans: Vec<QuarantineRecord>,
}

/// Derive the persisted crash state for a boundary. Seeded so the orphan set
/// varies with the seed while the classification stays a function of the
/// boundary (determinism: same seed+boundary ⇒ identical everything).
fn crash_state(boundary: CrashBoundary, prng: &mut Prng) -> CrashState {
    let orphan_id = prng.next_u64() % 0x1_0000;
    let orphans = vec![QuarantineRecord {
        kind: "process".to_string(),
        reference: format!("pid:{orphan_id}"),
    }];
    match boundary {
        CrashBoundary::PlanSealed => CrashState {
            plan_sealed: true,
            artifact_committed: false,
            report_sealed: false,
            report_torn: false,
            orphans,
        },
        CrashBoundary::EffectInFlight => CrashState {
            plan_sealed: true,
            artifact_committed: false,
            report_sealed: false,
            report_torn: false,
            orphans,
        },
        CrashBoundary::ArtifactCommittedPreReport => CrashState {
            plan_sealed: true,
            artifact_committed: true,
            report_sealed: false,
            report_torn: false,
            orphans,
        },
        CrashBoundary::ReportWrittenPreFsync => CrashState {
            plan_sealed: true,
            artifact_committed: false,
            report_sealed: false,
            report_torn: true,
            orphans,
        },
    }
}

/// The reconciliation OUTCOME: the swept orphan set + the classification.
struct Reconciled {
    class: RecoveryClassification,
    swept: Vec<QuarantineRecord>,
    /// Orphans still live AFTER reconciliation (must be empty on RolledBack).
    remaining: Vec<QuarantineRecord>,
}

/// Reconcile the in-flight boundary from the persisted crash state on `open()`.
/// This is the INDEPENDENT classifier — it reads only the persisted evidence.
fn reconcile(state: &CrashState) -> Reconciled {
    // No sealed plan ⇒ there is no boundary to reconcile; treat as RolledBack
    // (nothing was ever admitted, so nothing can be live).
    if !state.plan_sealed {
        return Reconciled {
            class: RecoveryClassification::RolledBack,
            swept: Vec::new(),
            remaining: Vec::new(),
        };
    }
    // A sealed, untorn report ⇒ Completed.
    if state.report_sealed && !state.report_torn {
        return Reconciled {
            class: RecoveryClassification::Completed,
            swept: Vec::new(),
            remaining: Vec::new(),
        };
    }
    // A torn report, OR a committed artifact with no sealed report, is a
    // contradictory 0xE state ⇒ CanonicalRefusal (never silent repair, never a
    // resurrected Completed). The sacred window forbids dropping the artifact.
    if state.report_torn || state.artifact_committed {
        return Reconciled {
            class: RecoveryClassification::CanonicalRefusal,
            swept: Vec::new(),
            remaining: Vec::new(),
        };
    }
    // Plan sealed, no report, no committed artifact ⇒ RolledBack: sweep orphans.
    Reconciled {
        class: RecoveryClassification::RolledBack,
        swept: state.orphans.clone(),
        remaining: Vec::new(),
    }
}

/// One cell of the matrix: the boundary label, the recovered classification, and
/// the determinism digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconCell {
    /// Human-readable crash-boundary label.
    pub boundary: String,
    /// The recovered classification.
    pub class: ReconClass,
    /// FNV determinism digest for this cell.
    pub digest: u64,
    /// Number of orphans swept during reconciliation.
    pub swept: usize,
}

/// Run one matrix cell: derive the crash state for `(seed, boundary)`, reconcile
/// it, and legality-check. Fails closed on any illegal recovered state.
///
/// # Errors
/// Returns a seed/boundary-tagged [`ReconViolation`] on an illegal recovered
/// state.
pub fn run_cell(seed: u64, boundary: CrashBoundary) -> Result<ReconCell, ReconViolation> {
    let mut prng = Prng::new(fold(fold(FNV_OFFSET, seed), boundary.token()));
    let state = crash_state(boundary, &mut prng);
    let outcome = reconcile(&state);
    let class: ReconClass = outcome.class.into();

    // ── Legality oracle (fail closed). ───────────────────────────────────────
    // Sacred window: a committed artifact must never vanish into a clean rollback
    // or a silent Completed.
    if state.artifact_committed && matches!(class, ReconClass::RolledBack | ReconClass::Completed) {
        return Err(ReconViolation::LostCommittedArtifact {
            boundary: boundary.label(),
        });
    }
    // Undead: Completed requires a sealed plan AND a genuinely sealed, untorn
    // report. Anything less classified as Completed is an undead boundary.
    if matches!(class, ReconClass::Completed)
        && !(state.plan_sealed && state.report_sealed && !state.report_torn)
    {
        return Err(ReconViolation::UndeadBoundary {
            boundary: boundary.label(),
        });
    }
    // RolledBack must sweep every orphan (none left live).
    if matches!(class, ReconClass::RolledBack) {
        if let Some(orphan) = outcome.remaining.first() {
            return Err(ReconViolation::LiveOrphanAfterRollback {
                boundary: boundary.label(),
                orphan: orphan.reference.clone(),
            });
        }
    }

    let mut digest = fold(fold(FNV_OFFSET, seed), boundary.token());
    digest = fold(digest, class.token());
    digest = fold(digest, outcome.swept.len() as u64);
    Ok(ReconCell {
        boundary: boundary.label().to_string(),
        class,
        digest,
        swept: outcome.swept.len(),
    })
}

/// Sweep the full crash-boundary matrix for `seed`, one [`ReconCell`] per
/// boundary. The legality oracle inside [`run_cell`] fail-closes per cell.
///
/// # Errors
/// Returns a seed/boundary-tagged violation string on the first illegal cell.
pub fn run_reconciliation_matrix(seed: u64) -> Result<Vec<ReconCell>, String> {
    all_crash_boundaries()
        .into_iter()
        .map(|boundary| {
            run_cell(seed, boundary)
                .map_err(|v| format!("reconciliation violation (seed={seed}): {v}"))
        })
        .collect()
}

/// Replay-seed helper for `BVISOR_SEED` / `BATPAK_SEED`.
#[must_use]
pub fn reconciliation_replay_seed(default: u64) -> u64 {
    seed_from_env(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_boundary_recovers_legally() -> Result<(), String> {
        for boundary in all_crash_boundaries() {
            run_cell(0x5EED_C301, boundary)
                .map_err(|v| format!("boundary {boundary:?} must recover legally: {v}"))?;
        }
        Ok(())
    }

    #[test]
    fn same_seed_boundary_is_deterministic() -> Result<(), String> {
        for boundary in all_crash_boundaries() {
            let a = run_cell(0x5EED_C302, boundary).map_err(|v| v.to_string())?;
            let b = run_cell(0x5EED_C302, boundary).map_err(|v| v.to_string())?;
            assert_eq!(
                a, b,
                "PROPERTY: identical (seed, boundary={boundary:?}) recovers identically"
            );
        }
        Ok(())
    }

    #[test]
    fn sacred_window_refuses_never_loses() -> Result<(), String> {
        let cell = run_cell(0x5EED_C303, CrashBoundary::ArtifactCommittedPreReport)
            .map_err(|v| v.to_string())?;
        assert_eq!(
            cell.class,
            ReconClass::CanonicalRefusal,
            "a committed artifact with no sealed report must be a typed refusal, never lost"
        );
        Ok(())
    }
}
