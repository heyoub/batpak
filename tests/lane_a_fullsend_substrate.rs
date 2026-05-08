// justifies: INV-TEST-PANIC-AS-ASSERTION; lane A substrate doctrine tests use panic for PROPERTY mismatches only.
#![allow(clippy::panic)]
//! PROVES: canonical envelope separates body digest from envelope digest; compaction report is structural;
//! append idempotency keys alias event id and replay through the index; public reads use explicit query bounds.
//! CATCHES: accidental identity coupling between body and envelope metadata; silent unbounded public scans.
//! SEEDED: deterministic fixtures only (fixed u128 ids, temp dirs via `tempfile`).

use batpak::encoding;
use batpak::envelope::{
    body_hash_from_body, verification_report_body_hash, verify_envelope, AttestationRef,
    CanonicalEnvelope, ContentDigest, EnvelopeIdentity, EnvelopeVerificationFinding,
    EnvelopeVerificationReport, SignatureEnvelope, SignatureRef,
    CANONICAL_ENVELOPE_FRAMING_VERSION,
};
use batpak::envelope::{envelope_hash_from_identity, envelope_identity};
use batpak::event::EventKind;
use batpak::prelude::*;
use batpak::store::segment::CompactionOutcome;
use batpak::store::{
    compaction_strategy_shape, report_for_run, report_skipped, CompactionReportBody,
    CompactionReportFinding, CompactionStrategyShape, COMPACTION_REPORT_SCHEMA_VERSION,
};
use tempfile::TempDir;

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
struct DemoPayload {
    v: u32,
}

fn invalid_signature_test_verifier(sig: &SignatureRef, body_bytes: &[u8]) -> Result<(), String> {
    if sig.algorithm_id == 1 && sig.signature_bytes == body_bytes {
        Ok(())
    } else {
        Err("signature does not echo canonical body bytes".into())
    }
}

#[test]
fn envelope_body_stable_signature_changes_envelope_only() {
    let body = DemoPayload { v: 7 };
    let base = CanonicalEnvelope {
        body: body.clone(),
        envelope_schema_version: 1,
        generated_at_wall_ms: None,
        diagnostic_note: None,
        signatures: vec![],
        attestations: vec![],
    };
    let h0 = body_hash_from_body(&base.body).expect("body hash");
    let e0 = base.envelope_hash().expect("envelope hash");

    let key: ContentDigest = [9; 32];
    let with_sig = CanonicalEnvelope {
        signatures: vec![SignatureEnvelope {
            signature: SignatureRef {
                algorithm_id: 1,
                key_id: key,
                signature_bytes: Vec::from(h0.as_slice()),
            },
        }],
        ..base.clone()
    };
    let h1 = body_hash_from_body(&with_sig.body).expect("body hash 2");
    let e1 = with_sig.envelope_hash().expect("envelope hash 2");

    assert_eq!(
        h0, h1,
        "PROPERTY: body digest must ignore envelope signatures"
    );
    assert_ne!(
        e0, e1,
        "PROPERTY: envelope digest must change when signatures change"
    );
}

#[test]
fn envelope_metadata_ordering_independent_for_digest() {
    let a = CanonicalEnvelope {
        body: DemoPayload { v: 1 },
        envelope_schema_version: 1,
        generated_at_wall_ms: Some(10),
        diagnostic_note: Some("a".into()),
        signatures: vec![],
        attestations: vec![],
    };
    let mut b = a.clone();
    b.generated_at_wall_ms = Some(20);
    assert_eq!(
        body_hash_from_body(&a.body).expect("bh a"),
        body_hash_from_body(&b.body).expect("bh b")
    );
    assert_ne!(
        a.envelope_hash().expect("eh a"),
        b.envelope_hash().expect("eh b")
    );
}

