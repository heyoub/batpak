//! PROVES: `batpak::registry` row bodies hash with sorted `named_digests`; drift and verification report
//! `body_hash` are stable; attested rows compose `CanonicalArtifactEnvelope` with normalized signing bytes.
//! CATCHES: permutation-sensitive row hashing; drift/verification finding order sensitivity; broken supersession edges.
//! SEEDED: fixed `RegistryRowId` / digest fixtures only.

use batpak::artifact::{
    artifact_body_hash_from_body, ArtifactEnvelopeFinding, ArtifactHash,
    ArtifactVerificationReport, CanonicalArtifactEnvelope, SignatureEnvelope, SignatureRef,
};
use batpak::registry::{
    normalize_registry_row_body, registry_drift_findings_sorted, registry_drift_report_body_hash,
    registry_row_body_bytes, registry_row_body_hash, registry_row_body_hash_matches_signing_bytes,
    registry_row_signing_bytes, registry_supersession_findings_sorted,
    registry_verification_report_body_hash, sort_registry_row_hash_pairs,
    verify_registry_attested_row, verify_registry_row_signatures_only, NamedDigest,
    RegistryDriftFinding, RegistryDriftReportBody, RegistryRowBody, RegistryRowId,
    RegistrySupersessionFinding, RegistryVerificationFinding, RegistryVerificationReport,
    REGISTRY_DRIFT_REPORT_SCHEMA_VERSION, REGISTRY_LIFECYCLE_ANNOUNCED,
    REGISTRY_LIFECYCLE_DEPRECATED, REGISTRY_LIFECYCLE_LIVE, REGISTRY_LIFECYCLE_REMOVED,
    REGISTRY_ROW_BODY_SCHEMA_VERSION, REGISTRY_VERIFICATION_REPORT_SCHEMA_VERSION,
};

fn echo_sig_ok(sig: &SignatureRef, body_bytes: &[u8]) -> Result<(), String> {
    if sig.algorithm_id == 1 && sig.signature_bytes == body_bytes {
        Ok(())
    } else {
        Err("echo signature mismatch".into())
    }
}

fn rid(tag: u8) -> RegistryRowId {
    RegistryRowId([tag; 32])
}

fn sample_body(row_id: RegistryRowId, lifecycle: u32) -> RegistryRowBody {
    RegistryRowBody {
        schema_version: REGISTRY_ROW_BODY_SCHEMA_VERSION,
        row_id,
        row_kind: 42,
        row_layout_version: 1,
        opaque_payload: vec![1, 2, 3],
        named_digests: vec![
            NamedDigest {
                name: "b".into(),
                digest: [2u8; 32],
            },
            NamedDigest {
                name: "a".into(),
                digest: [1u8; 32],
            },
        ],
        lifecycle,
        supersedes: None,
    }
}

#[test]
fn registry_named_digest_order_is_immaterial_to_row_hash() {
    let id = rid(7);
    let a = sample_body(id, REGISTRY_LIFECYCLE_LIVE);
    let mut b = a.clone();
    b.named_digests.reverse();
    let ha = registry_row_body_hash(&a).expect("hash a");
    let hb = registry_row_body_hash(&b).expect("hash b");
    assert_eq!(
        ha, hb,
        "PROPERTY: row_hash must sort named_digests before digest"
    );
    let na = normalize_registry_row_body(&a);
    let nb = normalize_registry_row_body(&b);
    assert_eq!(na.named_digests, nb.named_digests);
}

#[test]
fn registry_row_signing_bytes_match_artifact_body_plane_on_normalized() {
    let id = rid(9);
    let body = sample_body(id, REGISTRY_LIFECYCLE_ANNOUNCED);
    assert!(
        registry_row_body_hash_matches_signing_bytes(&body).expect("cmp"),
        "PROPERTY: normalized row body must share one MessagePack plane with artifact body bytes"
    );
    let n = normalize_registry_row_body(&body);
    let h1 = registry_row_body_hash(&body).expect("rh");
    let h2 = artifact_body_hash_from_body(&n).expect("ah");
    assert_eq!(h1, h2);
    let raw = registry_row_body_bytes(&body).expect("bytes");
    let sign = registry_row_signing_bytes(&body).expect("sign bytes");
    assert_eq!(raw, sign);
}

