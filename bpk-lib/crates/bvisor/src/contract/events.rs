//! 0xE BatPak event payloads (0xD is reserved for effects).
//!
//! These derive [`batpak::EventPayload`] so the host can append them into a
//! BatPak store; bvisor itself writes nothing. The `#[derive(EventPayload)]`
//! macro requires a NAMED-FIELD struct (it rejects tuple structs), so each event
//! is a single- or small-named-field wrapper — wire-equivalent, derive-compatible.

use crate::contract::host_control::CommitDurability;
use crate::contract::ids::{ArtifactId, AttemptId, BoundaryPlanHash};
use crate::contract::plan::BoundaryPlan;
use crate::contract::recovery::{QuarantineRecord, RecoveryClassification};
use crate::contract::report::BoundaryReport;
use batpak::EventPayload;
use serde::{Deserialize, Serialize};

/// 0xE/0x001 — a sealed [`BoundaryPlan`], durably appended by the host AT
/// RUN-START (gated to `Durable` before the backend executes).
///
/// Its PRESENCE in the stream is the recovery hinge: a plan computed but never
/// started leaves NO event (it is *absent*, not rolled back); a started event
/// with no [`BoundaryReportEvent`] is a started-then-crashed run to sweep. The
/// `plan` field embeds the admitted plan. (Formerly `BoundaryPlanEvent` — renamed
/// to name the moment it marks; the wire shape is unchanged.)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xE, type_id = 0x001, version = 1)]
pub struct BoundaryStartedEvent {
    /// The admitted, machine-bound plan.
    pub plan: BoundaryPlan,
}

/// 0xE/0x002 — a sealed [`BoundaryReport`], appended by the host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xE, type_id = 0x002, version = 1)]
pub struct BoundaryReportEvent {
    /// The sealed (canonicalized + hashed) report.
    pub report: BoundaryReport,
}

/// 0xE/0x003 — a typed, replayable startup-reconciliation verdict.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xE, type_id = 0x003, version = 1)]
pub struct BoundaryRecoveryEvent {
    /// The plan whose in-flight boundary was reconciled.
    pub plan_id: BoundaryPlanHash,
    /// The reconciliation verdict.
    pub classification: RecoveryClassification,
    /// Orphan procs/fds/dirs swept on `RolledBack`.
    pub quarantined: Vec<QuarantineRecord>,
}

/// What the host decided to do with a produced (staged) artifact.
///
/// Disposition is POST-REPORT: the report STAGES artifacts; the host authorizes
/// commit/discard SEPARATELY, so a report is never self-authorizing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum DispositionAction {
    /// Promote the artifact out of quarantine.
    Promote,
    /// Keep it quarantined for later review.
    Retain,
    /// Destroy the quarantined bytes.
    Discard,
    /// Refuse it (torn/contradictory evidence): retain for review, never promote.
    Refuse,
}

/// The lifecycle phase a [`BoundaryDispositionEvent`] records for one artifact.
///
/// The ceremony is `Decided` (persisted + durable BEFORE any byte move) →
/// `Applied` (the move happened, at the achieved durability) | `Failed`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum DispositionPhase {
    /// The host decided an action for the artifact — durable BEFORE any promote
    /// or discard, so a crash mid-ceremony reconciles unambiguously.
    Decided {
        /// The authorized action.
        action: DispositionAction,
    },
    /// The decided action was carried out, at the achieved commit durability
    /// (never claims more atomicity than actually happened).
    Applied {
        /// The durability actually achieved.
        durability: CommitDurability,
    },
    /// Carrying out the decided action failed; the bytes remain quarantined.
    Failed {
        /// Stable failure detail (audit evidence).
        reason: String,
    },
}

/// 0xE/0x004 — a post-report artifact disposition decision/result, appended by
/// the host.
///
/// The authorization boundary for a staged artifact: a [`BoundaryReportEvent`]
/// lists what was produced; THIS event records what the host authorized and what
/// happened to it. Splitting them keeps the report from being self-authorizing
/// and lets a disposition occur (and be reconciled) after the report seals.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xE, type_id = 0x004, version = 1)]
pub struct BoundaryDispositionEvent {
    /// The plan whose run produced the artifact.
    pub plan_id: BoundaryPlanHash,
    /// The attempt that produced it.
    pub attempt: AttemptId,
    /// The artifact occurrence being disposed.
    pub artifact: ArtifactId,
    /// The disposition lifecycle phase this event records.
    pub phase: DispositionPhase,
}
