//! PROVES: backup manifest `manifest_hash` ignores segment permutation; restore proof reports are
//! deterministic; envelope-only metadata does not change manifest body identity.
//! CATCHES: unsorted segment normalization gaps; missing/extra segment at restore; digest mismatches.
//! SEEDED: synthetic segment ids and fixed digests only.

use batpak::artifact::{artifact_body_hash_from_body, SignatureEnvelope, SignatureRef};
use batpak::encoding;
use batpak::store::backup_envelope::{
    audit_backup_manifest_segments, backup_manifest_body_bytes, backup_manifest_body_hash,
    backup_manifest_envelope_body_hash, backup_manifest_envelope_hash,
    normalize_backup_manifest_body, normalize_backup_manifest_envelope,
    restore_proof_evidence_report, restore_proof_report_body, restore_proof_report_body_hash,
    sort_backup_segment_refs, verify_backup_manifest_envelope,
    verify_backup_manifest_signatures_only, BackupEnvelopeFinding, BackupManifestBody,
    BackupManifestEnvelope, BackupManifestVerification, BackupSegmentRef,
    RestoreProofEvidenceReport, RestoreProofHash, RestoreProofReportBody, SegmentBytesDigest,
    BACKUP_MANIFEST_BODY_SCHEMA_VERSION, RESTORE_PROOF_REPORT_SCHEMA_VERSION,
};
use std::io::Write;

fn dig(b: u8) -> SegmentBytesDigest {
    [b; 32]
}

fn sample_manifest() -> BackupManifestBody {
    BackupManifestBody {
        schema_version: BACKUP_MANIFEST_BODY_SCHEMA_VERSION,
        backup_id: dig(1),
        layout_revision: 2,
        tooling_revision: 3,
        segments: vec![
            BackupSegmentRef {
                segment_id: 2,
                bytes_digest: dig(2),
            },
            BackupSegmentRef {
                segment_id: 1,
                bytes_digest: dig(1),
            },
        ],
    }
}

fn echo_sig_ok(sig: &SignatureRef, body_bytes: &[u8]) -> Result<(), String> {
    if sig.algorithm_id == 1 && sig.signature_bytes == body_bytes {
        Ok(())
    } else {
        Err("echo signature mismatch".into())
    }
}

#[test]
fn backup_manifest_hash_order_independent() {
    let a = sample_manifest();
    let mut b = a.clone();
    b.segments.reverse();
    let ha = backup_manifest_body_hash(&a).expect("h a");
    let hb = backup_manifest_body_hash(&b).expect("h b");
    assert_eq!(
        ha, hb,
        "PROPERTY: segment order must not change manifest hash"
    );
}

#[test]
fn backup_manifest_hash_changes_when_segment_digest_changes() {
    let mut a = sample_manifest();
    let h0 = backup_manifest_body_hash(&a).expect("h0");
    a.segments[0].bytes_digest = dig(9);
    let h1 = backup_manifest_body_hash(&a).expect("h1");
    assert_ne!(h0, h1);
}

#[test]
fn backup_manifest_signing_plane_matches_artifact_body_hash() {
    let m = sample_manifest();
    let n = normalize_backup_manifest_body(&m);
    let h1 = backup_manifest_body_hash(&m).expect("mh");
    let h2 = artifact_body_hash_from_body(&n).expect("ah");
    assert_eq!(h1, h2);
}

#[test]
fn backup_sort_backup_segment_refs_and_restore_proof_stable() {
    let manifest = sample_manifest();
    let observed = vec![
        BackupSegmentRef {
            segment_id: 2,
            bytes_digest: dig(2),
        },
        BackupSegmentRef {
            segment_id: 1,
            bytes_digest: dig(1),
        },
    ];
    let r1 = restore_proof_report_body(&manifest, &observed).expect("r1");
    let mut obs2 = observed.clone();
    obs2.reverse();
    let r2 = restore_proof_report_body(&manifest, &obs2).expect("r2");
    assert_eq!(r1.findings, r2.findings);
    assert_eq!(r1.manifest_body_hash, r2.manifest_body_hash);
    let g1 = restore_proof_report_body_hash(&r1).expect("rh1");
    let envelope = RestoreProofEvidenceReport::from_body(r1.clone()).expect("restore envelope");
    let _: RestoreProofHash = envelope.body_hash;
    assert_eq!(envelope.body_hash, g1);
    let built_envelope =
        restore_proof_evidence_report(&manifest, &observed).expect("built envelope");
    assert_eq!(built_envelope.body_hash, g1);
    let mut findings = r1.findings.clone();
    findings.reverse();
    let r_perm = RestoreProofReportBody {
        findings,
        ..r1.clone()
    };
    assert_eq!(g1, restore_proof_report_body_hash(&r_perm).expect("rh2"));
    assert_eq!(r1.schema_version, RESTORE_PROOF_REPORT_SCHEMA_VERSION);
    let _ev: RestoreProofReportBody = r1.clone();
}

