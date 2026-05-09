// justifies: INV-TEST-PANIC-AS-ASSERTION; Lane B4 reservation ledger doctrine uses panic for PROPERTY mismatches only.
#![allow(clippy::panic)]
//! PROVES: reservation simulation is deterministic on transition order normalization; ledger and
//! reconciliation report `body_hash` are stable; invalid ops emit sorted structural findings.
//! CATCHES: double commit, refund-after-commit, orphan/reconcile buckets.
//! SEEDED: fixed `ReservationId` fixtures and abstract `units` counts only.

use batpak::reservation::{
    normalize_reservation_subject_ref, normalize_reservation_transition,
    normalize_reservation_transition_list, reservation_ledger_report_body_hash,
    reservation_reconciliation_report, reservation_reconciliation_report_body_hash,
    reservation_transition_bytes, reservation_transition_log_digest, simulate_reservation_ledger,
    ReservationCauseRef, ReservationDigest, ReservationEntry, ReservationFinding, ReservationId,
    ReservationLedgerReportBody, ReservationQuantity, ReservationReconciliationReport,
    ReservationReconciliationReportBody, ReservationState, ReservationSubjectRef,
    ReservationTransition, RESERVATION_LEDGER_REPORT_SCHEMA_VERSION, RESERVATION_OP_COMMIT,
    RESERVATION_OP_EXPIRE, RESERVATION_OP_ORPHAN, RESERVATION_OP_REFUND, RESERVATION_OP_RESERVE,
    RESERVATION_REASON_COMMIT_WITHOUT_RESERVE, RESERVATION_REASON_DOUBLE_COMMIT,
    RESERVATION_REASON_DUPLICATE_RESERVE, RESERVATION_REASON_EXPIRE_INVALID_STATE,
    RESERVATION_REASON_ORPHAN_INVALID_STATE, RESERVATION_REASON_REFUND_AFTER_COMMIT,
    RESERVATION_REASON_REFUND_INVALID_STATE, RESERVATION_REASON_RESERVE_INVALID_SUBJECT_OR_UNITS,
    RESERVATION_REASON_TRANSITION_ON_TERMINAL, RESERVATION_RECONCILIATION_REPORT_SCHEMA_VERSION,
    RESERVATION_STATE_COMMITTED, RESERVATION_STATE_EXPIRED, RESERVATION_STATE_ORPHANED,
    RESERVATION_STATE_REFUNDED, RESERVATION_STATE_RESERVED, RESERVATION_TRANSITION_SCHEMA_VERSION,
};

fn rid(tag: u8) -> ReservationId {
    ReservationId([tag; 32])
}

fn subject(ns: u32, key: &[u8]) -> ReservationSubjectRef {
    ReservationSubjectRef {
        namespace: ns,
        key_bytes: key.to_vec(),
    }
}

#[test]
fn reservation_public_constants_are_closed_surface_witnesses() {
    let schema_versions = [
        RESERVATION_LEDGER_REPORT_SCHEMA_VERSION,
        RESERVATION_RECONCILIATION_REPORT_SCHEMA_VERSION,
        RESERVATION_TRANSITION_SCHEMA_VERSION,
    ];
    let states = [
        RESERVATION_STATE_RESERVED,
        RESERVATION_STATE_COMMITTED,
        RESERVATION_STATE_REFUNDED,
        RESERVATION_STATE_EXPIRED,
        RESERVATION_STATE_ORPHANED,
    ];
    let ops = [
        RESERVATION_OP_RESERVE,
        RESERVATION_OP_COMMIT,
        RESERVATION_OP_REFUND,
        RESERVATION_OP_EXPIRE,
        RESERVATION_OP_ORPHAN,
    ];
    let reasons = [
        RESERVATION_REASON_DOUBLE_COMMIT,
        RESERVATION_REASON_COMMIT_WITHOUT_RESERVE,
        RESERVATION_REASON_REFUND_INVALID_STATE,
        RESERVATION_REASON_REFUND_AFTER_COMMIT,
        RESERVATION_REASON_EXPIRE_INVALID_STATE,
        RESERVATION_REASON_ORPHAN_INVALID_STATE,
        RESERVATION_REASON_DUPLICATE_RESERVE,
        RESERVATION_REASON_RESERVE_INVALID_SUBJECT_OR_UNITS,
        RESERVATION_REASON_TRANSITION_ON_TERMINAL,
    ];

    assert_eq!(schema_versions, [1, 1, 1]);
    assert_eq!(states, [0, 1, 2, 3, 4]);
    assert_eq!(ops, [0, 1, 2, 3, 4]);
    assert_eq!(reasons, [1, 2, 3, 4, 5, 6, 7, 8, 9]);
}

