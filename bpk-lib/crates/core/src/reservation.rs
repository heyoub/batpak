//! Batpak Substrate Closure reservation ledger: dimensionless `units`, opaque `subject_ref`,
//! closed structural states, explicit transition operations, deterministic findings, and
//! reconciliation buckets. This module does **not** import [`crate::store`] and encodes no payment,
//! inventory, capability, or workflow policy.

use crate::evidence::{content_hash, sort_findings, sorted_findings};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Schema version for [`ReservationLedgerReportBody`].
pub const RESERVATION_LEDGER_REPORT_SCHEMA_VERSION: u32 = 1;

/// Schema version for [`ReservationReconciliationReportBody`].
pub const RESERVATION_RECONCILIATION_REPORT_SCHEMA_VERSION: u32 = 1;

/// Schema version for [`ReservationTransition`] inputs understood by v1 helpers.
pub const RESERVATION_TRANSITION_SCHEMA_VERSION: u32 = 1;

/// Structural reservation state lane (closed set).
pub const RESERVATION_STATE_RESERVED: u32 = 0;
/// Reservation fulfilled and closed.
pub const RESERVATION_STATE_COMMITTED: u32 = 1;
/// Reservation released without commit.
pub const RESERVATION_STATE_REFUNDED: u32 = 2;
/// Reservation lapsed without commit.
pub const RESERVATION_STATE_EXPIRED: u32 = 3;
/// Reservation abandoned while still outstanding.
pub const RESERVATION_STATE_ORPHANED: u32 = 4;

/// Open a new reservation.
pub const RESERVATION_OP_RESERVE: u32 = 0;
/// Commit a reserved slot.
pub const RESERVATION_OP_COMMIT: u32 = 1;
/// Refund/release a reserved slot before commit.
pub const RESERVATION_OP_REFUND: u32 = 2;
/// Mark a reserved slot as expired.
pub const RESERVATION_OP_EXPIRE: u32 = 3;
/// Mark a reserved slot as orphaned (structural hygiene).
pub const RESERVATION_OP_ORPHAN: u32 = 4;

/// Attempted second commit on an already committed reservation.
pub const RESERVATION_REASON_DOUBLE_COMMIT: u32 = 1;
/// Commit when no reservation exists.
pub const RESERVATION_REASON_COMMIT_WITHOUT_RESERVE: u32 = 2;
/// Refund when not in reserved lane.
pub const RESERVATION_REASON_REFUND_INVALID_STATE: u32 = 3;
/// Refund after commit (terminal committed lane).
pub const RESERVATION_REASON_REFUND_AFTER_COMMIT: u32 = 4;
/// Expire when not reserved.
pub const RESERVATION_REASON_EXPIRE_INVALID_STATE: u32 = 5;
/// Orphan when not reserved.
pub const RESERVATION_REASON_ORPHAN_INVALID_STATE: u32 = 6;
/// Second reserve for the same id.
pub const RESERVATION_REASON_DUPLICATE_RESERVE: u32 = 7;
/// Reserve missing subject or zero units.
pub const RESERVATION_REASON_RESERVE_INVALID_SUBJECT_OR_UNITS: u32 = 8;
/// Transition applied to a terminal non-reserved lane.
pub const RESERVATION_REASON_TRANSITION_ON_TERMINAL: u32 = 9;

/// Structural state lane (`RESERVATION_STATE_*` constants).
pub type ReservationState = u32;

/// Stable reservation identity (digest-sized).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ReservationId(pub [u8; 32]);

/// Opaque subject reference (`key_bytes` are caller-defined bytes and are never reordered).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReservationSubjectRef {
    /// Caller-defined namespace discriminant.
    pub namespace: u32,
    /// Opaque subject key material.
    pub key_bytes: Vec<u8>,
}

impl PartialOrd for ReservationSubjectRef {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ReservationSubjectRef {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.namespace
            .cmp(&other.namespace)
            .then_with(|| self.key_bytes.cmp(&other.key_bytes))
    }
}

/// Dimensionless quantity (no currency or stock semantics).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ReservationQuantity {
    /// Count of abstract units held by the reservation.
    pub units: u64,
}

/// Opaque cause reference (sorted before canonical transition hashing).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ReservationCauseRef {
    /// Caller-defined lane.
    pub lane: u32,
    /// Opaque key bytes (lexicographic tie-break after `lane`).
    pub opaque_key: Vec<u8>,
}

