// justifies: INV-TEST-PANIC-AS-ASSERTION; canonical patch-stability golden tests assert via panic and intentionally support explicit fixture regeneration.
#![allow(clippy::panic, clippy::print_stderr)]
//! Patch-stability tests for public evidence body bytes.
//!
//! PROVES: INV-CANONICAL-PATCH-STABILITY
//! CATCHES: accidental field-order, serde-shape, or report-body identity drift
//! across patch releases.
//! SEEDED: deterministic golden body plus proptest-generated equal snapshots.

use batpak::artifact::{
    verify_canonical_artifact_envelope, AttestationRef, CanonicalArtifactEnvelope,
    SignatureEnvelope, SignatureRef,
};
use batpak::prelude::*;
use batpak::registry::{
    registry_drift_findings_sorted, registry_drift_report_body_hash, registry_row_body_hash,
    registry_verification_report_body_hash, NamedDigest, RegistryDriftReportBody, RegistryRowBody,
    RegistryRowId, RegistryVerificationFinding, RegistryVerificationReport,
    REGISTRY_DRIFT_REPORT_SCHEMA_VERSION, REGISTRY_LIFECYCLE_LIVE,
    REGISTRY_ROW_BODY_SCHEMA_VERSION, REGISTRY_VERIFICATION_REPORT_SCHEMA_VERSION,
};
use batpak::reservation::{
    reservation_ledger_report_body_hash, reservation_reconciliation_report,
    reservation_reconciliation_report_body_hash, simulate_reservation_ledger, ReservationCauseRef,
    ReservationId, ReservationLedgerReportBody, ReservationReconciliationReportBody,
    ReservationSubjectRef, ReservationTransition, RESERVATION_OP_COMMIT, RESERVATION_OP_REFUND,
    RESERVATION_OP_RESERVE, RESERVATION_TRANSITION_SCHEMA_VERSION,
};
use batpak::schema::{compare_schema_snapshot, SchemaSnapshot, SchemaSnapshotReportBody};
use batpak::store::backup_envelope::{
    backup_manifest_body_hash, normalize_backup_manifest_body, restore_proof_report_body,
    restore_proof_report_body_hash, BackupManifestBody, BackupSegmentRef, RestoreProofReportBody,
    BACKUP_MANIFEST_BODY_SCHEMA_VERSION,
};
use batpak::store::{
    report_skipped, store_resource_report_body_from_diagnostics, store_resource_report_body_hash,
    ActiveSegmentReadEvidence, ChainWalkReportBody, ClockEvidence, CompactionReportBody,
    LockLeafSymlinkProtection, MmapAdmissionSummary, MmapEvidence, ParentDirSyncAdmissionSummary,
    ParentDirSyncEvidence, ReadWalkReportBody, StoreLockAdmissionSummary, StorePathStatusEvidence,
    StoreResourceReportBody, SubscriberFrontierReportBody,
};
use batpak::transition::{
    build_state_transition_report, state_transition_report_body_hash, StateTransitionEvent,
    StateTransitionReportBody, TransitionCauseRef, TransitionId, TransitionMachineId,
    TransitionSubjectId, STATE_TRANSITION_EVENT_SCHEMA_VERSION,
};
use proptest::prelude::*;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::error::Error;

#[path = "common/proptest.rs"]
mod proptest_support;

#[path = "support/small_store.rs"]
mod small_store_support;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn golden_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn check_or_update_golden(name: &str, actual_bytes: &[u8]) {
    let path = golden_dir().join(name);
    let actual_hex = hex_encode(actual_bytes);
    let updating = std::env::var("GOLDEN_UPDATE").as_deref() == Ok("I_KNOW_WHAT_IM_DOING");
    if updating {
        eprintln!("GOLDEN_UPDATE: regenerating golden file {}", path.display());
        std::fs::write(&path, &actual_hex)
            .unwrap_or_else(|error| panic!("failed to write {}: {error}", path.display()));
        return;
    }

    let expected_hex = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "golden file {} not found: {error}. Run \
             GOLDEN_UPDATE=I_KNOW_WHAT_IM_DOING cargo test -p batpak --test canonical_patch_stability",
            path.display()
        )
    });
    assert_eq!(
        actual_hex.trim(),
        expected_hex.trim(),
        "CANONICAL PATCH DRIFT: {name} no longer matches {}",
        path.display()
    );
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn body_hash_via_canonical(body: &(impl Serialize + Sized)) -> Result<[u8; 32], Box<dyn Error>> {
    let bytes = batpak::canonical::to_bytes(body)?;
    Ok(evidence_content_hash(&bytes))
}