#[test]
fn backup_restore_finds_missing_and_mismatch_and_extra() {
    let manifest = sample_manifest();
    let bad_obs = vec![
        BackupSegmentRef {
            segment_id: 1,
            bytes_digest: dig(1),
        },
        BackupSegmentRef {
            segment_id: 2,
            bytes_digest: dig(7),
        },
        BackupSegmentRef {
            segment_id: 99,
            bytes_digest: dig(4),
        },
    ];
    let r = restore_proof_report_body(&manifest, &bad_obs).expect("report");
    assert!(
        r.findings.iter().any(|f| matches!(
            f,
            BackupEnvelopeFinding::SegmentBytesDigestMismatch { segment_id: 2, .. }
        )),
        "expected digest mismatch on id 2: {:?}",
        r.findings
    );
    assert!(
        r.findings.iter().any(|f| matches!(
            f,
            BackupEnvelopeFinding::UnexpectedObservedSegment { segment_id: 99 }
        )),
        "unexpected segment 99"
    );
    let partial = vec![BackupSegmentRef {
        segment_id: 1,
        bytes_digest: dig(1),
    }];
    let r2 = restore_proof_report_body(&manifest, &partial).expect("r2");
    assert!(
        r2.findings.iter().any(|f| matches!(
            f,
            BackupEnvelopeFinding::MissingExpectedSegment { segment_id: 2 }
        )),
        "missing id 2"
    );
}

#[test]
fn backup_duplicate_and_inconsistent_segment_findings() {
    let mut m = sample_manifest();
    m.segments = vec![
        BackupSegmentRef {
            segment_id: 1,
            bytes_digest: dig(1),
        },
        BackupSegmentRef {
            segment_id: 1,
            bytes_digest: dig(1),
        },
    ];
    let f = audit_backup_manifest_segments(&m);
    assert!(
        f.iter().any(|x| matches!(
            x,
            BackupEnvelopeFinding::DuplicateSegmentRef { segment_id: 1, .. }
        )),
        "{f:?}"
    );
    m.segments = vec![
        BackupSegmentRef {
            segment_id: 1,
            bytes_digest: dig(1),
        },
        BackupSegmentRef {
            segment_id: 1,
            bytes_digest: dig(2),
        },
    ];
    let f2 = audit_backup_manifest_segments(&m);
    assert!(
        f2.iter().any(|x| matches!(
            x,
            BackupEnvelopeFinding::InconsistentSegmentId { segment_id: 1, .. }
        )),
        "{f2:?}"
    );
}

#[test]
fn backup_unsupported_manifest_schema_version_is_a_finding() {
    let mut body = sample_manifest();
    body.schema_version = BACKUP_MANIFEST_BODY_SCHEMA_VERSION + 1;

    let findings = audit_backup_manifest_segments(&body);
    assert!(
        findings.iter().any(|f| matches!(
            f,
            BackupEnvelopeFinding::UnsupportedManifestBodySchemaVersion { observed, expected }
                if *observed == BACKUP_MANIFEST_BODY_SCHEMA_VERSION + 1
                    && *expected == BACKUP_MANIFEST_BODY_SCHEMA_VERSION
        )),
        "PROPERTY: unsupported backup manifest schema must be explicit evidence: {findings:?}"
    );

    let restore = restore_proof_report_body(&body, &body.segments).expect("restore proof");
    assert!(
        restore.findings.iter().any(|f| matches!(
            f,
            BackupEnvelopeFinding::UnsupportedManifestBodySchemaVersion { observed, expected }
                if *observed == BACKUP_MANIFEST_BODY_SCHEMA_VERSION + 1
                    && *expected == BACKUP_MANIFEST_BODY_SCHEMA_VERSION
        )),
        "PROPERTY: restore proof must preserve unsupported manifest schema evidence: {:?}",
        restore.findings
    );
}