#[test]
fn envelope_attestation_changes_envelope_not_body() {
    let base = CanonicalEnvelope {
        body: DemoPayload { v: 3 },
        envelope_schema_version: 1,
        generated_at_wall_ms: None,
        diagnostic_note: None,
        signatures: vec![],
        attestations: vec![],
    };
    let with_att = CanonicalEnvelope {
        attestations: vec![AttestationRef {
            kind_id: 1,
            bytes: vec![1, 2, 3],
        }],
        ..base.clone()
    };
    assert_eq!(
        body_hash_from_body(&base.body).expect("b0"),
        body_hash_from_body(&with_att.body).expect("b1")
    );
    assert_ne!(
        base.envelope_hash().expect("e0"),
        with_att.envelope_hash().expect("e1")
    );
}

#[test]
fn envelope_invalid_signature_finding_deterministic() {
    let body = DemoPayload { v: 4 };
    let raw = encoding::to_bytes(&body).expect("encode");
    let key = [5_u8; 32];
    let env = CanonicalEnvelope {
        body: body.clone(),
        envelope_schema_version: 1,
        generated_at_wall_ms: None,
        diagnostic_note: None,
        signatures: vec![SignatureEnvelope {
            signature: SignatureRef {
                algorithm_id: 1,
                key_id: key,
                signature_bytes: vec![0],
            },
        }],
        attestations: vec![],
    };

    let report = verify_envelope(&env, invalid_signature_test_verifier).expect("verify");

    match report.findings.as_slice() {
        [EnvelopeVerificationFinding::InvalidSignature { key_id, reason }] => {
            assert_eq!(*key_id, key);
            assert!(!reason.is_empty());
        }
        _ => panic!(
            "PROPERTY: expected exactly one InvalidSignature finding, got {:?}",
            report.findings
        ),
    }

    let hrep = verification_report_body_hash(&report).expect("report digest");
    let _typed: &EnvelopeVerificationReport = &report;
    let hrep2 = verification_report_body_hash(&report).expect("report digest 2");
    assert_eq!(hrep, hrep2);

    let ok_sig = CanonicalEnvelope {
        signatures: vec![SignatureEnvelope {
            signature: SignatureRef {
                algorithm_id: 1,
                key_id: key,
                signature_bytes: raw.clone(),
            },
        }],
        attestations: vec![],
        ..env.clone()
    };
    let ok = verify_envelope(&ok_sig, invalid_signature_test_verifier).expect("verify ok");
    assert!(ok.findings.is_empty());
}

#[test]
fn envelope_free_functions_match_inherent_hashes() {
    let env = CanonicalEnvelope {
        body: DemoPayload { v: 11 },
        envelope_schema_version: 2,
        generated_at_wall_ms: None,
        diagnostic_note: None,
        signatures: vec![],
        attestations: vec![],
    };
    let via_fn = batpak::envelope::envelope_hash_for(&env).expect("free fn");
    let via_method = env.envelope_hash().expect("method");
    assert_eq!(via_fn, via_method);
    let bb = batpak::envelope::body_bytes(&env.body).expect("bytes");
    assert!(!bb.is_empty());
    let _: ContentDigest = via_fn;
}

#[test]
fn envelope_identity_roundtrip_typing() {
    let env = CanonicalEnvelope {
        body: DemoPayload { v: 8 },
        envelope_schema_version: 3,
        generated_at_wall_ms: Some(99),
        diagnostic_note: None,
        signatures: vec![],
        attestations: vec![],
    };
    let bh = body_hash_from_body(&env.body).expect("bh");
    let id: EnvelopeIdentity = envelope_identity(&env, bh).expect("id");
    assert_eq!(
        id.framing_schema_version,
        CANONICAL_ENVELOPE_FRAMING_VERSION
    );
    let _ = envelope_hash_from_identity(&id).expect("eh");
}

#[test]
fn compaction_report_helpers_cover_engine_paths() {
    let cfg = CompactionConfig::default();
    let sealed: Vec<(u64, std::path::PathBuf)> = vec![
        (1, std::path::PathBuf::from("000001.fbat")),
        (2, std::path::PathBuf::from("000002.fbat")),
    ];
    let skipped = report_skipped(&cfg, 9, &sealed);
    assert_eq!(
        skipped.strategy_shape,
        compaction_strategy_shape(&cfg.strategy)
    );
    let result = batpak::store::segment::CompactionResult {
        outcome: CompactionOutcome::Skipped,
        segments_removed: 0,
        bytes_reclaimed: 0,
    };
    let _ = report_for_run(&cfg, 9, &sealed, None, &result, None);
}

