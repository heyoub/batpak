// justifies: INV-TEST-PANIC-AS-ASSERTION; lane A substrate doctrine tests use panic for PROPERTY mismatches only.
#![allow(clippy::panic)]
//! PROVES: canonical artifact separates body digest from envelope digest; compaction report is structural;
//! append idempotency keys alias event id and replay through the index; public reads use explicit query bounds.
//! CATCHES: accidental identity coupling between body and artifact envelope metadata; silent unbounded public scans.
//! SEEDED: deterministic fixtures only (fixed u128 ids, temp dirs via `tempfile`).

use batpak::artifact::{
    artifact_body_bytes, artifact_envelope_hash_for, artifact_envelope_hash_from_identity,
    artifact_envelope_identity, artifact_verification_report_body_hash,
    verify_canonical_artifact_envelope, ArtifactEnvelopeFinding, ArtifactEnvelopeIdentity,
    ArtifactHash, ArtifactVerificationReport, AttestationRef, CanonicalArtifactEnvelope,
    SignatureEnvelope, SignatureRef, ARTIFACT_ENVELOPE_FRAMING_VERSION,
};
use batpak::encoding;
use batpak::event::EventKind;
use batpak::prelude::*;
use batpak::store::segment::CompactionOutcome;
use batpak::store::{
    compaction_strategy_shape, report_for_run, report_skipped, CompactionReportBody,
    CompactionReportFinding, CompactionStrategyShape, COMPACTION_REPORT_SCHEMA_VERSION,
};
use std::path::PathBuf;
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
fn artifact_body_stable_signature_changes_envelope_only() {
    let body = DemoPayload { v: 7 };
    let base = CanonicalArtifactEnvelope {
        body: body.clone(),
        envelope_schema_version: 1,
        generated_at_wall_ms: None,
        diagnostic_note: None,
        signatures: vec![],
        attestations: vec![],
    };
    let h0 = base.body_hash().expect("body hash");
    let e0 = base.envelope_hash().expect("envelope hash");

    let key: ArtifactHash = [9; 32];
    let with_sig = CanonicalArtifactEnvelope {
        signatures: vec![SignatureEnvelope {
            signature: SignatureRef {
                algorithm_id: 1,
                key_id: key,
                signature_bytes: Vec::from(h0.as_slice()),
            },
        }],
        ..base.clone()
    };
    let h1 = with_sig.body_hash().expect("body hash 2");
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
fn artifact_metadata_ordering_independent_for_body_digest() {
    let a = CanonicalArtifactEnvelope {
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
        batpak::artifact::artifact_body_hash_from_body(&a.body).expect("bh a"),
        batpak::artifact::artifact_body_hash_from_body(&b.body).expect("bh b"),
        "PROPERTY: canonical body digest must exclude envelope-only timestamps"
    );
    assert_ne!(
        a.envelope_hash().expect("eh a"),
        b.envelope_hash().expect("eh b")
    );
}

#[test]
fn artifact_verification_body_digest_ignores_envelope_only_metadata() {
    let body = DemoPayload { v: 22 };
    let low = CanonicalArtifactEnvelope {
        body,
        envelope_schema_version: 1,
        generated_at_wall_ms: Some(1),
        diagnostic_note: Some("a".into()),
        signatures: vec![],
        attestations: vec![],
    };
    let mut hi = low.clone();
    hi.generated_at_wall_ms = Some(999);
    hi.diagnostic_note = Some("zzz".into());
    let r_low = verify_canonical_artifact_envelope(&low, |_s, _b| Ok(())).expect("v1");
    let r_hi = verify_canonical_artifact_envelope(&hi, |_s, _b| Ok(())).expect("v2");
    assert_eq!(
        r_low.body_hash, r_hi.body_hash,
        "PROPERTY: verification report body's digest field must exclude envelope timestamps/notes"
    );
    assert_ne!(
        r_low.envelope_hash, r_hi.envelope_hash,
        "PROPERTY: envelope digest must observe envelope-only metadata"
    );
}

#[test]
fn artifact_attestation_changes_envelope_not_body() {
    let base = CanonicalArtifactEnvelope {
        body: DemoPayload { v: 3 },
        envelope_schema_version: 1,
        generated_at_wall_ms: None,
        diagnostic_note: None,
        signatures: vec![],
        attestations: vec![],
    };
    let with_att = CanonicalArtifactEnvelope {
        attestations: vec![AttestationRef {
            kind_id: 1,
            bytes: vec![1, 2, 3],
        }],
        ..base.clone()
    };
    assert_eq!(
        base.body_hash().expect("b0"),
        with_att.body_hash().expect("b1")
    );
    assert_ne!(
        base.envelope_hash().expect("e0"),
        with_att.envelope_hash().expect("e1")
    );
}

#[test]
fn artifact_invalid_signature_finding_deterministic() {
    let body = DemoPayload { v: 4 };
    let raw = encoding::to_bytes(&body).expect("encode");
    let key = [5_u8; 32];
    let env = CanonicalArtifactEnvelope {
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

    let report =
        verify_canonical_artifact_envelope(&env, invalid_signature_test_verifier).expect("verify");

    match report.findings.as_slice() {
        [ArtifactEnvelopeFinding::InvalidSignature { key_id, reason }] => {
            assert_eq!(*key_id, key);
            assert!(!reason.is_empty());
        }
        _ => panic!(
            "PROPERTY: expected exactly one InvalidSignature finding, got {:?}",
            report.findings
        ),
    }

    let hrep = artifact_verification_report_body_hash(&report).expect("report digest");
    let _typed: &ArtifactVerificationReport = &report;
    let hrep2 = artifact_verification_report_body_hash(&report).expect("report digest 2");
    assert_eq!(hrep, hrep2);

    let ok_sig = CanonicalArtifactEnvelope {
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
    let ok = verify_canonical_artifact_envelope(&ok_sig, invalid_signature_test_verifier)
        .expect("verify ok");
    assert!(ok.findings.is_empty());
}

#[test]
fn artifact_free_functions_match_inherent_hashes() {
    let env = CanonicalArtifactEnvelope {
        body: DemoPayload { v: 11 },
        envelope_schema_version: 2,
        generated_at_wall_ms: None,
        diagnostic_note: None,
        signatures: vec![],
        attestations: vec![],
    };
    let via_fn = artifact_envelope_hash_for(&env).expect("free fn");
    let via_method = env.envelope_hash().expect("method");
    assert_eq!(via_fn, via_method);
    let bb = artifact_body_bytes(&env.body).expect("bytes");
    assert!(!bb.is_empty());
    let _: ArtifactHash = via_fn;
}

#[test]
fn artifact_identity_roundtrip_typing() {
    let env = CanonicalArtifactEnvelope {
        body: DemoPayload { v: 8 },
        envelope_schema_version: 3,
        generated_at_wall_ms: Some(99),
        diagnostic_note: None,
        signatures: vec![],
        attestations: vec![],
    };
    let bh = batpak::artifact::artifact_body_hash_from_body(&env.body).expect("bh");
    let id: ArtifactEnvelopeIdentity = artifact_envelope_identity(&env, bh);
    assert_eq!(id.framing_schema_version, ARTIFACT_ENVELOPE_FRAMING_VERSION);
    let _ = artifact_envelope_hash_from_identity(&id).expect("eh");
}

#[test]
fn compaction_report_helpers_cover_engine_paths() {
    let cfg = CompactionConfig::default();
    let sealed: Vec<(u64, std::path::PathBuf)> = vec![
        (1, std::path::PathBuf::from("000001.fbat")),
        (2, std::path::PathBuf::from("000002.fbat")),
    ];
    let skipped = report_skipped(&cfg, 9, &sealed).expect("skipped");
    let _: CompactionReportBody = skipped.clone();
    let _: CompactionStrategyShape = skipped.strategy_shape;
    assert_eq!(
        skipped.strategy_shape,
        compaction_strategy_shape(&cfg.strategy)
    );
    assert_eq!(skipped.source_segment_ids_sorted, vec![1, 2]);
    assert_eq!(skipped.input_segment_id_low, Some(1));
    assert_eq!(skipped.input_segment_id_high, Some(2));
    let result = batpak::store::segment::CompactionResult {
        outcome: CompactionOutcome::Skipped,
        segments_removed: 0,
        bytes_reclaimed: 0,
    };
    let _ = report_for_run(&cfg, 9, &sealed, None, &result, None).expect("run");
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
    assert_eq!(rep0.compaction_id, rep1.compaction_id);
    assert_eq!(rep0, rep1);
    let h0 = rep0.body_hash().expect("h0");
    let h1 = rep1.body_hash().expect("h1");
    assert_eq!(h0, h1);
    assert_eq!(rep0.schema_version, COMPACTION_REPORT_SCHEMA_VERSION);
    store.close().expect("close");
}

#[test]
fn compaction_id_stable_when_only_findings_change() {
    let cfg = CompactionConfig::default();
    let sealed: Vec<(u64, std::path::PathBuf)> = vec![
        (10, std::path::PathBuf::from("seg10.fbat")),
        (20, std::path::PathBuf::from("seg20.fbat")),
    ];
    let mut base = report_skipped(&cfg, 99, &sealed).expect("base");
    let cid0 = base.compaction_id;
    base.findings
        .push(CompactionReportFinding::OutputSegmentHashUnavailable {
            reason: "inject".into(),
        });
    let cid1 = base.compaction_id;
    assert_eq!(
        cid0, cid1,
        "PROPERTY: compaction_id must fingerprint structural inputs only"
    );
    let mut noisy = base.clone();
    noisy
        .findings
        .push(CompactionReportFinding::OutputSegmentHashUnavailable { reason: "b".into() });
    assert_eq!(
        noisy.compaction_id, cid0,
        "PROPERTY: compaction_id excludes ordered findings vectors"
    );
}

#[test]
fn compaction_failure_emits_preswap_rollback_finding() {
    let cfg = CompactionConfig::default();
    let sealed: Vec<(u64, std::path::PathBuf)> = vec![(7, PathBuf::from("x"))];
    let result = batpak::store::segment::CompactionResult {
        outcome: CompactionOutcome::Failed {
            reason: "pre-swap rollback".into(),
        },
        segments_removed: 0,
        bytes_reclaimed: 0,
    };
    let rep = report_for_run(&cfg, 3, &sealed, Some(99), &result, None).expect("rep");
    assert!(
        matches!(
            rep.findings.as_slice(),
            [CompactionReportFinding::PreSwapRollback { reason }] if reason.contains("pre-swap rollback")
        ),
        "expected PreSwapRollback finding, got {:?}",
        rep.findings
    );
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
        .append_with_options(&coord_a, kind, &serde_json::json!({ "who": "first" }), opts)
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
    let cfg = CompactionConfig::default();
    let sealed: Vec<(u64, std::path::PathBuf)> = vec![
        (1, std::path::PathBuf::from("a")),
        (2, std::path::PathBuf::from("b")),
    ];
    let mut a = report_skipped(&cfg, 5, &sealed).expect("rep");
    a.findings.extend([
        CompactionReportFinding::OutputSegmentHashUnavailable { reason: "b".into() },
        CompactionReportFinding::OutputSegmentHashUnavailable { reason: "a".into() },
    ]);
    let mut b = a.clone();
    b.findings.reverse();
    assert_eq!(
        a.body_hash().expect("ha"),
        b.body_hash().expect("hb"),
        "PROPERTY: report body hashing must canonicalize finding order"
    );
}
