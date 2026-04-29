// justifies: INV-TEST-PANIC-AS-ASSERTION, INV-MACRO-BOUNDED-CAST; snapshot/compaction tests rely on unwrap/panic as assertion style and intentionally bounded fixture data.
#![allow(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    clippy::cast_possible_truncation,
    clippy::needless_borrows_for_generic_args,
    clippy::panic
)]
//! Snapshot and compaction contract tests extracted from `store_advanced.rs`.
//!
//! PROVES: snapshot preserves honest on-disk state and compaction preserves or
//! intentionally rewrites indexed state without leaking superseded artifacts.
//! DEFENDS: stale snapshot destination reuse, in-flight compaction races,
//! pending-compaction marker loss, hidden-range corruption, and pre-swap
//! rollback drift.

use batpak::prelude::*;
use batpak::store::{
    segment::{CompactionOutcome, CompactionResult},
    ReadOnly, Store, StoreConfig, StoreError, SyncConfig,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

#[path = "support/small_store.rs"]
mod small_store_support;
use small_store_support::small_segment_store as test_store;

#[test]
fn snapshot_copies_segments() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:snap", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    for i in 0..10 {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }
    store.sync().expect("sync");

    let snap_dir = TempDir::new().expect("snap dir");
    store.snapshot(snap_dir.path()).expect("snapshot");

    let fbat_count = std::fs::read_dir(snap_dir.path())
        .expect("read snap dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "fbat")
                .unwrap_or(false)
        })
        .count();

    assert!(
        fbat_count > 0,
        "PROPERTY: snapshot destination must contain at least one .fbat segment file.\n\
         Investigate: src/store/mod.rs snapshot.\n\
         Common causes: snapshot copies to wrong directory, segment files flushed after snapshot call.\n\
         Run: cargo test --test store_snapshot_compaction snapshot_copies_segments"
    );

    let snap_config = StoreConfig {
        data_dir: snap_dir.path().to_path_buf(),
        ..StoreConfig::new("")
    };
    let snap_store = Store::<ReadOnly>::open_read_only(snap_config).expect("open snapshot");
    let stats = snap_store.stats();
    assert_eq!(
        stats.event_count, 11,
        "PROPERTY: snapshot must preserve full event count — no events lost during copy.\n\
         Investigate: src/store/mod.rs snapshot.\n\
         Common causes: segment file not flushed before copy, partial write, index not rebuilt.\n\
         Run: cargo test --test store_snapshot_compaction snapshot_copies_segments"
    );
    store.close().expect("close");
}

fn user_visible_entries(store: &Store) -> Vec<batpak::store::IndexEntry> {
    store
        .query(&Region::all())
        .into_iter()
        .filter(|entry| {
            !matches!(
                entry.kind,
                EventKind::SYSTEM_OPEN_COMPLETED | EventKind::SYSTEM_CLOSE_COMPLETED
            )
        })
        .collect()
}

#[test]
fn snapshot_rejects_when_visibility_fence_is_active() {
    let (store, _dir) = test_store();
    let fence = store
        .begin_visibility_fence()
        .expect("begin visibility fence");
    let snap_dir = TempDir::new().expect("snap dir");

    let err = match store.snapshot(snap_dir.path()) {
        Ok(_) => panic!("PROPERTY: snapshot must not proceed while a visibility fence is active"),
        Err(err) => err,
    };
    assert!(
        matches!(err, StoreError::VisibilityFenceActive),
        "PROPERTY: snapshot with an active visibility fence must surface VisibilityFenceActive, got {err:?}"
    );

    fence.cancel().expect("cancel visibility fence");
    store.close().expect("close");
}

#[test]
fn snapshot_reused_destination_replaces_stale_store_artifacts() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:snap:source", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 7);
    for i in 0..6 {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append source");
    }
    let live_stats = store.stats();

    let snapshot_dir = TempDir::new().expect("snapshot dir");
    {
        let stale_store =
            Store::open(StoreConfig::new(snapshot_dir.path())).expect("open stale store");
        let stale_coord = Coordinate::new("entity:snap:stale", "scope:test").expect("stale coord");
        stale_store
            .append(&stale_coord, kind, &serde_json::json!({"stale": true}))
            .expect("append stale");
        stale_store.close().expect("close stale");
    }

    store
        .snapshot(snapshot_dir.path())
        .expect("snapshot into reused dir");

    let reopened = Store::<ReadOnly>::open_read_only(StoreConfig::new(snapshot_dir.path()))
        .expect("open snapshot");
    let snap_stats = reopened.stats();
    assert_eq!(
        snap_stats.event_count, live_stats.event_count,
        "PROPERTY: snapshot into a reused destination must clear stale store artifacts before copying."
    );
    assert_eq!(
        snap_stats.global_sequence, live_stats.global_sequence,
        "PROPERTY: snapshot into a reused destination must not keep stale cold-start artifacts or superseded segments."
    );
    store.close().expect("close source");
}