/// One explicit ledger operation (apply in ascending [`ReservationTransition::sequence`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReservationTransition {
    /// Must be `1` for v1 transition encoding helpers.
    pub schema_version: u32,
    /// Monotonic sequence key chosen by the caller (ties broken by [`ReservationId`]).
    pub sequence: u64,
    /// Target reservation id.
    pub reservation_id: ReservationId,
    /// Operation discriminant (see [`RESERVATION_OP_RESERVE`] … [`RESERVATION_OP_ORPHAN`]).
    pub op: u32,
    /// Units for [`RESERVATION_OP_RESERVE`] only (ignored for other ops).
    pub quantity_units: u64,
    /// Required for reserve; omitted for other ops.
    pub subject: Option<ReservationSubjectRef>,
    /// Cause refs; normalized by sorting before hashing.
    pub cause_refs: Vec<ReservationCauseRef>,
}

/// One row in the simulated ledger.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReservationEntry {
    /// Stable id for this reservation.
    pub reservation_id: ReservationId,
    /// Subject the reservation is bound to.
    pub subject_ref: ReservationSubjectRef,
    /// Outstanding units (unchanged by commit/refund lanes in v1).
    pub quantity: ReservationQuantity,
    /// Structural state lane (see `RESERVATION_STATE_*`).
    pub state: ReservationState,
    /// Sequence of the opening reserve.
    pub opened_at_sequence: u64,
}

/// Structural ledger finding (sorted before report `body_hash`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ReservationFinding {
    /// Illegal or inconsistent operation for the current lane.
    InvalidTransition {
        /// Target reservation id.
        reservation_id: ReservationId,
        /// State before the attempted op (best-effort `u32::MAX` when missing).
        from_state: u32,
        /// Attempted op ([`RESERVATION_OP_RESERVE`] …).
        attempted_op: u32,
        /// Stable reason code (see `RESERVATION_REASON_*`).
        reason_code: u32,
    },
    /// Transition schema version is not supported by these v1 helpers.
    UnsupportedTransitionSchemaVersion {
        /// Target reservation id.
        reservation_id: ReservationId,
        /// Observed transition schema version.
        observed: u32,
        /// Supported transition schema version.
        expected: u32,
    },
}

/// Canonical ledger report body after simulating normalized transitions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReservationLedgerReportBody {
    /// Must equal [`RESERVATION_LEDGER_REPORT_SCHEMA_VERSION`] for v1.
    pub schema_version: u32,
    /// Digest over canonical normalized transition bytes (see [`reservation_transition_log_digest`]).
    pub transition_log_digest: [u8; 32],
    /// Ledger rows sorted by [`ReservationId`].
    pub entries_sorted: Vec<ReservationEntry>,
    /// Structural findings (sorted before [`reservation_ledger_report_body_hash`]).
    pub findings_sorted: Vec<ReservationFinding>,
}

/// Reconciliation view over structural terminal and outstanding lanes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReservationReconciliationReportBody {
    /// Must equal [`RESERVATION_RECONCILIATION_REPORT_SCHEMA_VERSION`] for v1.
    pub schema_version: u32,
    /// Ids still in reserved lane.
    pub reserved_open_ids: Vec<ReservationId>,
    /// Ids in expired lane.
    pub expired_ids: Vec<ReservationId>,
    /// Ids in orphaned lane.
    pub orphaned_ids: Vec<ReservationId>,
    /// Ids in committed lane.
    pub committed_ids: Vec<ReservationId>,
    /// Ids in refunded lane.
    pub refunded_ids: Vec<ReservationId>,
}

/// Digest width for reservation reports.
pub type ReservationDigest = [u8; 32];

/// Normalize subject ref.
///
/// `key_bytes` are opaque caller material and are preserved byte-for-byte.
#[must_use]
pub fn normalize_reservation_subject_ref(subject: &ReservationSubjectRef) -> ReservationSubjectRef {
    subject.clone()
}

/// Normalize transition for hashing (sorts `cause_refs`).
#[must_use]
pub fn normalize_reservation_transition(t: &ReservationTransition) -> ReservationTransition {
    let mut cause_refs = t.cause_refs.clone();
    cause_refs.sort();
    ReservationTransition {
        cause_refs,
        ..t.clone()
    }
}

/// Canonical bytes for a normalized transition.
///
/// # Errors
/// MessagePack encode failure from `rmp-serde`.
pub fn reservation_transition_bytes(
    t: &ReservationTransition,
) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    let n = normalize_reservation_transition(t);
    crate::encoding::to_bytes(&n)
}

/// Sort transitions by `(sequence, reservation_id)` then normalize each.
#[must_use]
pub fn normalize_reservation_transition_list(
    transitions: &[ReservationTransition],
) -> Vec<ReservationTransition> {
    let mut out: Vec<ReservationTransition> = transitions
        .iter()
        .map(normalize_reservation_transition)
        .collect();
    out.sort_by(|a, b| {
        a.sequence
            .cmp(&b.sequence)
            .then_with(|| a.reservation_id.cmp(&b.reservation_id))
    });
    out
}