#[test]
fn registry_drift_report_body_hash_sorts_findings() {
    let id = rid(3);
    let h: ArtifactHash = [5u8; 32];
    let mut expected = vec![(id, h)];
    let mut observed = vec![(id, h)];
    sort_registry_row_hash_pairs(&mut expected);
    sort_registry_row_hash_pairs(&mut observed);
    let f1 = registry_drift_findings_sorted(&expected, &observed);
    let mut report = RegistryDriftReportBody {
        schema_version: REGISTRY_DRIFT_REPORT_SCHEMA_VERSION,
        expected: expected.clone(),
        observed: observed.clone(),
        findings: vec![],
    };
    let g0 = registry_drift_report_body_hash(&report).expect("drift hash empty");
    report.findings = vec![
        RegistryDriftFinding::MissingRow { row_id: rid(1) },
        RegistryDriftFinding::ExtraRow { row_id: rid(2) },
    ];
    let g1 = registry_drift_report_body_hash(&report).expect("drift hash findings");
    report.findings.reverse();
    let g2 = registry_drift_report_body_hash(&report).expect("drift hash findings permuted");
    assert_eq!(
        g1, g2,
        "PROPERTY: drift report body_hash must sort findings"
    );
    assert_ne!(g0, g1);
    assert_eq!(f1.len(), 0);
}

#[test]
fn registry_drift_report_body_hash_sorts_expected_and_observed_pairs() {
    let a = rid(10);
    let b = rid(11);
    let expected_sorted = vec![(a, [1u8; 32]), (b, [2u8; 32])];
    let observed_sorted = vec![(a, [3u8; 32]), (b, [4u8; 32])];
    let report = RegistryDriftReportBody {
        schema_version: REGISTRY_DRIFT_REPORT_SCHEMA_VERSION,
        expected: expected_sorted.clone(),
        observed: observed_sorted.clone(),
        findings: vec![],
    };
    let report_permuted = RegistryDriftReportBody {
        expected: expected_sorted.into_iter().rev().collect(),
        observed: observed_sorted.into_iter().rev().collect(),
        ..report.clone()
    };

    assert_eq!(
        registry_drift_report_body_hash(&report).expect("h sorted"),
        registry_drift_report_body_hash(&report_permuted).expect("h permuted"),
        "PROPERTY: drift report body_hash must normalize expected and observed row/hash pairs"
    );
}

#[test]
fn registry_drift_detects_hash_mismatch() {
    let id = rid(4);
    let mut expected = vec![(id, [1u8; 32])];
    let observed = vec![(id, [2u8; 32])];
    sort_registry_row_hash_pairs(&mut expected);
    let mut obs = observed;
    sort_registry_row_hash_pairs(&mut obs);
    let findings = registry_drift_findings_sorted(&expected, &obs);
    assert!(
        matches!(
            findings.as_slice(),
            [RegistryDriftFinding::HashMismatch { .. }]
        ),
        "expected single HashMismatch, got {findings:?}"
    );
}

