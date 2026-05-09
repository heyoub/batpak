//! Batpak Substrate Closure state transition evidence: opaque machine/subject identifiers, prior
//! and successor state discriminants, transition identity, sorted cause references, optional
//! ordering metadata, and deterministic reports with structural findings only.
//!
//! This module does **not** import [`crate::store`]. It encodes no policy, scheduler, or
//! application-specific lifecycle vocabulary; callers supply state `u64` lanes and optional
//! allowed-edge sets.

use crate::evidence::{content_hash, sort_findings, sorted_findings};
use serde::{Deserialize, Serialize};

/// Schema version for canonical [`StateTransitionEvent`] encoding.
pub const STATE_TRANSITION_EVENT_SCHEMA_VERSION: u32 = 1;

/// Schema version for canonical [`StateTransitionReportBody`] encoding.
pub const STATE_TRANSITION_REPORT_SCHEMA_VERSION: u32 = 1;

/// `(previous_state, next_state)` is not an element of the supplied allowed-edge set ([`StateTransitionFinding::InvalidTransition`]).
pub const TRANSITION_INVALID_DISALLOWED_EDGE: u32 = 1;

/// Opaque stable machine identity (digest-sized).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TransitionMachineId(pub [u8; 32]);

/// Opaque subject identity scoped by the caller (digest-sized).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TransitionSubjectId(pub [u8; 32]);

/// Opaque transition occurrence identity (digest-sized).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TransitionId(pub [u8; 32]);

/// Opaque cause reference (sorted before canonical event hashing).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TransitionCauseRef {
    /// Caller-defined cause lane.
    pub lane: u32,
    /// Opaque key material (lexicographic tie-break after `lane`).
    pub opaque_key: Vec<u8>,
}

/// Canonical transition **event** body (immutable evidence of a single step).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateTransitionEvent {
    /// Must equal [`STATE_TRANSITION_EVENT_SCHEMA_VERSION`] for v1 helpers.
    pub schema_version: u32,
    /// Caller-scoped state machine identity.
    pub machine_id: TransitionMachineId,
    /// Caller-scoped subject identity.
    pub subject_id: TransitionSubjectId,
    /// Prior state discriminant (caller-defined).
    pub previous_state: u64,
    /// Successor state discriminant (caller-defined).
    pub next_state: u64,
    /// Unique transition occurrence id chosen by the caller.
    pub transition_id: TransitionId,
    /// Cause references; normalized by sorting before hashing.
    pub causes: Vec<TransitionCauseRef>,
    /// Optional monotonic ordering key when one exists (watermark, sequence, etc.).
    pub ordering_sequence: Option<u64>,
    /// Optional digest tying this transition to a frontier or snapshot witness.
    pub frontier_digest: Option<[u8; 32]>,
}

/// Structural finding for transition validation (sorted before report `body_hash`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum StateTransitionFinding {
    /// Event schema version is not supported by these v1 helpers.
    UnsupportedEventSchemaVersion {
        /// Observed event schema version.
        observed: u32,
        /// Supported event schema version.
        expected: u32,
    },
    /// `(previous_state, next_state)` is not allowed by the supplied edge set.
    InvalidTransition {
        /// Machine id copied from the event.
        machine_id: TransitionMachineId,
        /// Subject id copied from the event.
        subject_id: TransitionSubjectId,
        /// Prior state from the event.
        from_state: u64,
        /// Target state from the event.
        to_state: u64,
        /// Stable reason code (e.g. [`TRANSITION_INVALID_DISALLOWED_EDGE`]).
        reason_code: u32,
    },
    /// `causes` were not already sorted in the supplied event (structural hygiene).
    UnsortedCausesInSourceEvent,
    /// `allowed_edges` input was not sorted ascending by `(from,to)` pair.
    UnsortedAllowedTransitionEdges,
}

/// Deterministic report body over a transition event and structural findings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateTransitionReportBody {
    /// Must equal [`STATE_TRANSITION_REPORT_SCHEMA_VERSION`] for v1.
    pub schema_version: u32,
    /// Digest of canonical [`StateTransitionEvent`] bytes.
    pub transition_event_digest: [u8; 32],
    /// Identity columns copied for queryability without re-decoding the event.
    pub machine_id: TransitionMachineId,
    /// Subject identity copied from the event.
    pub subject_id: TransitionSubjectId,
    /// Prior state discriminant copied from the event.
    pub previous_state: u64,
    /// Successor state discriminant copied from the event.
    pub next_state: u64,
    /// Transition occurrence id copied from the event.
    pub transition_id: TransitionId,
    /// Causes in canonical sorted order (matches normalized event).
    pub causes_sorted: Vec<TransitionCauseRef>,
    /// Optional ordering key copied from the event.
    pub ordering_sequence: Option<u64>,
    /// Optional frontier digest copied from the event.
    pub frontier_digest: Option<[u8; 32]>,
    /// Findings (sorted before [`state_transition_report_body_hash`]).
    pub findings: Vec<StateTransitionFinding>,
}