/// Digest over concatenated canonical transition bytes (deterministic for a normalized list).
///
/// # Errors
/// MessagePack encode failure from `rmp-serde`.
pub fn reservation_transition_log_digest(
    transitions_sorted: &[ReservationTransition],
) -> Result<ReservationDigest, rmp_serde::encode::Error> {
    let mut buf = Vec::new();
    for t in transitions_sorted {
        buf.extend_from_slice(&reservation_transition_bytes(t)?);
    }
    Ok(content_hash(&buf))
}

fn push_invalid(
    out: &mut Vec<ReservationFinding>,
    id: ReservationId,
    from: u32,
    op: u32,
    reason: u32,
) {
    out.push(ReservationFinding::InvalidTransition {
        reservation_id: id,
        from_state: from,
        attempted_op: op,
        reason_code: reason,
    });
}

/// Simulate transitions and return a canonical ledger report body.
///
/// # Errors
/// MessagePack encode failure while computing the transition log digest.
pub fn simulate_reservation_ledger(
    transitions: &[ReservationTransition],
) -> Result<ReservationLedgerReportBody, rmp_serde::encode::Error> {
    let sorted = normalize_reservation_transition_list(transitions);
    let digest = reservation_transition_log_digest(&sorted)?;
    let mut findings = Vec::new();
    let mut ledger: BTreeMap<ReservationId, ReservationEntry> = BTreeMap::new();

    for t in &sorted {
        if t.schema_version != RESERVATION_TRANSITION_SCHEMA_VERSION {
            findings.push(ReservationFinding::UnsupportedTransitionSchemaVersion {
                reservation_id: t.reservation_id,
                observed: t.schema_version,
                expected: RESERVATION_TRANSITION_SCHEMA_VERSION,
            });
            continue;
        }
        let id = t.reservation_id;
        match t.op {
            RESERVATION_OP_RESERVE => {
                if let Some(existing) = ledger.get(&id) {
                    push_invalid(
                        &mut findings,
                        id,
                        existing.state,
                        t.op,
                        RESERVATION_REASON_DUPLICATE_RESERVE,
                    );
                    continue;
                }
                let Some(subject) = t.subject.as_ref() else {
                    push_invalid(
                        &mut findings,
                        id,
                        u32::MAX,
                        t.op,
                        RESERVATION_REASON_RESERVE_INVALID_SUBJECT_OR_UNITS,
                    );
                    continue;
                };
                if t.quantity_units == 0 {
                    push_invalid(
                        &mut findings,
                        id,
                        u32::MAX,
                        t.op,
                        RESERVATION_REASON_RESERVE_INVALID_SUBJECT_OR_UNITS,
                    );
                    continue;
                }
                let subject_ref = normalize_reservation_subject_ref(subject);
                ledger.insert(
                    id,
                    ReservationEntry {
                        reservation_id: id,
                        subject_ref,
                        quantity: ReservationQuantity {
                            units: t.quantity_units,
                        },
                        state: RESERVATION_STATE_RESERVED,
                        opened_at_sequence: t.sequence,
                    },
                );
            }
            RESERVATION_OP_COMMIT => {
                let Some(entry) = ledger.get_mut(&id) else {
                    push_invalid(
                        &mut findings,
                        id,
                        u32::MAX,
                        t.op,
                        RESERVATION_REASON_COMMIT_WITHOUT_RESERVE,
                    );
                    continue;
                };
                match entry.state {
                    RESERVATION_STATE_RESERVED => entry.state = RESERVATION_STATE_COMMITTED,
                    RESERVATION_STATE_COMMITTED => {
                        push_invalid(
                            &mut findings,
                            id,
                            entry.state,
                            t.op,
                            RESERVATION_REASON_DOUBLE_COMMIT,
                        );
                    }
                    _ => {
                        push_invalid(
                            &mut findings,
                            id,
                            entry.state,
                            t.op,
                            RESERVATION_REASON_TRANSITION_ON_TERMINAL,
                        );
                    }
                }
            }
            RESERVATION_OP_REFUND => {
                let Some(entry) = ledger.get_mut(&id) else {
                    push_invalid(
                        &mut findings,
                        id,
                        u32::MAX,
                        t.op,
                        RESERVATION_REASON_REFUND_INVALID_STATE,
                    );
                    continue;
                };
                match entry.state {
                    RESERVATION_STATE_RESERVED => entry.state = RESERVATION_STATE_REFUNDED,
                    RESERVATION_STATE_COMMITTED => {
                        push_invalid(
                            &mut findings,
                            id,
                            entry.state,
                            t.op,
                            RESERVATION_REASON_REFUND_AFTER_COMMIT,
                        );
                    }
                    _ => {
                        push_invalid(
                            &mut findings,
                            id,
                            entry.state,
                            t.op,
                            RESERVATION_REASON_REFUND_INVALID_STATE,
                        );
                    }
                }
            }
            RESERVATION_OP_EXPIRE => {
                let Some(entry) = ledger.get_mut(&id) else {
                    push_invalid(
                        &mut findings,
                        id,
                        u32::MAX,
                        t.op,
                        RESERVATION_REASON_EXPIRE_INVALID_STATE,
                    );
                    continue;
                };
                if entry.state == RESERVATION_STATE_RESERVED {
                    entry.state = RESERVATION_STATE_EXPIRED;
                } else {
                    push_invalid(
                        &mut findings,
                        id,
                        entry.state,
                        t.op,
                        RESERVATION_REASON_EXPIRE_INVALID_STATE,
                    );
                }
            }
            RESERVATION_OP_ORPHAN => {
                let Some(entry) = ledger.get_mut(&id) else {
                    push_invalid(
                        &mut findings,
                        id,
                        u32::MAX,
                        t.op,
                        RESERVATION_REASON_ORPHAN_INVALID_STATE,
                    );
                    continue;
                };
                if entry.state == RESERVATION_STATE_RESERVED {
                    entry.state = RESERVATION_STATE_ORPHANED;
                } else {
                    push_invalid(
                        &mut findings,
                        id,
                        entry.state,
                        t.op,
                        RESERVATION_REASON_ORPHAN_INVALID_STATE,
                    );
                }
            }
            _ => {
                push_invalid(
                    &mut findings,
                    id,
                    u32::MAX,
                    t.op,
                    RESERVATION_REASON_TRANSITION_ON_TERMINAL,
                );
            }
        }
    }

    sort_findings(&mut findings);
    let mut entries_sorted: Vec<ReservationEntry> = ledger.into_values().collect();
    entries_sorted.sort_by(|a, b| a.reservation_id.cmp(&b.reservation_id));

    Ok(ReservationLedgerReportBody {
        schema_version: RESERVATION_LEDGER_REPORT_SCHEMA_VERSION,
        transition_log_digest: digest,
        entries_sorted,
        findings_sorted: findings,
    })
}