#[test]
fn registry_verify_attested_row_happy_path() {
    let row_id = rid(8);
    let body = sample_body(row_id, REGISTRY_LIFECYCLE_LIVE);
    let row_hash = registry_row_body_hash(&body).expect("row hash");
    let body_raw = registry_row_body_bytes(&body).expect("row bytes");
    let envelope = CanonicalArtifactEnvelope {
        body: body.clone(),
        envelope_schema_version: 1,
        generated_at_wall_ms: None,
        diagnostic_note: None,
        signatures: vec![SignatureEnvelope {
            signature: SignatureRef {
                algorithm_id: 1,
                key_id: [8u8; 32],
                signature_bytes: body_raw.clone(),
            },
        }],
        attestations: vec![],
    };
    let report =
        verify_registry_attested_row(&envelope, row_id, row_hash, echo_sig_ok).expect("verify");
    assert_eq!(
        report.schema_version,
        REGISTRY_VERIFICATION_REPORT_SCHEMA_VERSION
    );
    assert!(report.findings.is_empty());
    let plane = &report.envelope_plane;
    let vh = registry_verification_report_body_hash(&report).expect("vr hash");
    let mut findings_dup = report.findings.clone();
    findings_dup.reverse();
    let report2 = RegistryVerificationReport {
        findings: findings_dup,
        ..report.clone()
    };
    assert_eq!(
        vh,
        registry_verification_report_body_hash(&report2).expect("vr hash 2"),
        "PROPERTY: verification report body_hash sorts findings"
    );
    let only_sig = verify_registry_row_signatures_only(&envelope, echo_sig_ok).expect("sig only");
    assert_eq!(only_sig.body_hash, plane.body_hash);
    assert_eq!(only_sig.envelope_hash, plane.envelope_hash);
}

#[test]
fn registry_verify_flags_bad_lifecycle_and_bad_row_hash() {
    let row_id = rid(6);
    let body = sample_body(row_id, 99);
    let _row_hash = registry_row_body_hash(&body).expect("row hash");
    let raw = registry_row_body_bytes(&body).expect("bytes");
    let envelope = CanonicalArtifactEnvelope {
        body,
        envelope_schema_version: 1,
        generated_at_wall_ms: None,
        diagnostic_note: None,
        signatures: vec![SignatureEnvelope {
            signature: SignatureRef {
                algorithm_id: 1,
                key_id: [6u8; 32],
                signature_bytes: raw,
            },
        }],
        attestations: vec![],
    };
    let bad_claim_hash = [0u8; 32];
    let r = verify_registry_attested_row(&envelope, row_id, bad_claim_hash, echo_sig_ok)
        .expect("verify");
    assert!(
        r.findings
            .iter()
            .any(|f| matches!(f, RegistryVerificationFinding::InvalidLifecycle { .. })),
        "expected InvalidLifecycle"
    );
    assert!(
        r.findings
            .iter()
            .any(|f| matches!(f, RegistryVerificationFinding::RowHashMismatch { .. })),
        "expected RowHashMismatch"
    );
}

#[test]
fn registry_verify_flags_unsupported_row_schema_version() {
    let row_id = rid(12);
    let mut body = sample_body(row_id, REGISTRY_LIFECYCLE_LIVE);
    body.schema_version = REGISTRY_ROW_BODY_SCHEMA_VERSION + 1;
    let row_hash = registry_row_body_hash(&body).expect("row hash");
    let raw = registry_row_body_bytes(&body).expect("bytes");
    let envelope = CanonicalArtifactEnvelope {
        body,
        envelope_schema_version: 1,
        generated_at_wall_ms: None,
        diagnostic_note: None,
        signatures: vec![SignatureEnvelope {
            signature: SignatureRef {
                algorithm_id: 1,
                key_id: [12u8; 32],
                signature_bytes: raw,
            },
        }],
        attestations: vec![],
    };

    let report =
        verify_registry_attested_row(&envelope, row_id, row_hash, echo_sig_ok).expect("verify");

    assert!(report.findings.iter().any(|finding| matches!(
        finding,
        RegistryVerificationFinding::UnsupportedRowSchemaVersion {
            row_id: finding_row_id,
            observed,
            expected,
        } if *finding_row_id == row_id
            && *observed == REGISTRY_ROW_BODY_SCHEMA_VERSION + 1
            && *expected == REGISTRY_ROW_BODY_SCHEMA_VERSION
    )));
}