fn assert_golden_round_trip<T>(name: &str, body: &T) -> TestResult
where
    T: Serialize + DeserializeOwned + PartialEq + core::fmt::Debug,
{
    let bytes = batpak::canonical::to_bytes(body)?;
    check_or_update_golden(name, &bytes);
    let decoded: T = batpak::canonical::from_bytes(&bytes)?;
    assert_eq!(
        decoded, *body,
        "PROPERTY: {name} golden bytes must round-trip through batpak canonical encoding",
    );
    Ok(())
}

fn evidence_content_hash(bytes: &[u8]) -> [u8; 32] {
    #[cfg(feature = "blake3")]
    {
        batpak::event::hash::compute_hash(bytes)
    }
    #[cfg(not(feature = "blake3"))]
    {
        let crc = crc32fast::hash(bytes).to_be_bytes();
        let mut out = [0_u8; 32];
        out[..4].copy_from_slice(&crc);
        out
    }
}

fn sample_report_body(
) -> Result<SchemaSnapshotReportBody, batpak::schema::SchemaSnapshotReportError> {
    let expected = SchemaSnapshot::from_hashes("schema.snapshot.report.v1", [0x11; 32], [0x22; 32]);
    let observed = SchemaSnapshot::from_hashes("schema.snapshot.report.v1", [0x11; 32], [0x23; 32]);
    Ok(compare_schema_snapshot(&expected, &observed)?.body)
}

fn digest(tag: u8) -> [u8; 32] {
    [tag; 32]
}

fn sample_artifact_report() -> TestResult<batpak::artifact::ArtifactVerificationReport> {
    let envelope = CanonicalArtifactEnvelope {
        body: serde_json::json!({ "fixture": "artifact-body", "n": 7 }),
        envelope_schema_version: 1,
        generated_at_wall_ms: Some(123_456),
        diagnostic_note: Some("fixture-note".into()),
        signatures: vec![SignatureEnvelope {
            signature: SignatureRef {
                algorithm_id: 1,
                key_id: digest(0xA1),
                signature_bytes: vec![0xCA, 0xFE],
            },
        }],
        attestations: vec![AttestationRef {
            kind_id: 2,
            bytes: vec![0xBA, 0xD0],
        }],
    };
    Ok(verify_canonical_artifact_envelope(
        &envelope,
        |_sig, _body| Err("fixture signature denied".into()),
    )?)
}

fn sample_registry_row_body() -> RegistryRowBody {
    RegistryRowBody {
        schema_version: REGISTRY_ROW_BODY_SCHEMA_VERSION,
        row_id: RegistryRowId(digest(0x11)),
        row_kind: 42,
        row_layout_version: 1,
        opaque_payload: b"registry-row".to_vec(),
        named_digests: vec![
            NamedDigest {
                name: "zeta".into(),
                digest: digest(0x2A),
            },
            NamedDigest {
                name: "alpha".into(),
                digest: digest(0x2B),
            },
        ],
        lifecycle: REGISTRY_LIFECYCLE_LIVE,
        supersedes: Some(RegistryRowId(digest(0x10))),
    }
}

fn sample_registry_drift_body() -> RegistryDriftReportBody {
    let expected = vec![
        (RegistryRowId(digest(0x01)), digest(0xA1)),
        (RegistryRowId(digest(0x02)), digest(0xA2)),
    ];
    let observed = vec![
        (RegistryRowId(digest(0x02)), digest(0xB2)),
        (RegistryRowId(digest(0x03)), digest(0xB3)),
    ];
    RegistryDriftReportBody {
        schema_version: REGISTRY_DRIFT_REPORT_SCHEMA_VERSION,
        findings: registry_drift_findings_sorted(&expected, &observed),
        expected,
        observed,
    }
}

fn sample_registry_verification_body() -> TestResult<RegistryVerificationReport> {
    Ok(RegistryVerificationReport {
        schema_version: REGISTRY_VERIFICATION_REPORT_SCHEMA_VERSION,
        envelope_plane: sample_artifact_report()?,
        findings: vec![RegistryVerificationFinding::InvalidLifecycle {
            row_id: RegistryRowId(digest(0x44)),
            lifecycle: 99,
        }],
    })
}

