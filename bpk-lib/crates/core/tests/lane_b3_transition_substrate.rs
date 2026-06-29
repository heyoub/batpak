//! PROVES: transition event digest sorts `causes`; report `body_hash` sorts findings; allowed-edge
//! evaluation is deterministic on sorted edge inputs.
//! CATCHES: unsorted causes/edges structural findings; disallowed `(from,to)` edges.
//! SEEDED: fixed machine/subject/transition ids and synthetic state lanes only.

use batpak::encoding;
use batpak::transition::{
    allowed_transition_edges_are_sorted, build_state_transition_report,
    normalize_state_transition_event, state_transition_event_bytes, state_transition_event_digest,
    state_transition_report_body_hash, transition_causes_are_sorted, StateTransitionEvent,
    StateTransitionFinding, StateTransitionReportBody, TransitionCauseRef,
    TransitionEvidenceDigest, TransitionId, TransitionMachineId, TransitionSubjectId,
    STATE_TRANSITION_EVENT_SCHEMA_VERSION, STATE_TRANSITION_REPORT_SCHEMA_VERSION,
    TRANSITION_INVALID_DISALLOWED_EDGE,
};

fn mid(b: u8) -> TransitionMachineId {
    TransitionMachineId([b; 32])
}

fn sid(b: u8) -> TransitionSubjectId {
    TransitionSubjectId([b; 32])
}

fn tid(b: u8) -> TransitionId {
    TransitionId([b; 32])
}

fn sample_event(prev: u64, next: u64) -> StateTransitionEvent {
    StateTransitionEvent {
        schema_version: STATE_TRANSITION_EVENT_SCHEMA_VERSION,
        machine_id: mid(1),
        subject_id: sid(2),
        previous_state: prev,
        next_state: next,
        transition_id: tid(3),
        causes: vec![
            TransitionCauseRef {
                lane: 2,
                opaque_key: vec![2u8],
            },
            TransitionCauseRef {
                lane: 1,
                opaque_key: vec![1u8],
            },
        ],
        ordering_sequence: Some(10),
        frontier_digest: Some([7u8; 32]),
    }
}

#[test]
fn transition_event_digest_sorts_causes() {
    let a = sample_event(0, 1);
    let mut b = a.clone();
    b.causes.reverse();
    let da = state_transition_event_digest(&a).expect("d a");
    let db = state_transition_event_digest(&b).expect("d b");
    assert_eq!(da, db, "PROPERTY: cause order must not change event digest");
}

#[test]
fn transition_different_step_changes_digest() {
    let a = sample_event(0, 1);
    let mut b = a.clone();
    b.next_state = 2;
    assert_ne!(
        state_transition_event_digest(&a).expect("a"),
        state_transition_event_digest(&b).expect("b")
    );
}

#[test]
fn transition_legal_report_empty_findings_stable() {
    let ev = sample_event(0, 1);
    let mut causes = ev.causes.clone();
    causes.sort();
    let ev_sorted = StateTransitionEvent {
        causes,
        ..ev.clone()
    };
    let edges = [(0u64, 1u64), (1, 2)];
    assert!(allowed_transition_edges_are_sorted(&edges));
    let r1 = build_state_transition_report(&ev_sorted, &edges).expect("r1");
    let r2 = build_state_transition_report(&ev_sorted, &edges).expect("r2");
    assert!(r1.findings.is_empty());
    assert_eq!(
        state_transition_report_body_hash(&r1).expect("h1"),
        state_transition_report_body_hash(&r2).expect("h2")
    );
}

#[test]
fn transition_invalid_edge_finding() {
    let ev = sample_event(0, 2);
    let mut c = ev.causes.clone();
    c.sort();
    let ev_ok = StateTransitionEvent { causes: c, ..ev };
    let edges = [(0u64, 1u64)];
    let r = build_state_transition_report(&ev_ok, &edges).expect("report");
    assert!(
        r.findings.iter().any(|f| matches!(
            f,
            StateTransitionFinding::InvalidTransition {
                reason_code: TRANSITION_INVALID_DISALLOWED_EDGE,
                from_state: 0,
                to_state: 2,
                ..
            }
        )),
        "{:?}",
        r.findings
    );
}

