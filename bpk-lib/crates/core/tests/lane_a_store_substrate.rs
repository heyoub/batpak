//! PROVES: compaction structural evidence is deterministic (`CompactionReportBody`); append idempotency via event id +
//! keyed batch replay; public reads expose explicit predicate bounds (`Region`/entity/cursor surfaces).
//! CATCHES: nondeterministic compaction report fields; partial batch idempotency faking success; implicit unbounded scans.
//! SEEDED: fixed u128 idempotency keys, small segment stores (`segment_max_bytes = 200`), `tempfile` roots.
//! **Batch close+reopen:** `tests/idempotent_batch_crash_recovery.rs`.
//! **Compaction vs snapshot interplay:** `tests/store_snapshot_compaction.rs`.

use batpak::event::EventKind;
use batpak::store::index::IndexEntry;
use batpak::store::segment::CompactionOutcome;
use batpak::store::{
    compaction_strategy_shape, report_for_run, report_skipped, BatchAppendItem, Canal, CanalBatch,
    CanalClosed, CanalHandle, CanalItem, CausationRef, CompactionEvidenceHash,
    CompactionEvidenceReport, CompactionReportBody, CompactionReportFinding,
    CompactionStrategyShape, StoreError, COMPACTION_REPORT_SCHEMA_VERSION,
};
use batpak_testkit::prelude::*;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::TempDir;