#[test]
fn snapshot_waits_for_in_flight_compaction() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 512,
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        ..StoreConfig::new("")
    };
    let store = Arc::new(Store::open(config).expect("open store"));
    let coord = Coordinate::new("entity:snapshot-vs-compact", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 0x44);
    let payload = "x".repeat(300);
    for i in 0..12 {
        store
            .append(&coord, kind, &serde_json::json!({"i": i, "blob": payload}))
            .expect("append");
    }

    let compaction_entered = Arc::new(AtomicBool::new(false));
    let allow_compaction_finish = Arc::new(AtomicBool::new(false));
    let compaction_store = Arc::clone(&store);
    let compaction_entered_thread = Arc::clone(&compaction_entered);
    let allow_compaction_finish_thread = Arc::clone(&allow_compaction_finish);
    let compaction = std::thread::Builder::new()
        .name("store-snapshot-compaction-vs-compact".into())
        .spawn(move || {
            compaction_store.compact(&CompactionConfig {
                min_segments: 1,
                strategy: CompactionStrategy::Retention(Box::new(move |_event| {
                    compaction_entered_thread.store(true, Ordering::SeqCst);
                    while !allow_compaction_finish_thread.load(Ordering::SeqCst) {
                        std::thread::yield_now();
                    }
                    true
                })),
            })
        })
        .expect("spawn compaction thread");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !compaction_entered.load(Ordering::SeqCst) {
        assert!(
            std::time::Instant::now() < deadline,
            "PROPERTY: compaction predicate should be entered before snapshot probe starts"
        );
        std::thread::yield_now();
    }

    let snapshot_dir = TempDir::new().expect("snapshot dir");
    let snapshot_dest = snapshot_dir.path().to_path_buf();
    let snapshot_store = Arc::clone(&store);
    let (snapshot_done_tx, snapshot_done_rx) = std::sync::mpsc::channel();
    let snapshot = std::thread::Builder::new()
        .name("store-snapshot-compaction-blocked-by-compact".into())
        .spawn(move || {
            let result = snapshot_store.snapshot(&snapshot_dest);
            let _ = snapshot_done_tx.send(result);
        })
        .expect("spawn snapshot thread");

    std::thread::sleep(std::time::Duration::from_millis(150));
    assert!(
        snapshot_done_rx.try_recv().is_err(),
        "PROPERTY: snapshot must not complete while compaction is mutating the on-disk segment set"
    );

    allow_compaction_finish.store(true, Ordering::SeqCst);
    let compaction_result = compaction.join().expect("join compaction thread");
    assert!(
        matches!(
            compaction_result.expect("compact result").outcome,
            CompactionOutcome::Performed | CompactionOutcome::Skipped
        ),
        "compaction should finish honestly once the test releases the predicate gate"
    );

    snapshot_done_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("snapshot completion after compaction")
        .expect("snapshot result");
    snapshot.join().expect("join snapshot thread");

    let reopened = Store::<ReadOnly>::open_read_only(StoreConfig::new(snapshot_dir.path()))
        .expect("open snapshot");
    let live_stats = store.stats();
    let snap_stats = reopened.stats();
    assert_eq!(
        snap_stats.event_count, live_stats.event_count,
        "PROPERTY: snapshot that starts during compaction must serialize behind compaction and reopen to the same event count as the live store"
    );
    assert_eq!(
        snap_stats.global_sequence, live_stats.global_sequence,
        "PROPERTY: snapshot that starts during compaction must preserve the live store watermark after compaction finishes"
    );
    let store = match Arc::try_unwrap(store) {
        Ok(store) => store,
        Err(_) => panic!("snapshot/compaction threads must release the store Arc"),
    };
    store.close().expect("close");
}

#[test]
fn snapshot_preserves_pending_compaction_marker() {
    let (store, dir) = test_store();
    let coord = Coordinate::new("entity:snapshot:marker", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 0x66);
    store
        .append(&coord, kind, &serde_json::json!({"i": 0}))
        .expect("append");
    std::fs::write(
        dir.path().join("compaction.pending.json"),
        br#"{"merged_id":1,"source_segment_ids":[1]}"#,
    )
    .expect("write pending compaction marker");

    let snapshot_dir = TempDir::new().expect("snapshot dir");
    store.snapshot(snapshot_dir.path()).expect("snapshot");

    assert!(
        snapshot_dir.path().join("compaction.pending.json").exists(),
        "PROPERTY: snapshot must preserve pending-compaction markers so reopen semantics match the source store"
    );

    store.close().expect("close");
}

