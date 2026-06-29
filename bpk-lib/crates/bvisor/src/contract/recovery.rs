//! Startup reconciliation — FIRST-CLASS, not prose.
//!
//! What the NEXT `open()` concludes about a boundary whose plan was sealed but
//! whose report was not (host crash). DISTINCT from the run-time
//! [`crate::Outcome`].
//!
//! Reconciliation is **view + probe + reconcile** (master plan §13):
//! - [`RunView`] is a PURE fold over the durable 0xE events of one attempt (it
//!   doubles as the live-status projection — one fold, two uses);
//! - [`RecoveryProbe`] is INDEPENDENTLY observed host reality (live orphans, a
//!   torn report frame, per-artifact quarantine/promoted bytes) — never a
//!   backend self-report;
//! - [`reconcile`] is a PURE decision over the two, yielding a [`RecoveryAction`]
//!   the host applies before appending a [`crate::BoundaryRecoveryEvent`] (0x003).
//!
//! The independent-oracle rule (the monster never grades itself) is preserved:
//! [`reconcile`] compares what the events SAY ([`RunView`]) against what the world
//! IS ([`RecoveryProbe`]).

use crate::contract::events::{
    BoundaryDispositionEvent, BoundaryRecoveryEvent, BoundaryReportEvent, BoundaryStartedEvent,
    DispositionAction, DispositionPhase,
};
use crate::contract::ids::{ArtifactId, AttemptId, BoundaryPlanHash};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A reconciliation verdict for one in-flight boundary on `open()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RecoveryClassification {
    /// A terminal report was sealed; the boundary completed.
    Completed,
    /// Plan sealed, no report, no committed artifacts; rolled back + swept.
    RolledBack,
    /// Torn / contradictory 0xE state; a typed refusal, never silent repair.
    CanonicalRefusal,
}

/// One orphan (proc / fd / dir) swept during a `RolledBack` reconciliation.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct QuarantineRecord {
    /// Stable kind tag, e.g. `"process"`, `"fd"`, `"dir"`.
    pub kind: String,
    /// Stable identifier of the swept resource (audit evidence).
    pub reference: String,
}

/// The folded disposition state of one artifact (from its 0x004 phases).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DispositionState {
    /// The authorized action, once a `Decided` phase is durable.
    pub decided: Option<DispositionAction>,
    /// Whether an `Applied` phase was recorded (the move happened).
    pub applied: bool,
    /// Whether a `Failed` phase was recorded (the move failed; bytes retained).
    pub failed: bool,
}

/// A PURE projection of one attempt, folded from its durable 0xE events.
///
/// Serves both the live-status read model and the reconciliation input (the
/// "RecoveryView" of master plan §13) — one fold, two uses. Built empty for an
/// attempt, then `observe_*` is called once per decoded 0xE event in stream
/// order; the fold is idempotent per phase (re-observing the same phase is a
/// no-op), so replay reconstructs the identical view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunView {
    /// The attempt this view projects.
    pub attempt: AttemptId,
    /// The plan that attempt ran.
    pub plan_id: BoundaryPlanHash,
    /// A `BoundaryStartedEvent` (0x001) is durable — the attempt entered truth.
    pub started: bool,
    /// A `BoundaryReportEvent` (0x002) is durable and decoded.
    pub reported: bool,
    /// A `BoundaryRecoveryEvent` (0x003) is already durable (idempotent restart).
    pub recovery_recorded: bool,
    /// Per-artifact disposition state, folded from 0x004 phases.
    pub dispositions: BTreeMap<ArtifactId, DispositionState>,
}

impl RunView {
    /// An empty view for `attempt` running `plan_id` — before any event is folded.
    #[must_use]
    pub fn new(attempt: AttemptId, plan_id: BoundaryPlanHash) -> Self {
        Self {
            attempt,
            plan_id,
            started: false,
            reported: false,
            recovery_recorded: false,
            dispositions: BTreeMap::new(),
        }
    }

    /// Fold a 0x001 started event.
    pub fn observe_started(&mut self, _event: &BoundaryStartedEvent) {
        self.started = true;
    }

    /// Fold a 0x002 report event.
    pub fn observe_report(&mut self, _event: &BoundaryReportEvent) {
        self.reported = true;
    }

    /// Fold a 0x003 recovery event (a prior reconciliation already ran).
    pub fn observe_recovery(&mut self, _event: &BoundaryRecoveryEvent) {
        self.recovery_recorded = true;
    }

    /// Fold a 0x004 disposition event into the per-artifact state.
    pub fn observe_disposition(&mut self, event: &BoundaryDispositionEvent) {
        let state = self.dispositions.entry(event.artifact).or_default();
        match &event.phase {
            DispositionPhase::Decided { action } => state.decided = Some(*action),
            DispositionPhase::Applied { .. } => state.applied = true,
            DispositionPhase::Failed { .. } => state.failed = true,
        }
    }
}

/// What the host INDEPENDENTLY observes about one artifact's bytes on `open()`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ArtifactReality {
    /// Bytes are still present in quarantine.
    pub quarantined_bytes_present: bool,
    /// Bytes are present at the promoted destination.
    pub promoted_bytes_present: bool,
}

/// INDEPENDENTLY observed host reality at `open()` — NOT a backend self-report.
///
/// The second input to [`reconcile`]: live orphans the crash left running, a torn
/// report frame on disk, and per-artifact byte reality. In the pure contract this
/// is data the host fills (the probing impl lives in bvisor-host); the decision
/// over it is pure here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecoveryProbe {
    /// Orphan procs/fds/dirs found live (to sweep on `RolledBack`).
    pub orphans: Vec<QuarantineRecord>,
    /// A torn / partial 0x002 report frame exists on disk (cannot decode).
    pub torn_report: bool,
    /// Per-artifact byte reality, keyed by occurrence id.
    pub artifacts: BTreeMap<ArtifactId, ArtifactReality>,
}

impl RecoveryProbe {
    /// Whether ANY probed artifact has promoted bytes present.
    #[must_use]
    pub fn any_promoted(&self) -> bool {
        self.artifacts.values().any(|a| a.promoted_bytes_present)
    }

    /// The reality for one artifact (all-false default if unprobed).
    #[must_use]
    fn for_artifact(&self, artifact: ArtifactId) -> ArtifactReality {
        self.artifacts.get(&artifact).copied().unwrap_or_default()
    }
}

/// An interrupted disposition the host must finish idempotently before the
/// boundary is settled (re-promote / re-discard the artifact, then record the
/// `Applied` phase).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArtifactFix {
    /// The artifact occurrence to finish.
    pub artifact: ArtifactId,
    /// The decided action to carry to completion.
    pub action: DispositionAction,
}

/// The PURE decision [`reconcile`] reaches for one in-flight attempt. The host
/// APPLIES it (sweep / finish / refuse), then appends the 0x003 verdict from
/// [`RecoveryAction::classification`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecoveryAction {
    /// No started event — the attempt never entered durable truth (§13 "Absent").
    /// Nothing to sweep, nothing to record: [`RecoveryAction::classification`] is
    /// `None`.
    Absent,
    /// A sealed report is durable and every disposition is settled: the boundary
    /// completed. No remediation.
    Completed,
    /// Started, no report: roll back. The host sweeps `orphans`, then records
    /// `RolledBack`.
    RollBack {
        /// Orphans to sweep (from the probe).
        orphans: Vec<QuarantineRecord>,
    },
    /// Reported, but one or more dispositions were interrupted and can be
    /// finished idempotently. After the host finishes them, the boundary is
    /// `Completed`.
    FinishDisposition {
        /// The interrupted dispositions to complete, in artifact-id order.
        artifacts: Vec<ArtifactFix>,
    },
    /// Torn / contradictory durable state — a typed refusal, never silent repair
    /// (a torn report frame; promoted bytes with no durable authorization; an
    /// artifact both promoted and discard-decided). Records `CanonicalRefusal`.
    CanonicalRefusal {
        /// Stable detail of the contradiction (audit evidence).
        reason: String,
    },
}

impl RecoveryAction {
    /// The 0x003 verdict to record AFTER the host applies this action. `Absent`
    /// records nothing (`None`); a finished disposition completes the boundary.
    #[must_use]
    pub fn classification(&self) -> Option<RecoveryClassification> {
        match self {
            Self::Absent => None,
            Self::Completed | Self::FinishDisposition { .. } => {
                Some(RecoveryClassification::Completed)
            }
            Self::RollBack { .. } => Some(RecoveryClassification::RolledBack),
            Self::CanonicalRefusal { .. } => Some(RecoveryClassification::CanonicalRefusal),
        }
    }
}

/// Reconcile one in-flight attempt: PURE decision over what the events SAY
/// ([`RunView`]) vs what the world IS ([`RecoveryProbe`]). Walks the §13 crash
/// windows in order of severity, fail-closed.
///
/// LAW (§13): for every `(seed, boundary)`, this produces the IDENTICAL
/// classification as the independent reconciliation oracle.
#[must_use]
pub fn reconcile(view: &RunView, probe: &RecoveryProbe) -> RecoveryAction {
    // §13 row 1 — no 0x001: never durable truth. Nothing to do.
    if !view.started {
        return RecoveryAction::Absent;
    }

    // §13 row 3 — a torn report frame is a contradiction the host cannot resolve.
    if probe.torn_report {
        return RecoveryAction::CanonicalRefusal {
            reason: "torn report frame on disk".to_string(),
        };
    }

    // §13 row 2 — started, no report.
    if !view.reported {
        // Sacred-window guard: promoted bytes with no sealed report are undead
        // (authorized output we cannot tie to a terminal). Never roll them away.
        if probe.any_promoted() {
            return RecoveryAction::CanonicalRefusal {
                reason: "promoted bytes with no sealed report (sacred window)".to_string(),
            };
        }
        return RecoveryAction::RollBack {
            orphans: probe.orphans.clone(),
        };
    }

    // Reported — reconcile each disposition (§13 rows 4 & 5).
    let mut fixes = Vec::new();
    for (artifact, state) in &view.dispositions {
        match reconcile_disposition(state, probe.for_artifact(*artifact)) {
            DispositionOutcome::Settled => {}
            DispositionOutcome::Finish(action) => fixes.push(ArtifactFix {
                artifact: *artifact,
                action,
            }),
            DispositionOutcome::Refuse(reason) => {
                return RecoveryAction::CanonicalRefusal { reason }
            }
        }
    }

    if fixes.is_empty() {
        RecoveryAction::Completed
    } else {
        RecoveryAction::FinishDisposition { artifacts: fixes }
    }
}

/// The per-artifact reconciliation verdict.
enum DispositionOutcome {
    /// The disposition is settled; nothing to do.
    Settled,
    /// The disposition was interrupted; finish this action idempotently.
    Finish(DispositionAction),
    /// The disposition is torn / contradictory; refuse with this reason.
    Refuse(String),
}

/// Reconcile one artifact's folded disposition against its byte reality.
fn reconcile_disposition(state: &DispositionState, reality: ArtifactReality) -> DispositionOutcome {
    let Some(action) = state.decided else {
        // No durable Decided. Promoted bytes here are unauthorized → refuse;
        // otherwise the artifact is merely staged and may stay quarantined.
        if reality.promoted_bytes_present {
            return DispositionOutcome::Refuse(
                "artifact promoted with no durable disposition decision".to_string(),
            );
        }
        return DispositionOutcome::Settled;
    };

    // A recorded Failed phase settles the artifact (bytes remain quarantined).
    if state.failed {
        return DispositionOutcome::Settled;
    }

    match action {
        // Promote: the bytes must end up at the destination. If Applied is
        // recorded but the bytes are missing, OR Applied is absent, finish it
        // (re-promote is idempotent via the destination short-circuit).
        DispositionAction::Promote => {
            if state.applied && reality.promoted_bytes_present {
                DispositionOutcome::Settled
            } else {
                DispositionOutcome::Finish(DispositionAction::Promote)
            }
        }
        // Discard: a promoted-bytes reality contradicts a discard decision.
        DispositionAction::Discard => {
            if reality.promoted_bytes_present {
                DispositionOutcome::Refuse(
                    "artifact promoted but disposition decided Discard".to_string(),
                )
            } else if state.applied {
                DispositionOutcome::Settled
            } else {
                DispositionOutcome::Finish(DispositionAction::Discard)
            }
        }
        // Retain / Refuse: the bytes stay quarantined; no byte move to finish.
        // A promoted-bytes reality still contradicts a "leave it" decision.
        DispositionAction::Retain | DispositionAction::Refuse => {
            if reality.promoted_bytes_present {
                DispositionOutcome::Refuse(
                    "artifact promoted but disposition decided Retain/Refuse".to_string(),
                )
            } else {
                DispositionOutcome::Settled
            }
        }
    }
}

#[cfg(test)]
mod reconcile_tests {
    use super::{
        reconcile, ArtifactFix, ArtifactReality, DispositionState, RecoveryAction,
        RecoveryClassification, RunView,
    };
    use crate::contract::events::{BoundaryDispositionEvent, DispositionAction, DispositionPhase};
    use crate::contract::host_control::CommitDurability;
    use crate::contract::ids::{ArtifactId, AttemptId, BoundaryPlanHash};
    use crate::contract::recovery::QuarantineRecord;

    fn att() -> AttemptId {
        AttemptId([1u8; 32])
    }
    fn pid() -> BoundaryPlanHash {
        BoundaryPlanHash([2u8; 32])
    }
    fn aid(n: u8) -> ArtifactId {
        ArtifactId([n; 32])
    }

    /// A started, reported view with no dispositions.
    fn reported_view() -> RunView {
        let mut view = RunView::new(att(), pid());
        view.started = true;
        view.reported = true;
        view
    }

    fn orphan() -> QuarantineRecord {
        QuarantineRecord {
            kind: "process".to_string(),
            reference: "pid:4242".to_string(),
        }
    }

    // §13 row 1 — never started → Absent, records nothing.
    #[test]
    fn absent_when_never_started() {
        let view = RunView::new(att(), pid());
        let action = reconcile(&view, &super::RecoveryProbe::default());
        assert_eq!(action, RecoveryAction::Absent);
        assert_eq!(action.classification(), None);
    }

    // §13 row 2 — started, no report → RollBack sweeping the probed orphans.
    #[test]
    fn rollback_when_started_without_report() {
        let mut view = RunView::new(att(), pid());
        view.started = true;
        let probe = super::RecoveryProbe {
            orphans: vec![orphan()],
            ..Default::default()
        };
        let action = reconcile(&view, &probe);
        assert_eq!(
            action,
            RecoveryAction::RollBack {
                orphans: vec![orphan()]
            }
        );
        assert_eq!(
            action.classification(),
            Some(RecoveryClassification::RolledBack)
        );
    }

    // §13 row 3 — a torn report frame → CanonicalRefusal (severity beats rollback).
    #[test]
    fn canonical_refusal_on_torn_report() {
        let mut view = RunView::new(att(), pid());
        view.started = true;
        let probe = super::RecoveryProbe {
            torn_report: true,
            orphans: vec![orphan()],
            ..Default::default()
        };
        assert!(matches!(
            reconcile(&view, &probe),
            RecoveryAction::CanonicalRefusal { .. }
        ));
    }

    // §13 row 5 (sacred window) — promoted bytes but no sealed report → refusal,
    // never roll the authorized output away.
    #[test]
    fn sacred_window_refuses_promoted_bytes_without_report() {
        let mut view = RunView::new(att(), pid());
        view.started = true;
        let mut probe = super::RecoveryProbe::default();
        probe.artifacts.insert(
            aid(9),
            ArtifactReality {
                promoted_bytes_present: true,
                ..Default::default()
            },
        );
        let action = reconcile(&view, &probe);
        assert!(matches!(action, RecoveryAction::CanonicalRefusal { .. }));
        assert_eq!(
            action.classification(),
            Some(RecoveryClassification::CanonicalRefusal)
        );
    }

    #[test]
    fn completed_when_reported_with_no_dispositions() {
        let action = reconcile(&reported_view(), &super::RecoveryProbe::default());
        assert_eq!(action, RecoveryAction::Completed);
        assert_eq!(
            action.classification(),
            Some(RecoveryClassification::Completed)
        );
    }

    // §13 row 4 — Decided{Promote}, not Applied → finish idempotently.
    #[test]
    fn finish_disposition_when_promote_decided_not_applied() {
        let mut view = reported_view();
        view.dispositions.insert(
            aid(3),
            DispositionState {
                decided: Some(DispositionAction::Promote),
                applied: false,
                failed: false,
            },
        );
        let action = reconcile(&view, &super::RecoveryProbe::default());
        assert_eq!(
            action,
            RecoveryAction::FinishDisposition {
                artifacts: vec![ArtifactFix {
                    artifact: aid(3),
                    action: DispositionAction::Promote,
                }]
            }
        );
        // A finished disposition completes the boundary.
        assert_eq!(
            action.classification(),
            Some(RecoveryClassification::Completed)
        );
    }

    #[test]
    fn settled_when_promote_applied_and_bytes_present() {
        let mut view = reported_view();
        view.dispositions.insert(
            aid(3),
            DispositionState {
                decided: Some(DispositionAction::Promote),
                applied: true,
                failed: false,
            },
        );
        let mut probe = super::RecoveryProbe::default();
        probe.artifacts.insert(
            aid(3),
            ArtifactReality {
                promoted_bytes_present: true,
                ..Default::default()
            },
        );
        assert_eq!(reconcile(&view, &probe), RecoveryAction::Completed);
    }

    // §7 — Applied recorded but bytes missing → re-promote (idempotent).
    #[test]
    fn finish_when_applied_but_promoted_bytes_missing() {
        let mut view = reported_view();
        view.dispositions.insert(
            aid(3),
            DispositionState {
                decided: Some(DispositionAction::Promote),
                applied: true,
                failed: false,
            },
        );
        assert!(matches!(
            reconcile(&view, &super::RecoveryProbe::default()),
            RecoveryAction::FinishDisposition { .. }
        ));
    }

    #[test]
    fn refuse_discard_decision_contradicted_by_promoted_bytes() {
        let mut view = reported_view();
        view.dispositions.insert(
            aid(3),
            DispositionState {
                decided: Some(DispositionAction::Discard),
                applied: false,
                failed: false,
            },
        );
        let mut probe = super::RecoveryProbe::default();
        probe.artifacts.insert(
            aid(3),
            ArtifactReality {
                promoted_bytes_present: true,
                ..Default::default()
            },
        );
        assert!(matches!(
            reconcile(&view, &probe),
            RecoveryAction::CanonicalRefusal { .. }
        ));
    }

    #[test]
    fn refuse_promoted_bytes_with_no_decision() {
        let mut view = reported_view();
        view.dispositions
            .insert(aid(3), DispositionState::default());
        let mut probe = super::RecoveryProbe::default();
        probe.artifacts.insert(
            aid(3),
            ArtifactReality {
                promoted_bytes_present: true,
                ..Default::default()
            },
        );
        assert!(matches!(
            reconcile(&view, &probe),
            RecoveryAction::CanonicalRefusal { .. }
        ));
    }

    #[test]
    fn failed_disposition_settles() {
        let mut view = reported_view();
        view.dispositions.insert(
            aid(3),
            DispositionState {
                decided: Some(DispositionAction::Promote),
                applied: false,
                failed: true,
            },
        );
        assert_eq!(
            reconcile(&view, &super::RecoveryProbe::default()),
            RecoveryAction::Completed
        );
    }

    // The fold: Decided then Applied accumulate onto one artifact's state.
    #[test]
    fn disposition_fold_accumulates_phases() {
        let mut view = RunView::new(att(), pid());
        view.observe_disposition(&BoundaryDispositionEvent {
            plan_id: pid(),
            attempt: att(),
            artifact: aid(5),
            phase: DispositionPhase::Decided {
                action: DispositionAction::Promote,
            },
        });
        view.observe_disposition(&BoundaryDispositionEvent {
            plan_id: pid(),
            attempt: att(),
            artifact: aid(5),
            phase: DispositionPhase::Applied {
                durability: CommitDurability::Durable,
            },
        });
        let state = view.dispositions.get(&aid(5)).expect("artifact folded");
        assert_eq!(state.decided, Some(DispositionAction::Promote));
        assert!(state.applied);
        assert!(!state.failed);
    }
}