/// Alias for callers that prefer the shorter report name.
pub type StateTransitionReport = StateTransitionReportBody;

/// Digest width for transition evidence (matches other substrate digests).
pub type TransitionEvidenceDigest = [u8; 32];

/// Normalize event for canonical digest (sorts `causes`).
#[must_use]
pub fn normalize_state_transition_event(event: &StateTransitionEvent) -> StateTransitionEvent {
    let mut causes = event.causes.clone();
    causes.sort();
    StateTransitionEvent {
        causes,
        ..event.clone()
    }
}

/// Canonical MessagePack bytes for the normalized transition event.
///
/// # Errors
/// MessagePack encode failure from `rmp-serde`.
pub fn state_transition_event_bytes(
    event: &StateTransitionEvent,
) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    let normalized = normalize_state_transition_event(event);
    crate::encoding::to_bytes(&normalized)
}

/// Digest of canonical normalized transition event bytes.
///
/// # Errors
/// MessagePack encode failure from `rmp-serde`.
pub fn state_transition_event_digest(
    event: &StateTransitionEvent,
) -> Result<TransitionEvidenceDigest, rmp_serde::encode::Error> {
    let bytes = state_transition_event_bytes(event)?;
    Ok(content_hash(&bytes))
}

/// True when `causes` are already sorted (no structural reordering).
#[must_use]
pub fn transition_causes_are_sorted(causes: &[TransitionCauseRef]) -> bool {
    causes.windows(2).all(|w| w[0] <= w[1])
}

/// True when `allowed_edges` is sorted by `(from_state, to_state)` ascending.
#[must_use]
pub fn allowed_transition_edges_are_sorted(allowed_edges: &[(u64, u64)]) -> bool {
    allowed_edges.windows(2).all(|w| w[0] <= w[1])
}

fn edge_allowed(allowed_edges_sorted: &[(u64, u64)], from: u64, to: u64) -> bool {
    allowed_edges_sorted
        .binary_search_by(|e| (e.0, e.1).cmp(&(from, to)))
        .is_ok()
}

/// Build a structural report for `event` against a sorted allowed-edge set.
///
/// `allowed_edges` must be sorted by `(from_state, to_state)`; if not, a finding is recorded and
/// evaluation still uses a sorted copy for edge membership.
///
/// # Errors
/// MessagePack encode failure from `rmp-serde` while hashing the event.
pub fn build_state_transition_report(
    event: &StateTransitionEvent,
    allowed_edges: &[(u64, u64)],
) -> Result<StateTransitionReportBody, rmp_serde::encode::Error> {
    let mut findings = Vec::new();
    if event.schema_version != STATE_TRANSITION_EVENT_SCHEMA_VERSION {
        findings.push(StateTransitionFinding::UnsupportedEventSchemaVersion {
            observed: event.schema_version,
            expected: STATE_TRANSITION_EVENT_SCHEMA_VERSION,
        });
    }
    if !transition_causes_are_sorted(&event.causes) {
        findings.push(StateTransitionFinding::UnsortedCausesInSourceEvent);
    }
    let mut edges_sorted: Vec<(u64, u64)> = allowed_edges.to_vec();
    edges_sorted.sort();
    if edges_sorted.as_slice() != allowed_edges {
        findings.push(StateTransitionFinding::UnsortedAllowedTransitionEdges);
    }
    let allowed = edges_sorted.as_slice();

    if !edge_allowed(allowed, event.previous_state, event.next_state) {
        findings.push(StateTransitionFinding::InvalidTransition {
            machine_id: event.machine_id,
            subject_id: event.subject_id,
            from_state: event.previous_state,
            to_state: event.next_state,
            reason_code: TRANSITION_INVALID_DISALLOWED_EDGE,
        });
    }

    let normalized = normalize_state_transition_event(event);
    let transition_event_digest = state_transition_event_digest(event)?;
    sort_findings(&mut findings);

    Ok(StateTransitionReportBody {
        schema_version: STATE_TRANSITION_REPORT_SCHEMA_VERSION,
        transition_event_digest,
        machine_id: normalized.machine_id,
        subject_id: normalized.subject_id,
        previous_state: normalized.previous_state,
        next_state: normalized.next_state,
        transition_id: normalized.transition_id,
        causes_sorted: normalized.causes,
        ordering_sequence: normalized.ordering_sequence,
        frontier_digest: normalized.frontier_digest,
        findings,
    })
}

/// Deterministic digest over [`StateTransitionReportBody`] (sorts `findings` clone).
///
/// # Errors
/// MessagePack encode failure from `rmp-serde`.
pub fn state_transition_report_body_hash(
    report: &StateTransitionReportBody,
) -> Result<TransitionEvidenceDigest, rmp_serde::encode::Error> {
    let findings = sorted_findings(&report.findings);
    let normalized = StateTransitionReportBody {
        findings,
        ..report.clone()
    };
    let bytes = crate::encoding::to_bytes(&normalized)?;
    Ok(content_hash(&bytes))
}