fn sample_backup_manifest_body() -> BackupManifestBody {
    BackupManifestBody {
        schema_version: BACKUP_MANIFEST_BODY_SCHEMA_VERSION,
        backup_id: digest(0x55),
        layout_revision: 3,
        tooling_revision: 9,
        segments: vec![
            BackupSegmentRef {
                segment_id: 2,
                bytes_digest: digest(0x62),
            },
            BackupSegmentRef {
                segment_id: 1,
                bytes_digest: digest(0x61),
            },
        ],
    }
}

fn sample_restore_proof_body() -> TestResult<RestoreProofReportBody> {
    Ok(restore_proof_report_body(
        &sample_backup_manifest_body(),
        &[
            BackupSegmentRef {
                segment_id: 1,
                bytes_digest: digest(0xFF),
            },
            BackupSegmentRef {
                segment_id: 3,
                bytes_digest: digest(0x63),
            },
        ],
    )?)
}

fn sample_transition_report_body() -> TestResult<StateTransitionReportBody> {
    let event = StateTransitionEvent {
        schema_version: STATE_TRANSITION_EVENT_SCHEMA_VERSION,
        machine_id: TransitionMachineId(digest(0x71)),
        subject_id: TransitionSubjectId(digest(0x72)),
        previous_state: 1,
        next_state: 9,
        transition_id: TransitionId(digest(0x73)),
        causes: vec![
            TransitionCauseRef {
                lane: 2,
                opaque_key: vec![2],
            },
            TransitionCauseRef {
                lane: 1,
                opaque_key: vec![1],
            },
        ],
        ordering_sequence: Some(77),
        frontier_digest: Some(digest(0x74)),
    };
    Ok(build_state_transition_report(&event, &[(1, 2)])?)
}

fn reservation_tx(
    sequence: u64,
    id: ReservationId,
    op: u32,
    quantity_units: u64,
    subject: Option<ReservationSubjectRef>,
) -> ReservationTransition {
    ReservationTransition {
        schema_version: RESERVATION_TRANSITION_SCHEMA_VERSION,
        sequence,
        reservation_id: id,
        op,
        quantity_units,
        subject,
        cause_refs: vec![
            ReservationCauseRef {
                lane: 2,
                opaque_key: vec![2],
            },
            ReservationCauseRef {
                lane: 1,
                opaque_key: vec![1],
            },
        ],
    }
}

fn sample_reservation_ledger_body() -> TestResult<ReservationLedgerReportBody> {
    Ok(simulate_reservation_ledger(&[
        reservation_tx(
            2,
            ReservationId(digest(0x81)),
            RESERVATION_OP_COMMIT,
            0,
            None,
        ),
        reservation_tx(
            1,
            ReservationId(digest(0x82)),
            RESERVATION_OP_RESERVE,
            4,
            Some(ReservationSubjectRef {
                namespace: 7,
                key_bytes: b"subject".to_vec(),
            }),
        ),
        reservation_tx(
            3,
            ReservationId(digest(0x82)),
            RESERVATION_OP_REFUND,
            0,
            None,
        ),
    ])?)
}

fn sample_reservation_reconciliation_body() -> TestResult<ReservationReconciliationReportBody> {
    let ledger = sample_reservation_ledger_body()?;
    Ok(reservation_reconciliation_report(&ledger.entries_sorted))
}

fn sample_compaction_report_body() -> TestResult<CompactionReportBody> {
    Ok(report_skipped(
        &CompactionConfig::default(),
        9,
        &[
            (2, std::path::PathBuf::from("002.fbat")),
            (1, std::path::PathBuf::from("001.fbat")),
        ],
    )?)
}

