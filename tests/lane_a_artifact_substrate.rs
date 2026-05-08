// justifies: INV-TEST-PANIC-AS-ASSERTION; lane A substrate doctrine tests use panic for PROPERTY mismatches only.
#![allow(clippy::panic)]
//! PROVES: `batpak::artifact` separates body digest from envelope digest; signature/attachment vector order is
//! canonically sorted before hashing so permutations do not change `envelope_hash`.
//! CATCHES: body/envelope identity coupling; permutation-sensitive envelope hashing.
//! SEEDED: deterministic `DemoPayload` fixtures only.

use batpak::artifact::{
    artifact_body_bytes, artifact_envelope_hash_for, artifact_envelope_hash_from_identity,
    artifact_envelope_identity, artifact_verification_report_body_hash,
    verify_canonical_artifact_envelope, ArtifactEnvelopeFinding, ArtifactEnvelopeIdentity,
    ArtifactHash, ArtifactVerificationReport, AttestationRef, CanonicalArtifactEnvelope,
    SignatureEnvelope, SignatureRef, ARTIFACT_ENVELOPE_FRAMING_VERSION,
};
use batpak::encoding;

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
fn artifact_signature_vector_order_does_not_change_envelope_hash() {
    let mk_sig = |algo: u32, tag: u8| SignatureEnvelope {
        signature: SignatureRef {
            algorithm_id: algo,
            key_id: [tag; 32],
            signature_bytes: vec![tag],
        },
    };
    let body = DemoPayload { v: 5 };
    let forward = CanonicalArtifactEnvelope {
        body: body.clone(),
        envelope_schema_version: 1,
        generated_at_wall_ms: None,
        diagnostic_note: None,
        signatures: vec![mk_sig(1, 1), mk_sig(2, 2)],
        attestations: vec![],
    };
    let reversed = CanonicalArtifactEnvelope {
        signatures: vec![mk_sig(2, 2), mk_sig(1, 1)],
        ..forward.clone()
    };
    assert_eq!(
        forward.envelope_hash().expect("eh f"),
        reversed.envelope_hash().expect("eh r"),
        "PROPERTY: envelope hashing must canonical-sort signatures"
    );
}

#[test]
fn artifact_attestation_vector_order_does_not_change_envelope_hash() {
    let a = AttestationRef {
        kind_id: 1,
        bytes: vec![1],
    };
    let b = AttestationRef {
        kind_id: 2,
        bytes: vec![2],
    };
    let body = DemoPayload { v: 6 };
    let forward = CanonicalArtifactEnvelope {
        body: body.clone(),
        envelope_schema_version: 1,
        generated_at_wall_ms: None,
        diagnostic_note: None,
        signatures: vec![],
        attestations: vec![a.clone(), b.clone()],
    };
    let reversed = CanonicalArtifactEnvelope {
        attestations: vec![b, a],
        ..forward.clone()
    };
    assert_eq!(
        forward.envelope_hash().expect("e1"),
        reversed.envelope_hash().expect("e2"),
        "PROPERTY: envelope hashing must canonical-sort attestations"
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