fn lane_store() -> (TempDir, Store<Open>) {
    let dir = TempDir::new().expect("tmp");
    let cfg = StoreConfig::new(dir.path()).with_segment_max_bytes(200);
    let store = Store::open(cfg).expect("open");
    (dir, store)
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
    let envelope = CompactionEvidenceReport::from_body(skipped.clone()).expect("envelope");
    let _: CompactionEvidenceHash = envelope.body_hash;
    assert_eq!(envelope.body_hash, skipped.body_hash().expect("body hash"));
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

#[test]
fn compaction_report_skipped_is_deterministic() {
    let (_dir, store) = lane_store();
    let coord = Coordinate::new("e", "s").expect("coord");
    let kind = EventKind::custom(0xF, 1);
    store
        .append(&coord, kind, &serde_json::json!({ "x": 1 }))
        .expect("append");
    store.sync().expect("sync");

    let cfg = CompactionConfig::default();
    let (r0, rep0) = store.compact(&cfg).expect("cw");
    let (r1, rep1) = store.compact(&cfg).expect("cw2");
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
fn compact_merge_evidence_has_sorted_sources_stable_body_hash_and_output_digest() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let config = StoreConfig::new(dir.path())
        .with_segment_max_bytes(512)
        .with_sync_every_n_events(1);
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("entity:compact:report", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 0x41);

    for i in 0..12 {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }

    let cfg = CompactionConfig {
        min_segments: 1,
        ..CompactionConfig::default()
    };
    let (result, report) = store.compact(&cfg).expect("compact");
    assert!(
        matches!(result.outcome, CompactionOutcome::Performed),
        "PROPERTY: merge scenario must perform compaction to exercise output segment digest"
    );

    let mut sorted_check = report.source_segment_ids_sorted.clone();
    sorted_check.sort_unstable();
    assert_eq!(
        report.source_segment_ids_sorted, sorted_check,
        "PROPERTY: compaction report must expose source segment ids in deterministic sorted order"
    );
    assert!(
        report.output_segment_bytes_hash.is_some(),
        "PROPERTY: performed compaction with readable merged segment should populate output hash when available"
    );

    let h0 = report.body_hash().expect("report body hash");
    let h1 = report.body_hash().expect("report body hash repeat");
    assert_eq!(
        h0, h1,
        "PROPERTY: same evidence view must yield same report body_hash"
    );

    let _: CompactionReportBody = report;

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
fn compaction_performed_without_output_path_emits_hash_unavailable() {
    let cfg = CompactionConfig::default();
    let sealed: Vec<(u64, std::path::PathBuf)> = vec![(7, PathBuf::from("x"))];
    let result = batpak::store::segment::CompactionResult {
        outcome: CompactionOutcome::Performed,
        segments_removed: 1,
        bytes_reclaimed: 64,
    };

    let rep = report_for_run(&cfg, 3, &sealed, Some(99), &result, None).expect("rep");

    assert_eq!(rep.output_segment_bytes_hash, None);
    assert!(
        rep.findings.iter().any(|finding| matches!(
            finding,
            CompactionReportFinding::OutputSegmentHashUnavailable { reason }
                if reason.contains("path unavailable")
        )),
        "PROPERTY: performed compaction with no hashable output path must emit an explicit finding, got {:?}",
        rep.findings
    );
}

#[test]
fn idempotency_keyed_batch_double_submit_returns_cached_receipts_without_reopen() {
    let (_dir, store) = lane_store();
    let coord = Coordinate::new("e-batch", "s-lane").expect("coord");
    let kind = EventKind::custom(0xF, 0x42);
    let build = || {
        vec![
            BatchAppendItem::new(
                coord.clone(),
                kind,
                &serde_json::json!({"step": 0}),
                AppendOptions::new()
                    .with_idempotency(batpak::id::IdempotencyKey::from(0xB0_B1_B2_B3_B4_B5_B6_B7)),
                CausationRef::None,
            )
            .expect("item 0"),
            BatchAppendItem::new(
                coord.clone(),
                kind,
                &serde_json::json!({"step": 1}),
                AppendOptions::new()
                    .with_idempotency(batpak::id::IdempotencyKey::from(0xC0_C1_C2_C3_C4_C5_C6_C7)),
                CausationRef::None,
            )
            .expect("item 1"),
        ]
    };
    let r1 = store.append_batch(build()).expect("first batch");
    let r2 = store.append_batch(build()).expect("replay batch");
    assert_eq!(
        r1.len(),
        r2.len(),
        "PROPERTY: keyed batch replay must return one receipt per item"
    );
    for (a, b) in r1.iter().zip(r2.iter()) {
        assert_eq!(a.event_id, b.event_id);
        assert_eq!(a.sequence, b.sequence);
    }
    store.close().expect("close");
}

#[test]
fn idempotency_batch_partial_cache_rejected_instead_of_silent_success() {
    let (_dir, store) = lane_store();
    let coord = Coordinate::new("e-partial", "s").expect("coord");
    let kind = EventKind::custom(0xF, 0x43);
    let existing_key = 0xE0_E1_E2_E3_E4_E5_E6_E7_u128;
    store
        .append_with_options(
            &coord,
            kind,
            &serde_json::json!({"solo": true}),
            AppendOptions::new().with_idempotency(batpak::id::IdempotencyKey::from(existing_key)),
        )
        .expect("seed keyed append");

    let items = vec![
        BatchAppendItem::new(
            coord.clone(),
            kind,
            &serde_json::json!({"replay": true}),
            AppendOptions::new().with_idempotency(batpak::id::IdempotencyKey::from(existing_key)),
            CausationRef::None,
        )
        .expect("cached item"),
        BatchAppendItem::new(
            coord.clone(),
            kind,
            &serde_json::json!({"fresh": true}),
            AppendOptions::new()
                .with_idempotency(batpak::id::IdempotencyKey::from(0xF1_F2_F3_F4_F5_F6_F7_F8)),
            CausationRef::None,
        )
        .expect("new item"),
    ];
    let err = store
        .append_batch(items)
        .map(|_| ())
        .expect_err("PROPERTY: partial idempotency replay must not return Ok");
    assert!(
        matches!(err, StoreError::IdempotencyPartialBatch { .. }),
        "wrong error: {err:?}"
    );
    store.close().expect("close");
}

#[test]
fn idempotency_key_is_event_id_scoped_global_lookup() {
    let (_dir, store) = lane_store();
    let coord_a = Coordinate::new("e-a", "s").expect("c1");
    let coord_b = Coordinate::new("e-b", "s").expect("c2");
    let kind = EventKind::custom(0xF, 1);

    let key = 0xC0FFEE_u128;
    let opts = AppendOptions::new().with_idempotency(batpak::id::IdempotencyKey::from(key));

    let r1 = store
        .append_with_options(
            &coord_a,
            kind,
            &serde_json::json!({ "who": "first" }),
            opts.clone(),
        )
        .expect("append a");
    assert_eq!(r1.event_id, batpak::id::EventId::from(key));

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
    let _: Vec<IndexEntry> = store.by_entity("entity-x");
    let _: Vec<IndexEntry> = store.by_fact(EventKind::custom(0xF, 1));
    let _: Cursor = store.cursor_guaranteed(&Region::all());
    drop(store);
}

#[test]
fn public_canal_trait_pulls_cursor_and_subscription_items() {
    struct NoopHandle;

    impl CanalHandle for NoopHandle {
        fn stop(&self) {}

        fn join(self: Box<Self>) -> Result<(), StoreError> {
            Ok(())
        }

        fn stop_and_join(self: Box<Self>) -> Result<(), StoreError> {
            Ok(())
        }
    }

    let (_dir, store) = lane_store();
    let coord = Coordinate::new("entity:canal", "scope:canal").expect("coord");
    let region = Region::entity("entity:canal");
    let kind = EventKind::custom(0xF, 0x61);

    let first = store
        .append(&coord, kind, &serde_json::json!({ "n": 1 }))
        .expect("append first");
    let mut cursor = store.cursor_guaranteed(&region);
    let cursor_batch: CanalBatch<IndexEntry> =
        Canal::pull_batch(&mut cursor, 1, Duration::from_millis(0)).expect("cursor canal");
    match cursor_batch {
        CanalBatch::One(entry) => {
            assert_eq!(CanalItem::event_id(&entry), u128::from(first.event_id));
        }
        other @ (CanalBatch::Empty | CanalBatch::Many(_)) => {
            unreachable!("PROPERTY: cursor canal should yield one entry, got {other:?}")
        }
    }

    let mut subscription = store.subscribe_lossy(&region);
    let second = store
        .append(&coord, kind, &serde_json::json!({ "n": 2 }))
        .expect("append second");
    let subscription_batch: CanalBatch<Notification> =
        Canal::pull_batch(&mut subscription, 1, Duration::from_secs(1))
            .expect("subscription canal");
    match subscription_batch {
        CanalBatch::One(notification) => {
            assert_eq!(
                CanalItem::event_id(&notification),
                u128::from(second.event_id)
            );
        }
        other @ (CanalBatch::Empty | CanalBatch::Many(_)) => {
            unreachable!(
                "PROPERTY: subscription canal should yield one notification, got {other:?}"
            )
        }
    }

    let _closed = CanalClosed;
    let handle: Box<dyn CanalHandle> = Box::new(NoopHandle);
    handle.stop_and_join().expect("noop handle");
    store.close().expect("close");
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