fn sample_store_reports() -> TestResult<(
    ChainWalkReportBody,
    SubscriberFrontierReportBody,
    ProjectionRunReportBody,
    ReadWalkReportBody,
    StoreResourceReportBody,
)> {
    let mut chain_findings = vec![
        ChainWalkFinding::TruncatedByLimit {
            limit: 2,
            next_parent_hash: digest(0x91),
        },
        ChainWalkFinding::EndNotReached {
            expected_end_event_id: 77,
        },
    ];
    chain_findings.sort();
    let chain = ChainWalkReportBody {
        schema_version: CHAIN_WALK_REPORT_SCHEMA_VERSION,
        mode: ChainWalkMode::Linear,
        checked_count: 2,
        first_ref: Some(101),
        last_ref: Some(99),
        walk_digest: digest(0x92),
        findings: chain_findings,
    };

    let mut subscriber_findings = vec![
        SubscriberFrontierFinding::ExactDroppedRange {
            start_sequence: 2,
            end_sequence: 4,
        },
        SubscriberFrontierFinding::LossObserved {
            precision: LossPrecision::ExactRange,
        },
    ];
    subscriber_findings.sort();
    let subscriber = SubscriberFrontierReportBody {
        schema_version: SUBSCRIBER_FRONTIER_REPORT_SCHEMA_VERSION,
        source: SubscriberFrontierSource::LossyPush,
        consumed_frontier_sequence: Some(1),
        available_frontier_sequence: 9,
        lag_events: Some(8),
        delivery_state: SubscriberDeliveryState::Active,
        loss_precision: LossPrecision::ExactRange,
        findings: subscriber_findings,
    };

    let projection = ProjectionRunReportBody {
        schema_version: PROJECTION_RUN_REPORT_SCHEMA_VERSION,
        projection_id: "golden.projection.v1".into(),
        source_refs: vec![
            ProjectionSourceRef::Entity {
                entity: "entity:canonical-golden".into(),
            },
            ProjectionSourceRef::RelevantKind {
                category: 0xE,
                type_id: 0x71,
            },
        ],
        replay_mode: ProjectionRunReplayMode::Current,
        requested_freshness: ProjectionRunRequestedFreshness::Consistent,
        observed_freshness: ProjectionRunFreshnessStatus::Fresh,
        input_frontier: Some(ProjectionRunInputFrontier {
            kind: ProjectionRunFrontierKind::Visible,
            wall_ms: 42,
            global_sequence: 9,
        }),
        output_hash: ProjectionRunOutputHash::Known(digest(0x93)),
        cache_status: ProjectionRunCacheStatus::Miss,
        checkpoint_ref: ProjectionRunCheckpointRef::NotApplicable,
        findings: Vec::new(),
    };

    let mut read_findings = vec![
        ReadWalkFinding::LimitedResults { dropped_count: 3 },
        ReadWalkFinding::MissingBackingEntry { event_id: 55 },
    ];
    read_findings.sort();
    let read_walk = ReadWalkReportBody {
        schema_version: READ_WALK_REPORT_SCHEMA_VERSION,
        source_refs: vec![
            ReadWalkSourceRef::Scope {
                scope: "scope:canonical-golden".into(),
            },
            ReadWalkSourceRef::FactExact {
                category: 0xE,
                type_id: 0x81,
            },
        ],
        replay_mode: ReadWalkReplayMode::Current,
        freshness_intent: ReadWalkFreshnessIntent::Consistent,
        input_frontier: Some(ReadWalkInputFrontier {
            kind: ReadWalkFrontierKind::Visible,
            wall_ms: 43,
            global_sequence: 10,
        }),
        requested_limit: Some(1),
        matched_count: 4,
        returned_count: 1,
        dropped_limited_count: ReadWalkDroppedCount::Known(3),
        proof_refs: ReadWalkProofRefs::Known(vec![ReadWalkProofRef {
            event_id: 55,
            global_sequence: 10,
            event_hash: digest(0x94),
        }]),
        findings: read_findings,
    };

    let (store, _guard) = small_store_support::small_segment_store()?;
    let mut resource = store_resource_report_body_from_diagnostics(&store.diagnostics());
    resource.schema_version = STORE_RESOURCE_REPORT_SCHEMA_VERSION;
    resource.data_dir_identity_hash = digest(0x95);
    resource.event_count = 4;
    resource.global_sequence = 10;
    resource.visible_sequence = 11;
    resource.segment_max_bytes = 4096;
    resource.fd_budget = 64;
    resource.restart_policy = StoreResourceRestartPolicyShape::Bounded {
        max_restarts: 1,
        within_ms: 1_000,
    };
    resource.writer_pressure = WriterPressure {
        queue_len: 1,
        capacity: 128,
    };
    resource.frontier = StoreResourceFrontierBody {
        accepted_wall_ms: 40,
        accepted_global_sequence: 8,
        written_wall_ms: 41,
        written_global_sequence: 9,
        durable_wall_ms: 41,
        durable_global_sequence: 9,
        visible_wall_ms: 42,
        visible_global_sequence: 10,
        applied_wall_ms: 42,
        applied_global_sequence: 10,
        emitted_wall_ms: 43,
        emitted_global_sequence: 10,
        visible_minus_durable_seq: 1,
        oldest_pending_write_age_ms: Some(5),
    };
    resource.index_topology = "aos".into();
    resource.tile_count = 0;
    resource.open_report = None;
    resource
        .platform_evidence
        .host
        .process_clock_epoch_marker_ns = 123;
    resource.platform_evidence.host.monotonic_clock = ClockEvidence::ProcessLocalInstantAnchor;
    resource.platform_evidence.store_path.path_status = StorePathStatusEvidence::ObservedDirectory;
    resource.platform_evidence.store_path.parent_dir_sync = ParentDirSyncEvidence::UnixFsync;
    resource
        .platform_evidence
        .store_path
        .lock_leaf_symlink_protection = LockLeafSymlinkProtection::AtomicNoFollow;
    resource.platform_evidence.store_path.mmap_index = MmapEvidence::FileBacked;
    resource.platform_evidence.store_path.sealed_segment_mmap = MmapEvidence::FileBacked;
    resource.platform_evidence.store_path.active_segment_read =
        ActiveSegmentReadEvidence::UnixReadAt;
    resource.platform_evidence.admission.store_lock = StoreLockAdmissionSummary::AtomicNoFollow;
    resource.platform_evidence.admission.parent_dir_sync = ParentDirSyncAdmissionSummary::UnixFsync;
    resource.platform_evidence.admission.mmap_index = MmapAdmissionSummary::FileBacked;
    resource.platform_evidence.admission.sealed_segment_mmap = MmapAdmissionSummary::FileBacked;

    Ok((chain, subscriber, projection, read_walk, resource))
}