#[test]
fn registry_verification_report_body_hash_sorts_nested_envelope_findings() {
    let envelope_plane = ArtifactVerificationReport {
        body_hash: [1u8; 32],
        envelope_hash: [2u8; 32],
        findings: vec![
            ArtifactEnvelopeFinding::InvalidSignature {
                key_id: [9u8; 32],
                reason: "z".into(),
            },
            ArtifactEnvelopeFinding::InvalidSignature {
                key_id: [8u8; 32],
                reason: "a".into(),
            },
        ],
    };
    let report = RegistryVerificationReport {
        schema_version: REGISTRY_VERIFICATION_REPORT_SCHEMA_VERSION,
        envelope_plane,
        findings: vec![],
    };
    let mut permuted = report.clone();
    permuted.envelope_plane.findings.reverse();

    assert_eq!(
        registry_verification_report_body_hash(&report).expect("h report"),
        registry_verification_report_body_hash(&permuted).expect("h permuted"),
        "PROPERTY: registry verification body_hash must normalize nested envelope findings"
    );
}

#[test]
fn registry_verify_row_id_mismatch() {
    let row_id = rid(1);
    let other = rid(2);
    let body = sample_body(row_id, REGISTRY_LIFECYCLE_DEPRECATED);
    let row_hash = registry_row_body_hash(&body).expect("row hash");
    let raw = registry_row_body_bytes(&body).expect("bytes");
    let envelope = CanonicalArtifactEnvelope {
        body,
        envelope_schema_version: 1,
        generated_at_wall_ms: None,
        diagnostic_note: None,
        signatures: vec![SignatureEnvelope {
            signature: SignatureRef {
                algorithm_id: 1,
                key_id: [1u8; 32],
                signature_bytes: raw,
            },
        }],
        attestations: vec![],
    };
    let r = verify_registry_attested_row(&envelope, other, row_hash, echo_sig_ok).expect("verify");
    assert!(
        r.findings
            .iter()
            .any(|f| matches!(f, RegistryVerificationFinding::RowIdMismatch { .. })),
        "expected RowIdMismatch"
    );
}

#[test]
fn registry_supersession_dangling_removed_and_cycle() {
    let a = rid(1);
    let b = rid(2);
    let c = rid(3);
    let d = rid(4);
    let missing = rid(9);

    let mut body_a = sample_body(a, REGISTRY_LIFECYCLE_LIVE);
    body_a.supersedes = Some(b);
    let mut body_b = sample_body(b, REGISTRY_LIFECYCLE_LIVE);
    body_b.supersedes = Some(a);
    let mut body_c = sample_body(c, REGISTRY_LIFECYCLE_REMOVED);
    body_c.supersedes = Some(a);
    let mut body_d = sample_body(d, REGISTRY_LIFECYCLE_LIVE);
    body_d.supersedes = Some(missing);

    let catalog = vec![(a, body_a), (b, body_b), (c, body_c), (d, body_d)];
    let mut f = registry_supersession_findings_sorted(&catalog);
    f.sort();
    assert!(f.iter().any(|x| matches!(
        x,
        RegistrySupersessionFinding::DanglingSupersedes { target, .. } if *target == missing
    )));
    assert!(f.iter().any(|x| matches!(
        x,
        RegistrySupersessionFinding::RemovedDeclaresSupersedes { from } if *from == c
    )));
    assert!(f
        .iter()
        .any(|x| matches!(x, RegistrySupersessionFinding::SupersedesCycle { .. })));
    let dup_catalog = vec![
        (a, sample_body(a, REGISTRY_LIFECYCLE_LIVE)),
        (a, sample_body(a, REGISTRY_LIFECYCLE_LIVE)),
    ];
    let dup = registry_supersession_findings_sorted(&dup_catalog);
    assert!(dup
        .iter()
        .any(|x| matches!(x, RegistrySupersessionFinding::DuplicateRowId { .. })));
}