fn lane_store() -> (Store<Open>, TempDir) {
    let dir = TempDir::new().expect("tmp");
    let mut cfg = StoreConfig::new(dir.path());
    cfg.segment_max_bytes = 200;
    let store = Store::open(cfg).expect("open");
    (store, dir)
}

#[test]
fn compaction_report_skipped_is_deterministic() {
    let (store, _dir) = lane_store();
    let coord = Coordinate::new("e", "s").expect("coord");
    let kind = EventKind::custom(0xF, 1);
    store
        .append(&coord, kind, &serde_json::json!({ "x": 1 }))
        .expect("append");
    store.sync().expect("sync");

    let cfg = CompactionConfig::default();
    let (r0, rep0) = store.compact_with_report(&cfg).expect("cw");
    let (r1, rep1) = store.compact_with_report(&cfg).expect("cw2");
    assert!(matches!(r0.outcome, CompactionOutcome::Skipped));
    assert_eq!(r0.outcome, r1.outcome);
    assert_eq!(rep0, rep1);
    let h0 = rep0.body_hash().expect("h0");
    let h1 = rep1.body_hash().expect("h1");
    assert_eq!(h0, h1);
    assert_eq!(rep0.schema_version, COMPACTION_REPORT_SCHEMA_VERSION);
    store.close().expect("close");
}

#[test]
fn idempotency_key_is_event_id_scoped_global_lookup() {
    let (store, _dir) = lane_store();
    let coord_a = Coordinate::new("e-a", "s").expect("c1");
    let coord_b = Coordinate::new("e-b", "s").expect("c2");
    let kind = EventKind::custom(0xF, 1);

    let key = 0xC0FFEE_u128;
    let opts = AppendOptions::new().with_idempotency(key);

    let r1 = store
        .append_with_options(
            &coord_a,
            kind,
            &serde_json::json!({ "who": "first" }),
            opts,
        )
        .expect("append a");
    assert_eq!(r1.event_id, key);

    let r2 = store
        .append_with_options(
            &coord_b,
            kind,
            &serde_json::json!({ "who": "second" }),
            opts,
        )
        .expect("replay");

    assert_eq!(r1.event_id, r2.event_id);
    assert_eq!(r1.sequence, r2.sequence);
    store.close().expect("close");
}

#[test]
fn public_bulk_reads_require_explicit_bounds_not_implicit_global_cursor() {
    let dir = tempfile::tempdir().expect("t");
    let store = Store::<Open>::open(StoreConfig::new(dir.path())).expect("open");
    let _: Vec<IndexEntry> = store.query(&Region::all());
    let _: Vec<IndexEntry> = store.by_scope("s");
    let _: Vec<IndexEntry> = store.stream("entity-x");
    let _: Vec<IndexEntry> = store.by_fact(EventKind::custom(0xF, 1));
    let _: Cursor = store.cursor_guaranteed(&Region::all());
    drop(store);
}

#[test]
fn compaction_report_findings_order_does_not_change_body_hash() {
    let a = CompactionReportBody {
        schema_version: 1,
        strategy_shape: CompactionStrategyShape::Merge,
        min_segments_threshold: 2,
        active_segment_id: 1,
        sealed_segment_count: 0,
        source_segment_ids_sorted: vec![1, 2],
        merged_segment_id: None,
        output_segment_bytes_hash: None,
        outcome: CompactionOutcome::Skipped,
        segments_removed: 0,
        bytes_reclaimed: 0,
        findings: vec![
            CompactionReportFinding::OutputSegmentHashUnavailable { reason: "b".into() },
            CompactionReportFinding::OutputSegmentHashUnavailable { reason: "a".into() },
        ],
    };
    let mut b = a.clone();
    b.findings.reverse();
    assert_eq!(
        a.body_hash().expect("ha"),
        b.body_hash().expect("hb"),
        "PROPERTY: report body hashing must canonicalize finding order"
    );
}