#[test]
fn compact_does_not_lose_data() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:compact", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    for i in 0..5 {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }

    let compaction: CompactionResult = store
        .compact(&CompactionConfig {
            min_segments: 1,
            ..CompactionConfig::default()
        })
        .expect("compact");
    let compaction_outcome = compaction.outcome;
    assert!(
        matches!(
            compaction_outcome,
            CompactionOutcome::Performed | CompactionOutcome::Skipped
        ),
        "PROPERTY: compact() over a populated store must either perform a merge or honestly report that nothing was compactable"
    );

    let stats = store.stats();
    assert_eq!(
        stats.event_count, 6,
        "PROPERTY: compact() must not lose any events — all 5 appended events must remain.\n\
         Investigate: src/store/mod.rs compact, src/store/segment/mod.rs compaction path.\n\
         Common causes: compaction drops events below tombstone horizon, segment replaced before flush.\n\
         Run: cargo test --test store_snapshot_compaction compact_does_not_lose_data"
    );

    store.close().expect("close");
}

#[test]
fn compact_merge_rebuild_does_not_duplicate_superseded_sealed_segments() {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 512,
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("entity:compact:dedupe", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 3);

    for i in 0..12 {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }
    store.close().expect("close");

    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 512,
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("reopen");
    let compaction = store
        .compact(&CompactionConfig {
            min_segments: 1,
            ..CompactionConfig::default()
        })
        .expect("compact");
    assert!(
        matches!(compaction.outcome, CompactionOutcome::Performed),
        "PROPERTY: forced merge compaction should perform once multiple sealed segments exist."
    );

    let all = user_visible_entries(&store);
    let mut ids: Vec<_> = all.iter().map(|entry| entry.event_id).collect();
    ids.sort_unstable();
    ids.dedup();

    assert_eq!(
        all.len(),
        12,
        "PROPERTY: post-compaction rebuild must not re-index superseded sealed segments alongside the merged segment."
    );
    assert_eq!(
        ids.len(),
        12,
        "PROPERTY: compact() must leave exactly one indexed copy of each event after merging sealed segments."
    );

    let segment_count = std::fs::read_dir(dir.path())
        .expect("read data dir")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .map(|ext| ext == "fbat")
                .unwrap_or(false)
        })
        .count();
    assert_eq!(
        segment_count,
        2,
        "PROPERTY: after merge compaction, the data dir should contain only the merged sealed segment plus the active segment."
    );

    store.close().expect("close");
}

#[test]
fn compact_fails_closed_on_corrupt_hidden_ranges_metadata() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 512,
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("entity:compact:hidden-ranges", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 0x55);

    for i in 0..12 {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append");
    }
    std::fs::write(dir.path().join("visibility_ranges.fbv"), b"corrupt")
        .expect("write corrupt hidden ranges metadata");

    let result = store
        .compact(&CompactionConfig {
            min_segments: 1,
            ..CompactionConfig::default()
        })
        .expect("compact result");
    let CompactionOutcome::Failed { reason } = result.outcome else {
        panic!("expected compaction failure on corrupt hidden ranges");
    };
    assert!(
        reason.contains("visibility-ranges"),
        "PROPERTY: corrupt hidden-ranges metadata must abort compaction before swap with an explicit reason, got {reason}"
    );

    assert_eq!(
        store.stats().event_count,
        13,
        "PROPERTY: failed compaction on corrupt hidden-ranges metadata must leave the live event count unchanged"
    );

    store.close().expect("close");
}

#[test]
fn compact_rolls_back_marker_on_pre_swap_rename_failure() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 512,
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("entity:compact:rollback", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 0x56);
    let payload = "x".repeat(300);
    for i in 0..12 {
        store
            .append(&coord, kind, &serde_json::json!({"i": i, "blob": payload}))
            .expect("append");
    }

    let mut segment_ids: Vec<u64> = std::fs::read_dir(dir.path())
        .expect("read data dir")
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let is_segment = path.extension().map(|ext| ext == "fbat").unwrap_or(false);
            if !is_segment {
                return None;
            }
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|stem| stem.parse::<u64>().ok())
        })
        .collect();
    segment_ids.sort_unstable();
    let merged_id = *segment_ids.first().expect("sealed segment id");
    let blocker = dir.path().join(format!("{merged_id:06}.fbat.compact-src"));
    std::fs::create_dir_all(&blocker).expect("create rename blocker");

    let result = store
        .compact(&CompactionConfig {
            min_segments: 1,
            ..CompactionConfig::default()
        })
        .expect("compact result");
    let CompactionOutcome::Failed { reason } = result.outcome else {
        panic!("expected compaction failure on pre-swap rename blocker");
    };
    assert!(
        reason.contains("pre-swap phase failed"),
        "PROPERTY: pre-swap rename failure must surface as a rolled-back compaction failure, got {reason}"
    );

    assert!(
        !dir.path().join("compaction.pending.json").exists(),
        "PROPERTY: failed pre-swap compaction must clear the pending marker during rollback"
    );
    assert_eq!(
        store.stats().event_count,
        13,
        "PROPERTY: failed pre-swap compaction must leave the live event count unchanged"
    );

    store.close().expect("close");
}