fn tx(
    seq: u64,
    id: ReservationId,
    op: u32,
    units: u64,
    subj: Option<ReservationSubjectRef>,
) -> ReservationTransition {
    let cause_refs = [
        ReservationCauseRef {
            lane: 2,
            opaque_key: vec![2],
        },
        ReservationCauseRef {
            lane: 1,
            opaque_key: vec![1],
        },
    ];
    ReservationTransition {
        schema_version: RESERVATION_TRANSITION_SCHEMA_VERSION,
        sequence: seq,
        reservation_id: id,
        op,
        quantity_units: units,
        subject: subj,
        cause_refs: cause_refs.to_vec(),
    }
}

#[test]
fn reservation_reserve_commit_happy_path() {
    let r = rid(1);
    let t = vec![
        tx(1, r, RESERVATION_OP_RESERVE, 3, Some(subject(1, b"k"))),
        tx(2, r, RESERVATION_OP_COMMIT, 0, None),
    ];
    let rep = simulate_reservation_ledger(&t).expect("sim");
    assert!(rep.findings_sorted.is_empty());
    assert_eq!(rep.entries_sorted.len(), 1);
    assert_eq!(rep.entries_sorted[0].state, RESERVATION_STATE_COMMITTED);
}

#[test]
fn reservation_reserve_refund() {
    let r = rid(2);
    let t = vec![
        tx(1, r, RESERVATION_OP_RESERVE, 1, Some(subject(1, b"x"))),
        tx(2, r, RESERVATION_OP_REFUND, 0, None),
    ];
    let rep = simulate_reservation_ledger(&t).expect("sim");
    assert!(rep.findings_sorted.is_empty());
    assert_eq!(rep.entries_sorted[0].state, RESERVATION_STATE_REFUNDED);
}

#[test]
fn reservation_reserve_expire() {
    let r = rid(3);
    let t = vec![
        tx(1, r, RESERVATION_OP_RESERVE, 2, Some(subject(1, b"y"))),
        tx(2, r, RESERVATION_OP_EXPIRE, 0, None),
    ];
    let rep = simulate_reservation_ledger(&t).expect("sim");
    assert!(rep.findings_sorted.is_empty());
    assert_eq!(rep.entries_sorted[0].state, RESERVATION_STATE_EXPIRED);
}

#[test]
fn reservation_double_commit_finding() {
    let r = rid(4);
    let t = vec![
        tx(1, r, RESERVATION_OP_RESERVE, 1, Some(subject(1, b"z"))),
        tx(2, r, RESERVATION_OP_COMMIT, 0, None),
        tx(3, r, RESERVATION_OP_COMMIT, 0, None),
    ];
    let rep = simulate_reservation_ledger(&t).expect("sim");
    assert!(
        rep.findings_sorted.iter().any(|f| matches!(
            f,
            ReservationFinding::InvalidTransition {
                reason_code: RESERVATION_REASON_DOUBLE_COMMIT,
                ..
            }
        )),
        "{:?}",
        rep.findings_sorted
    );
}

#[test]
fn reservation_refund_after_commit_rejected() {
    let r = rid(5);
    let t = vec![
        tx(1, r, RESERVATION_OP_RESERVE, 1, Some(subject(1, b"a"))),
        tx(2, r, RESERVATION_OP_COMMIT, 0, None),
        tx(3, r, RESERVATION_OP_REFUND, 0, None),
    ];
    let rep = simulate_reservation_ledger(&t).expect("sim");
    assert!(
        rep.findings_sorted.iter().any(|f| matches!(
            f,
            ReservationFinding::InvalidTransition {
                reason_code: RESERVATION_REASON_REFUND_AFTER_COMMIT,
                ..
            }
        )),
        "{:?}",
        rep.findings_sorted
    );
}

#[test]
fn reservation_orphan_and_reconciliation_deterministic() {
    let a = rid(6);
    let b = rid(7);
    let t = vec![
        tx(1, a, RESERVATION_OP_RESERVE, 1, Some(subject(1, b"p"))),
        tx(2, b, RESERVATION_OP_RESERVE, 1, Some(subject(1, b"q"))),
        tx(3, a, RESERVATION_OP_ORPHAN, 0, None),
        tx(4, b, RESERVATION_OP_EXPIRE, 0, None),
    ];
    let rep = simulate_reservation_ledger(&t).expect("sim");
    let rec = reservation_reconciliation_report(&rep.entries_sorted);
    let reconciliation_schema_version = RESERVATION_RECONCILIATION_REPORT_SCHEMA_VERSION;
    assert_eq!(rec.schema_version, reconciliation_schema_version);
    let h0 = reservation_reconciliation_report_body_hash(&rec).expect("rh");
    let h1 = reservation_reconciliation_report_body_hash(&rec).expect("rh2");
    assert_eq!(h0, h1);
    assert_eq!(rec.orphaned_ids, vec![a]);
    assert_eq!(rec.expired_ids, vec![b]);
    assert_eq!(rep.entries_sorted[0].state, RESERVATION_STATE_ORPHANED);
    assert_eq!(rep.entries_sorted[1].state, RESERVATION_STATE_EXPIRED);
    let _e: &ReservationEntry = &rep.entries_sorted[0];
    let _q: ReservationQuantity = rep.entries_sorted[0].quantity;
    let _alias: ReservationReconciliationReport = rec.clone();
    let _rec_body: ReservationReconciliationReportBody = rec.clone();
}