proptest! {
    #![proptest_config(proptest_support::cfg(256))]

    #[test]
    fn schema_snapshot_report_body_canonical_bytes_are_patch_stable_for_equal_logical_inputs(
        stable_id in "[a-z][a-z0-9_.:-]{0,31}",
        schema_hash in any::<[u8; 32]>(),
        fixture_hash in any::<[u8; 32]>(),
    ) {
        let expected = SchemaSnapshot::from_hashes(stable_id, schema_hash, fixture_hash);
        let observed = expected.clone();

        let first = compare_schema_snapshot(&expected, &observed)?;
        let second = compare_schema_snapshot(&expected, &observed)?;
        let first_bytes = batpak::canonical::to_bytes(&first.body)?;
        let second_bytes = batpak::canonical::to_bytes(&second.body)?;
        let decoded: SchemaSnapshotReportBody = batpak::canonical::from_bytes(&first_bytes)?;

        prop_assert_eq!(&first.body, &second.body);
        prop_assert_eq!(&first_bytes, &second_bytes);
        prop_assert_eq!(decoded, first.body);
        prop_assert_eq!(first.body_hash, evidence_content_hash(&first_bytes));
    }
}

#[test]
fn schema_snapshot_report_body_hash_matches_generated_canonical_bytes() -> TestResult {
    let body = sample_report_body()?;
    let report_hash = body_hash_via_canonical(&body)?;
    let report = compare_schema_snapshot(
        &SchemaSnapshot::from_hashes("schema.snapshot.report.v1", [0x11; 32], [0x22; 32]),
        &SchemaSnapshot::from_hashes("schema.snapshot.report.v1", [0x11; 32], [0x23; 32]),
    )?;

    assert_eq!(
        report.body_hash, report_hash,
        "PROPERTY: schema snapshot report body_hash must equal hash(canonical(body))"
    );
    Ok(())
}

#[test]
fn schema_snapshot_report_body_v1_golden_bytes_do_not_drift() -> TestResult {
    let body = sample_report_body()?;
    assert_golden_round_trip("schema_snapshot_report_body_v1.hex", &body)
}