#[test]
fn transition_unsorted_causes_and_edges_findings() {
    let ev = sample_event(0, 1);
    let edges = [(1u64, 2u64), (0u64, 1u64)];
    let r = build_state_transition_report(&ev, &edges).expect("r");
    assert!(
        r.findings
            .iter()
            .any(|f| matches!(f, StateTransitionFinding::UnsortedCausesInSourceEvent)),
        "{:?}",
        r.findings
    );
    assert!(
        r.findings
            .iter()
            .any(|f| matches!(f, StateTransitionFinding::UnsortedAllowedTransitionEdges)),
        "{:?}",
        r.findings
    );
}

#[test]
fn transition_unsupported_event_schema_version_is_a_finding() {
    let mut ev = sample_event(0, 1);
    ev.schema_version = STATE_TRANSITION_EVENT_SCHEMA_VERSION + 1;
    ev.causes.sort();
    let edges = [(0u64, 1u64)];

    let report = build_state_transition_report(&ev, &edges).expect("report");

    assert!(
        report.findings.iter().any(|finding| matches!(
            finding,
            StateTransitionFinding::UnsupportedEventSchemaVersion { observed, expected }
                if *observed == STATE_TRANSITION_EVENT_SCHEMA_VERSION + 1
                    && *expected == STATE_TRANSITION_EVENT_SCHEMA_VERSION
        )),
        "{:?}",
        report.findings
    );
}

#[test]
fn transition_report_body_hash_sorts_findings() {
    let ev = sample_event(0, 1);
    let mut c = ev.causes.clone();
    c.sort();
    let ev_sorted = StateTransitionEvent { causes: c, ..ev };
    let edges = [(0u64, 1u64)];
    let mut r = build_state_transition_report(&ev_sorted, &edges).expect("r");
    r.findings
        .push(StateTransitionFinding::UnsortedCausesInSourceEvent);
    r.findings
        .push(StateTransitionFinding::UnsortedAllowedTransitionEdges);
    let h0 = state_transition_report_body_hash(&r).expect("h0");
    r.findings.reverse();
    let h1 = state_transition_report_body_hash(&r).expect("h1");
    assert_eq!(h0, h1, "PROPERTY: report body_hash must sort findings");
}

#[test]
fn transition_helpers_detect_sort_order() {
    let mut c = vec![
        TransitionCauseRef {
            lane: 2,
            opaque_key: vec![],
        },
        TransitionCauseRef {
            lane: 1,
            opaque_key: vec![],
        },
    ];
    assert!(!transition_causes_are_sorted(&c));
    c.sort();
    assert!(transition_causes_are_sorted(&c));
    let mut e = vec![(2u64, 0u64), (1, 0)];
    assert!(!allowed_transition_edges_are_sorted(&e));
    e.sort();
    assert!(allowed_transition_edges_are_sorted(&e));
}

#[test]
fn transition_event_bytes_roundtrip_encoding() {
    let ev = sample_event(0, 1);
    let bytes = state_transition_event_bytes(&ev).expect("bytes");
    let decoded: StateTransitionEvent = encoding::from_bytes(&bytes).expect("decode");
    assert_eq!(
        state_transition_event_digest(&ev).expect("d0"),
        state_transition_event_digest(&decoded).expect("d1")
    );
}

#[test]
fn transition_normalize_matches_digest() {
    let ev = sample_event(0, 1);
    let n = normalize_state_transition_event(&ev);
    assert!(transition_causes_are_sorted(&n.causes));
    assert_eq!(
        state_transition_event_digest(&ev).expect("e"),
        state_transition_event_digest(&n).expect("n")
    );
}

#[test]
fn transition_report_alias_and_schema_constants() {
    let ev = sample_event(0, 1);
    let mut c = ev.causes.clone();
    c.sort();
    let ev_sorted = StateTransitionEvent { causes: c, ..ev };
    let edges = [(0u64, 1u64)];
    let body: StateTransitionReportBody =
        build_state_transition_report(&ev_sorted, &edges).expect("b");
    let _body: StateTransitionReportBody = body.clone();
    assert_eq!(body.schema_version, STATE_TRANSITION_REPORT_SCHEMA_VERSION);
    let _d: TransitionEvidenceDigest = body.transition_event_digest;
}