#[test]
fn compact_retention_removes_dropped_events_from_index() {
    let dir = TempDir::new().expect("create temp dir");
    let keep_kind = EventKind::custom(0xF, 1);
    let drop_kind = EventKind::custom(0xF, 2);

    let mut drop_ids = Vec::new();
    {
        let config = StoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_bytes: 512,
            sync: SyncConfig {
                every_n_events: 1,
                ..SyncConfig::default()
            },
            ..StoreConfig::new("")
        };
        let store = Store::open(config).expect("open store");
        let coord = Coordinate::new("entity:retention", "scope:test").expect("valid coord");

        for i in 0..10 {
            let kind = if i % 2 == 0 { keep_kind } else { drop_kind };
            let receipt = store
                .append(&coord, kind, &serde_json::json!({"i": i}))
                .expect("append");
            if i % 2 != 0 {
                drop_ids.push(receipt.event_id);
            }
        }
        store.close().expect("close");
    }

    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 512,
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("reopen");

    let retention: batpak::store::RetentionPredicate =
        Box::new(move |stored| stored.event.header.event_kind == keep_kind);
    let retention_config = CompactionConfig {
        strategy: CompactionStrategy::Retention(retention),
        min_segments: 1,
    };
    store.compact(&retention_config).expect("compact");

    for dropped_id in &drop_ids {
        let get_result = store.get(*dropped_id);
        let err = get_result.expect_err(
            "COMPACT RETENTION INDEX LEAK: get() should return NotFound after retention compaction dropped the event.\
             Investigate: src/store/mod.rs compact(), src/store/index/mod.rs clear()."
        );
        assert!(
            matches!(err, StoreError::NotFound(_)),
            "PROPERTY: get() on a compaction-dropped event must surface as StoreError::NotFound, got {err:?}"
        );
    }

    assert_eq!(
        user_visible_entries(&store).len(),
        5,
        "COMPACT RETENTION COUNT: expected 5 kept user events after dropping 5.\n\
         Investigate: src/store/mod.rs compact() index rebuild."
    );

    store.close().expect("close");
}

#[test]
fn compact_tombstone_updates_event_kind_in_index() {
    let dir = TempDir::new().expect("create temp dir");
    let live_kind = EventKind::custom(0xF, 1);
    let doomed_kind = EventKind::custom(0xF, 2);
    let tombstone_kind = EventKind::TOMBSTONE;

    {
        let config = StoreConfig {
            data_dir: dir.path().to_path_buf(),
            segment_max_bytes: 512,
            sync: SyncConfig {
                every_n_events: 1,
                ..SyncConfig::default()
            },
            ..StoreConfig::new("")
        };
        let store = Store::open(config).expect("open store");
        let coord = Coordinate::new("entity:tombstone", "scope:test").expect("valid coord");

        for i in 0..10 {
            let kind = if i % 2 == 0 { live_kind } else { doomed_kind };
            store
                .append(&coord, kind, &serde_json::json!({"i": i}))
                .expect("append");
        }
        store.close().expect("close");
    }

    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 512,
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("reopen");

    let tombstone_config = CompactionConfig {
        strategy: CompactionStrategy::Tombstone(Box::new(move |stored| {
            stored.event.header.event_kind == live_kind
        })),
        min_segments: 1,
    };
    store.compact(&tombstone_config).expect("compact");

    assert_eq!(
        store.stream("entity:tombstone").len(),
        10,
        "COMPACT TOMBSTONE COUNT: expected all 10 user events to remain (5 live + 5 tombstoned).\n\
         Investigate: src/store/mod.rs compact() tombstone path."
    );

    let region = Region::entity("entity:tombstone").with_fact(KindFilter::Exact(tombstone_kind));
    let tombstoned = store.query(&region);
    assert_eq!(
        tombstoned.len(),
        5,
        "COMPACT TOMBSTONE KIND: expected 5 events with tombstone kind in index after compaction.\n\
         Investigate: src/store/mod.rs compact() index rebuild, tombstone_kind.\n\
         Common causes: index not rebuilt after compaction, kind not updated."
    );

    store.close().expect("close");
}