#[test]
fn substrate_report_body_goldens_do_not_drift() -> TestResult {
    let artifact = sample_artifact_report()?;
    assert_eq!(
        artifact.body_hash,
        body_hash_via_canonical(&serde_json::json!({ "fixture": "artifact-body", "n": 7 }))?,
        "PROPERTY: artifact report anchors the exact canonical body bytes used for signing"
    );
    assert_golden_round_trip("artifact_verification_report_v1.hex", &artifact)?;

    let row = sample_registry_row_body();
    let normalized_row = batpak::registry::normalize_registry_row_body(&row);
    assert_eq!(
        registry_row_body_hash(&row)?,
        body_hash_via_canonical(&normalized_row)?,
        "PROPERTY: registry row hash must equal hash(canonical(normalized row body))",
    );
    assert_golden_round_trip("registry_row_body_v1.hex", &normalized_row)?;

    let drift = sample_registry_drift_body();
    assert_eq!(
        registry_drift_report_body_hash(&drift)?,
        body_hash_via_canonical(&drift)?,
        "PROPERTY: registry drift sample is already normalized for golden identity",
    );
    assert_golden_round_trip("registry_drift_report_body_v1.hex", &drift)?;

    let verification = sample_registry_verification_body()?;
    assert_eq!(
        registry_verification_report_body_hash(&verification)?,
        body_hash_via_canonical(&verification)?,
        "PROPERTY: registry verification sample is already normalized for golden identity",
    );
    assert_golden_round_trip("registry_verification_report_v1.hex", &verification)?;

    let backup = sample_backup_manifest_body();
    let normalized_backup = normalize_backup_manifest_body(&backup);
    assert_eq!(
        backup_manifest_body_hash(&backup)?,
        body_hash_via_canonical(&normalized_backup)?,
        "PROPERTY: backup manifest hash must equal hash(canonical(normalized body))",
    );
    assert_golden_round_trip("backup_manifest_body_v1.hex", &normalized_backup)?;

    let restore = sample_restore_proof_body()?;
    assert_eq!(
        restore_proof_report_body_hash(&restore)?,
        body_hash_via_canonical(&restore)?,
        "PROPERTY: restore proof sample is already normalized for golden identity",
    );
    assert_golden_round_trip("restore_proof_report_body_v1.hex", &restore)?;

    let transition = sample_transition_report_body()?;
    assert_eq!(
        state_transition_report_body_hash(&transition)?,
        body_hash_via_canonical(&transition)?,
        "PROPERTY: transition report sample is already normalized for golden identity",
    );
    assert_golden_round_trip("state_transition_report_body_v1.hex", &transition)?;

    let ledger = sample_reservation_ledger_body()?;
    assert_eq!(
        reservation_ledger_report_body_hash(&ledger)?,
        body_hash_via_canonical(&ledger)?,
        "PROPERTY: reservation ledger sample is already normalized for golden identity",
    );
    assert_golden_round_trip("reservation_ledger_report_body_v1.hex", &ledger)?;

    let reconciliation = sample_reservation_reconciliation_body()?;
    assert_eq!(
        reservation_reconciliation_report_body_hash(&reconciliation)?,
        body_hash_via_canonical(&reconciliation)?,
        "PROPERTY: reservation reconciliation hash must equal hash(canonical(body))",
    );
    assert_golden_round_trip(
        "reservation_reconciliation_report_body_v1.hex",
        &reconciliation,
    )?;

    let compaction = sample_compaction_report_body()?;
    assert_eq!(
        compaction.body_hash()?,
        body_hash_via_canonical(&compaction)?,
        "PROPERTY: compaction report sample is already normalized for golden identity",
    );
    assert_golden_round_trip("compaction_report_body_v1.hex", &compaction)?;
    Ok(())
}

#[test]
fn store_report_body_goldens_do_not_drift() -> TestResult {
    let (chain, subscriber, projection, read_walk, resource) = sample_store_reports()?;

    assert_golden_round_trip("chain_walk_report_body_v1.hex", &chain)?;
    assert_golden_round_trip("subscriber_frontier_report_body_v1.hex", &subscriber)?;
    assert_golden_round_trip("projection_run_report_body_v1.hex", &projection)?;
    assert_golden_round_trip("read_walk_report_body_v1.hex", &read_walk)?;
    assert_eq!(
        store_resource_report_body_hash(&resource)?,
        body_hash_via_canonical(&resource)?,
        "PROPERTY: store resource body hash must equal hash(canonical(body))",
    );
    assert_golden_round_trip("store_resource_report_body_v1.hex", &resource)?;
    Ok(())
}