#[test]
fn reservation_ledger_hash_stable_under_transition_permutation() {
    let rb = rid(8);
    let rc = rid(9);
    let t1 = vec![
        tx(1, rb, RESERVATION_OP_RESERVE, 1, Some(subject(1, b"m"))),
        tx(2, rc, RESERVATION_OP_RESERVE, 1, Some(subject(1, b"n"))),
        tx(3, rb, RESERVATION_OP_COMMIT, 0, None),
        tx(4, rc, RESERVATION_OP_COMMIT, 0, None),
    ];
    let t2 = vec![
        tx(3, rb, RESERVATION_OP_COMMIT, 0, None),
        tx(1, rb, RESERVATION_OP_RESERVE, 1, Some(subject(1, b"m"))),
        tx(4, rc, RESERVATION_OP_COMMIT, 0, None),
        tx(2, rc, RESERVATION_OP_RESERVE, 1, Some(subject(1, b"n"))),
    ];
    let a = simulate_reservation_ledger(&t1).expect("a");
    let b = simulate_reservation_ledger(&t2).expect("b");
    assert_eq!(
        reservation_ledger_report_body_hash(&a).expect("ha"),
        reservation_ledger_report_body_hash(&b).expect("hb")
    );
}

#[test]
fn reservation_cause_sorting_digest_stable() {
    let r = rid(9);
    let mut t1 = tx(1, r, RESERVATION_OP_RESERVE, 1, Some(subject(1, b"n")));
    let mut t2 = t1.clone();
    t2.cause_refs.reverse();
    let normalized_t1 = normalize_reservation_transition_list(&[t1.clone()]);
    let normalized_t2 = normalize_reservation_transition_list(&[t2]);
    let digest_t1 = reservation_transition_log_digest(&normalized_t1).expect("d1");
    let digest_t2 = reservation_transition_log_digest(&normalized_t2).expect("d2");
    assert_eq!(digest_t1, digest_t2);
    let b1 = reservation_transition_bytes(&t1).expect("b1");
    t1.cause_refs.reverse();
    let normalized_t1 = normalize_reservation_transition(&t1);
    let b2 = reservation_transition_bytes(&normalized_t1).expect("b2");
    assert_eq!(b1, b2);
}

#[test]
fn reservation_subject_key_bytes_are_opaque_and_preserved() {
    let s = subject(0, &[3u8, 1, 2]);
    let n = normalize_reservation_subject_ref(&s);
    assert_eq!(
        n.key_bytes,
        vec![3u8, 1, 2],
        "PROPERTY: opaque subject key bytes must not be sorted or rewritten"
    );
}

#[test]
fn reservation_subject_ab_and_ba_remain_distinct() {
    let ab = rid(21);
    let ba = rid(22);
    let transitions = vec![
        tx(1, ab, RESERVATION_OP_RESERVE, 1, Some(subject(7, b"ab"))),
        tx(2, ba, RESERVATION_OP_RESERVE, 1, Some(subject(7, b"ba"))),
    ];

    let report = simulate_reservation_ledger(&transitions).expect("sim");

    assert_eq!(report.entries_sorted.len(), 2);
    assert_eq!(
        report.entries_sorted[0].subject_ref.key_bytes,
        b"ab".to_vec()
    );
    assert_eq!(
        report.entries_sorted[1].subject_ref.key_bytes,
        b"ba".to_vec()
    );
    assert_ne!(
        report.entries_sorted[0].subject_ref, report.entries_sorted[1].subject_ref,
        "PROPERTY: ab and ba are different opaque subject keys"
    );
}