#[test]
fn backup_envelope_metadata_does_not_change_manifest_hash() {
    let body = sample_manifest();
    let raw = backup_manifest_body_bytes(&body).expect("bytes");
    let e1 = BackupManifestEnvelope {
        body: body.clone(),
        envelope_schema_version: 1,
        generated_at_wall_ms: Some(1),
        diagnostic_note: Some("a".into()),
        signatures: vec![],
        attestations: vec![],
    };
    let e2 = BackupManifestEnvelope {
        body: body.clone(),
        envelope_schema_version: 1,
        generated_at_wall_ms: Some(99),
        diagnostic_note: Some("b".into()),
        signatures: vec![],
        attestations: vec![],
    };
    assert_eq!(
        backup_manifest_body_hash(&e1.body).expect("b1"),
        backup_manifest_body_hash(&e2.body).expect("b2")
    );
    assert_ne!(
        backup_manifest_envelope_hash(&e1).expect("eh1"),
        backup_manifest_envelope_hash(&e2).expect("eh2"),
        "envelope digest should move with metadata"
    );
    let key: SegmentBytesDigest = [3u8; 32];
    let envelope = BackupManifestEnvelope {
        body: body.clone(),
        envelope_schema_version: 1,
        generated_at_wall_ms: None,
        diagnostic_note: None,
        signatures: vec![SignatureEnvelope {
            signature: SignatureRef {
                algorithm_id: 1,
                key_id: key,
                signature_bytes: raw.clone(),
            },
        }],
        attestations: vec![],
    };
    let claimed = backup_manifest_body_hash(&body).expect("claim");
    let v: BackupManifestVerification =
        verify_backup_manifest_envelope(&envelope, claimed, echo_sig_ok).expect("verify");
    assert!(v.findings.is_empty());
    let bad_claim = [0u8; 32];
    let v2 = verify_backup_manifest_envelope(&envelope, bad_claim, echo_sig_ok).expect("verify2");
    assert!(
        v2.findings
            .iter()
            .any(|f| matches!(f, BackupEnvelopeFinding::ManifestBodyHashMismatch { .. })),
        "{:?}",
        v2.findings
    );
    let plane = verify_backup_manifest_signatures_only(&envelope, echo_sig_ok).expect("sig");
    assert_eq!(plane.body_hash, v.envelope_plane.body_hash);
    let _be: BackupManifestEnvelope = envelope.clone();
}

#[test]
fn backup_manifest_envelope_hash_helpers_normalize_segments() {
    let body = sample_manifest();
    let mut body_reversed = body.clone();
    body_reversed.segments.reverse();
    let e1 = BackupManifestEnvelope {
        body: body.clone(),
        envelope_schema_version: 1,
        generated_at_wall_ms: Some(10),
        diagnostic_note: Some("same".into()),
        signatures: vec![],
        attestations: vec![],
    };
    let e2 = BackupManifestEnvelope {
        body: body_reversed,
        ..e1.clone()
    };

    assert_eq!(
        e1.body_hash().expect("method body h1"),
        e2.body_hash().expect("method body h2"),
        "PROPERTY: backup envelope method must normalize segment order"
    );
    assert_eq!(
        e1.envelope_hash().expect("method env h1"),
        e2.envelope_hash().expect("method env h2"),
        "PROPERTY: backup envelope method must not expose raw artifact envelope hashing"
    );
    assert_eq!(
        backup_manifest_envelope_body_hash(&e1).expect("body h1"),
        backup_manifest_envelope_body_hash(&e2).expect("body h2"),
        "PROPERTY: envelope body helper must normalize segment order"
    );
    assert_eq!(
        backup_manifest_envelope_hash(&e1).expect("env h1"),
        backup_manifest_envelope_hash(&e2).expect("env h2"),
        "PROPERTY: envelope hash helper must normalize manifest body before framing"
    );
    assert_eq!(
        normalize_backup_manifest_envelope(&e1).body,
        normalize_backup_manifest_envelope(&e2).body
    );
}

#[test]
fn backup_manifest_roundtrip_file_stable_hash() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("manifest.mp");
    let body = sample_manifest();
    let bytes = backup_manifest_body_bytes(&body).expect("encode");
    let h0 = backup_manifest_body_hash(&body).expect("hash0");
    let mut f = std::fs::File::create(&path).expect("create");
    f.write_all(&bytes).expect("write");
    drop(f);
    let read_back = std::fs::read(&path).expect("read");
    let decoded: BackupManifestBody = encoding::from_bytes(&read_back).expect("decode");
    let h1 = backup_manifest_body_hash(&decoded).expect("hash1");
    assert_eq!(
        h0, h1,
        "PROPERTY: disk round-trip preserves canonical manifest identity"
    );
    let mut segs = decoded.segments.clone();
    sort_backup_segment_refs(&mut segs);
    assert_eq!(segs, normalize_backup_manifest_body(&body).segments);
}
