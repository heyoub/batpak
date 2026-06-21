//! 0xE BatPak event payloads (0xD is reserved for effects).
//!
//! These derive [`batpak::EventPayload`] so the host can append them into a
//! BatPak store; bvisor itself writes nothing. The `#[derive(EventPayload)]`
//! macro requires a NAMED-FIELD struct (it rejects tuple structs), so the
//! sketch's `BoundaryPlanEvent(pub BoundaryPlan)` tuple shape is realized here
//! as a single-named-field wrapper — wire-equivalent, derive-compatible.

use crate::contract::ids::BoundaryPlanHash;
use crate::contract::plan::BoundaryPlan;
use crate::contract::recovery::{QuarantineRecord, RecoveryClassification};
use crate::contract::report::BoundaryReport;
use batpak::EventPayload;
use serde::{Deserialize, Serialize};

/// 0xE/0x001 — a sealed [`BoundaryPlan`], appended by the host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xE, type_id = 0x001, version = 1)]
pub struct BoundaryPlanEvent {
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