#[test]
fn reservation_unsupported_transition_schema_version_is_a_finding() {
    let r = rid(23);
    let mut bad = tx(1, r, RESERVATION_OP_RESERVE, 1, Some(subject(1, b"v")));
    bad.schema_version = RESERVATION_TRANSITION_SCHEMA_VERSION + 1;

    let report = simulate_reservation_ledger(&[bad]).expect("sim");

    assert!(report.entries_sorted.is_empty());
    assert!(
        report.findings_sorted.iter().any(|finding| matches!(
            finding,
            ReservationFinding::UnsupportedTransitionSchemaVersion {
                reservation_id,
                observed,
                expected,
            } if *reservation_id == r
                && *observed == RESERVATION_TRANSITION_SCHEMA_VERSION + 1
                && *expected == RESERVATION_TRANSITION_SCHEMA_VERSION
        )),
        "{:?}",
        report.findings_sorted
    );
}

#[test]
fn reservation_invalid_reserve_and_missing_commit() {
    let r = rid(10);
    let bad_reserve = vec![tx(1, r, RESERVATION_OP_RESERVE, 0, Some(subject(1, b"v")))];
    let rep = simulate_reservation_ledger(&bad_reserve).expect("sim");
    assert!(rep.findings_sorted.iter().any(|f| matches!(
        f,
        ReservationFinding::InvalidTransition {
            reason_code: RESERVATION_REASON_RESERVE_INVALID_SUBJECT_OR_UNITS,
            ..
        }
    )));
    let commit_only = vec![tx(1, r, RESERVATION_OP_COMMIT, 0, None)];
    let rep2 = simulate_reservation_ledger(&commit_only).expect("sim2");
    assert!(rep2.findings_sorted.iter().any(|f| matches!(
        f,
        ReservationFinding::InvalidTransition {
            reason_code: RESERVATION_REASON_COMMIT_WITHOUT_RESERVE,
            ..
        }
    )));
}

#[test]
fn reservation_duplicate_reserve() {
    let r = rid(11);
    let t = vec![
        tx(1, r, RESERVATION_OP_RESERVE, 1, Some(subject(1, b"w"))),
        tx(2, r, RESERVATION_OP_RESERVE, 1, Some(subject(1, b"w"))),
    ];
    let rep = simulate_reservation_ledger(&t).expect("sim");
    assert!(rep.findings_sorted.iter().any(|f| matches!(
        f,
        ReservationFinding::InvalidTransition {
            reason_code: RESERVATION_REASON_DUPLICATE_RESERVE,
            ..
        }
    )));
}

#[test]
fn reservation_expire_refund_orphan_invalid_without_reserve() {
    let r = rid(12);
    let t = vec![
        tx(1, r, RESERVATION_OP_EXPIRE, 0, None),
        tx(2, r, RESERVATION_OP_REFUND, 0, None),
        tx(3, r, RESERVATION_OP_ORPHAN, 0, None),
    ];
    let rep = simulate_reservation_ledger(&t).expect("sim");
    assert!(rep.findings_sorted.iter().any(|f| matches!(
        f,
        ReservationFinding::InvalidTransition {
            reason_code: RESERVATION_REASON_EXPIRE_INVALID_STATE,
            ..
        }
    )));
    assert!(rep.findings_sorted.iter().any(|f| matches!(
        f,
        ReservationFinding::InvalidTransition {
            reason_code: RESERVATION_REASON_REFUND_INVALID_STATE,
            ..
        }
    )));
    assert!(rep.findings_sorted.iter().any(|f| matches!(
        f,
        ReservationFinding::InvalidTransition {
            reason_code: RESERVATION_REASON_ORPHAN_INVALID_STATE,
            ..
        }
    )));
}

#[test]
fn reservation_report_findings_order_independent_hash() {
    let r = rid(13);
    let t = vec![
        tx(1, r, RESERVATION_OP_RESERVE, 1, Some(subject(1, b"t"))),
        tx(2, r, RESERVATION_OP_COMMIT, 0, None),
        tx(3, r, RESERVATION_OP_COMMIT, 0, None),
    ];
    let mut rep = simulate_reservation_ledger(&t).expect("sim");
    rep.findings_sorted
        .push(ReservationFinding::InvalidTransition {
            reservation_id: rid(99),
            from_state: RESERVATION_STATE_RESERVED,
            attempted_op: RESERVATION_OP_RESERVE,
            reason_code: RESERVATION_REASON_TRANSITION_ON_TERMINAL,
        });
    let h0 = reservation_ledger_report_body_hash(&rep).expect("h0");
    rep.findings_sorted.reverse();
    let h1 = reservation_ledger_report_body_hash(&rep).expect("h1");
    assert_eq!(h0, h1);
    let ledger_schema_version = RESERVATION_LEDGER_REPORT_SCHEMA_VERSION;
    assert_eq!(rep.schema_version, ledger_schema_version);
    let _st: ReservationState = RESERVATION_STATE_COMMITTED;
    let _d: ReservationDigest = rep.transition_log_digest;
    let _body: ReservationLedgerReportBody = rep.clone();
}