/// Build a reconciliation report from ledger entries (each bucket sorted by id).
#[must_use]
pub fn reservation_reconciliation_report(
    entries: &[ReservationEntry],
) -> ReservationReconciliationReportBody {
    let mut reserved_open_ids = Vec::new();
    let mut expired_ids = Vec::new();
    let mut orphaned_ids = Vec::new();
    let mut committed_ids = Vec::new();
    let mut refunded_ids = Vec::new();
    for e in entries {
        match e.state {
            RESERVATION_STATE_RESERVED => reserved_open_ids.push(e.reservation_id),
            RESERVATION_STATE_EXPIRED => expired_ids.push(e.reservation_id),
            RESERVATION_STATE_ORPHANED => orphaned_ids.push(e.reservation_id),
            RESERVATION_STATE_COMMITTED => committed_ids.push(e.reservation_id),
            RESERVATION_STATE_REFUNDED => refunded_ids.push(e.reservation_id),
            _ => {}
        }
    }
    reserved_open_ids.sort();
    expired_ids.sort();
    orphaned_ids.sort();
    committed_ids.sort();
    refunded_ids.sort();
    ReservationReconciliationReportBody {
        schema_version: RESERVATION_RECONCILIATION_REPORT_SCHEMA_VERSION,
        reserved_open_ids,
        expired_ids,
        orphaned_ids,
        committed_ids,
        refunded_ids,
    }
}

/// Deterministic digest over [`ReservationLedgerReportBody`] (sorts `findings_sorted` clone).
///
/// # Errors
/// MessagePack encode failure from `rmp-serde`.
pub fn reservation_ledger_report_body_hash(
    body: &ReservationLedgerReportBody,
) -> Result<ReservationDigest, rmp_serde::encode::Error> {
    let findings_sorted = sorted_findings(&body.findings_sorted);
    let mut entries_sorted = body.entries_sorted.clone();
    entries_sorted.sort_by(|a, b| a.reservation_id.cmp(&b.reservation_id));
    let normalized = ReservationLedgerReportBody {
        findings_sorted,
        entries_sorted,
        ..body.clone()
    };
    let bytes = crate::encoding::to_bytes(&normalized)?;
    Ok(content_hash(&bytes))
}

/// Deterministic digest over [`ReservationReconciliationReportBody`].
///
/// # Errors
/// MessagePack encode failure from `rmp-serde`.
pub fn reservation_reconciliation_report_body_hash(
    body: &ReservationReconciliationReportBody,
) -> Result<ReservationDigest, rmp_serde::encode::Error> {
    let bytes = crate::encoding::to_bytes(body)?;
    Ok(content_hash(&bytes))
}
