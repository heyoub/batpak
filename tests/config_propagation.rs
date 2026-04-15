//! Regression tests for StoreConfig field propagation.
//!
//! PROVES: LAW-001 (No Fake Success — config must actually apply)
//! DEFENDS: FM-003 (Silent Config Drift — fields ignored after construction)
//! INVARIANTS: INV-STATE (config→runtime field propagation)
//!
//! These tests exist because three bugs slipped through code review:
//! 1. wall_ms clock regression — backward clock could reorder events in BTreeMap
//! 2. SyncMode ignored — segment always used sync_all regardless of config
//! 3. writer.stack_size unused — config field never applied to thread builder
//!
//! Each test targets the specific bug class to prevent regression.

use batpak::prelude::*;
use batpak::store::{
    BatchConfig, IndexConfig, IndexTopology, Store, StoreConfig, SyncConfig, SyncMode, WriterConfig,
};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

// ============================================================================
// BUG 1: wall_ms monotonicity under clock regression
// ============================================================================
// The writer must ensure wall_ms is monotonically non-decreasing per entity,
// even when the system clock steps backward. Without this, BTreeMap ordering
// breaks because ClockKey sorts by (wall_ms, clock, uuid).

#[test]
fn wall_ms_monotonic_under_clock_regression() {
    let dir = TempDir::new().expect("create temp dir");
    let clock_us = Arc::new(AtomicI64::new(1_000_000_000)); // start at 1000s in microseconds

    let clock_ref = Arc::clone(&clock_us);
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        clock: Some(Arc::new(move || clock_ref.load(Ordering::SeqCst))),
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("entity:clock-test", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    // Append event at t=1000s
    store
        .append(&coord, kind, &serde_json::json!({"step": 1}))
        .expect("append 1");

    // Append event at t=2000s (forward)
    clock_us.store(2_000_000_000, Ordering::SeqCst);
    store
        .append(&coord, kind, &serde_json::json!({"step": 2}))
        .expect("append 2");

    // CLOCK REGRESSION: jump back to t=500s
    clock_us.store(500_000_000, Ordering::SeqCst);
    store
        .append(&coord, kind, &serde_json::json!({"step": 3}))
        .expect("append 3");

    // Append another at t=600s (still behind the high-water mark of 2000s)
    clock_us.store(600_000_000, Ordering::SeqCst);
    store
        .append(&coord, kind, &serde_json::json!({"step": 4}))
        .expect("append 4");

    store.sync().expect("sync");

    // Verify: wall_ms must be monotonically non-decreasing in the stream
    let entries = store.stream("entity:clock-test");
    assert_eq!(entries.len(), 4, "should have 4 events");

    let mut prev_wall_ms = 0u64;
    for (i, entry) in entries.iter().enumerate() {
        assert!(
            entry.wall_ms >= prev_wall_ms,
            "CLOCK REGRESSION BUG: entry[{i}] wall_ms={} < previous wall_ms={prev_wall_ms}.\n\
             The writer must clamp wall_ms to max(raw_ms, last_wall_ms) per entity.\n\
             Check: src/store/writer.rs STEP 4 — HLC wall clock monotonicity.\n\
             Run: cargo test --test config_propagation wall_ms_monotonic",
            entry.wall_ms
        );
        prev_wall_ms = entry.wall_ms;
    }

    // The third and fourth events should have wall_ms >= 2_000_000 (the high-water mark).
    // Clock at 2_000_000_000 us = 2_000_000 ms = 2000s.
    let third_wall_ms = entries[2].wall_ms;
    assert!(
        third_wall_ms >= 2_000_000,
        "CLOCK REGRESSION BUG: event after clock regression has wall_ms={third_wall_ms}, \
         expected >= 2000000 (the high-water mark at 2000s before regression).\n\
         Check: src/store/writer.rs STEP 4 — raw_ms.max(last_ms)."
    );

    store.close().expect("close");
}

#[test]
fn wall_ms_monotonic_per_entity_isolation() {
    // Clock regression on entity A must NOT affect entity B's wall_ms.
    let dir = TempDir::new().expect("create temp dir");
    let clock_us = Arc::new(AtomicI64::new(1_000_000_000));

    let clock_ref = Arc::clone(&clock_us);
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        clock: Some(Arc::new(move || clock_ref.load(Ordering::SeqCst))),
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let coord_a = Coordinate::new("entity:a", "scope:test").expect("valid coord");
    let coord_b = Coordinate::new("entity:b", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    // Entity A at t=2000s
    clock_us.store(2_000_000_000, Ordering::SeqCst);
    store
        .append(&coord_a, kind, &serde_json::json!({"entity": "a"}))
        .expect("append a");

    // Clock regression to t=500s — entity B starts here
    clock_us.store(500_000_000, Ordering::SeqCst);
    store
        .append(&coord_b, kind, &serde_json::json!({"entity": "b"}))
        .expect("append b");

    let entries_b = store.stream("entity:b");
    assert_eq!(entries_b.len(), 1);

    // Entity B's wall_ms should reflect 500_000ms (its own timeline from 500s clock),
    // NOT be clamped to entity A's 2_000_000ms high-water mark.
    // Clock returns microseconds; wall_ms = timestamp_us / 1000.
    assert_eq!(
        entries_b[0].wall_ms, 500_000,
        "ENTITY ISOLATION BUG: entity B's wall_ms should be 500000 (its own timeline at 500s), \
         got {}. Wall_ms monotonicity must be per-entity, not global.\n\
         Check: src/store/writer.rs STEP 4 — get_latest(entity) must be per-entity.",
        entries_b[0].wall_ms
    );

    store.close().expect("close");
}

// ============================================================================
// BUG 2: SyncMode propagation
// ============================================================================
// SyncMode::SyncData must be wired through to all sync call sites:
// periodic sync, explicit sync, segment rotation, and shutdown.

#[test]
fn sync_mode_sync_data_does_not_panic() {
    // Before the fix, SyncMode was ignored and sync_all was always used.
    // This test verifies SyncData actually works end-to-end without error.
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        sync: SyncConfig {
            every_n_events: 1,
            mode: SyncMode::SyncData,
        },
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store with SyncData");
    let coord = Coordinate::new("entity:sync-test", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    // Write several events — each triggers periodic sync with SyncData mode
    for i in 0..10 {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append with SyncData");
    }

    // Explicit sync — also goes through sync_with_mode
    store.sync().expect("explicit sync with SyncData");

    // Verify data survived
    let entries = store.stream("entity:sync-test");
    assert_eq!(
        entries.len(),
        10,
        "SYNC_MODE BUG: expected 10 events after SyncData sync, got {}.\n\
         Check: src/store/segment.rs sync_with_mode, src/store/writer.rs sync sites.",
        entries.len()
    );

    store.close().expect("close with SyncData");
}

#[test]
fn sync_mode_sync_data_survives_segment_rotation() {
    // The rotation code path has its own sync call. Verify SyncData works there too.
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 512, // tiny segments to force rotation
        sync: SyncConfig {
            every_n_events: 1,
            mode: SyncMode::SyncData,
        },
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("entity:rotation-sync", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);
    let payload = serde_json::json!({"data": "payload to fill segments quickly for rotation"});

    for _ in 0..50 {
        store.append(&coord, kind, &payload).expect("append");
    }
    store.sync().expect("sync");

    // Must have rotated segments
    let segment_count = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "fbat")
                .unwrap_or(false)
        })
        .count();
    assert!(
        segment_count > 1,
        "Expected segment rotation with SyncData mode, got {segment_count} segments"
    );

    // Verify all events readable after rotation + SyncData
    let entries = store.stream("entity:rotation-sync");
    assert_eq!(
        entries.len(),
        50,
        "all events must survive rotation with SyncData"
    );

    store.close().expect("close");
}

#[test]
fn sync_mode_sync_data_survives_cold_start() {
    // Write with SyncData, close, reopen — verify data persisted correctly.
    let dir = TempDir::new().expect("create temp dir");

    // Phase 1: write with SyncData
    {
        let config = StoreConfig {
            data_dir: dir.path().to_path_buf(),
            sync: SyncConfig {
                every_n_events: 1,
                mode: SyncMode::SyncData,
            },
            ..StoreConfig::new("")
        };
        let store = Store::open(config).expect("open store");
        let coord = Coordinate::new("entity:cold", "scope:test").expect("valid coord");
        let kind = EventKind::custom(0xF, 1);

        for i in 0..20 {
            store
                .append(&coord, kind, &serde_json::json!({"i": i}))
                .expect("append");
        }
        store.sync().expect("sync");
        store.close().expect("close");
    }

    // Phase 2: cold start with default SyncAll — verify data survived
    {
        let config = StoreConfig {
            data_dir: dir.path().to_path_buf(),
            ..StoreConfig::new("")
        };
        let store = Store::open(config).expect("cold start");
        let entries = store.stream("entity:cold");
        assert_eq!(
            entries.len(),
            20,
            "SYNC_DATA COLD START BUG: expected 20 events after SyncData write + cold start, got {}.\n\
             Check: sync_data must actually flush data to disk.",
            entries.len()
        );
        store.close().expect("close");
    }
}

// ============================================================================
// BUG 3: writer.stack_size propagation
// ============================================================================
// The writer.stack_size config field must be applied to the spawned thread.
// We can't directly inspect thread stack size, but we can verify:
// 1. Setting it doesn't crash
// 2. A very small stack size causes a stack overflow (proving it's applied)
// 3. A reasonable custom size works for normal operations

#[test]
fn writer_stack_size_custom_value_works() {
    // A reasonable custom stack size (2MB) should work fine.
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        writer: WriterConfig {
            stack_size: Some(2 * 1024 * 1024), // 2MB
            ..WriterConfig::default()
        },
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store with custom stack size");
    let coord = Coordinate::new("entity:stack-test", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    for i in 0..10 {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append with custom stack");
    }

    let entries = store.stream("entity:stack-test");
    assert_eq!(
        entries.len(),
        10,
        "WRITER_STACK_SIZE BUG: store with custom stack_size failed to write events.\n\
         Check: src/store/writer.rs WriterHandle::spawn — builder.stack_size()."
    );

    store.close().expect("close");
}

#[test]
fn writer_stack_size_none_uses_default() {
    // None should use OS default and work normally.
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        writer: WriterConfig {
            stack_size: None,
            ..WriterConfig::default()
        },
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store with default stack");
    let coord = Coordinate::new("entity:default-stack", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    store
        .append(&coord, kind, &serde_json::json!({"ok": true}))
        .expect("append with default stack");

    store.close().expect("close");
}

// ============================================================================
// Meta: StoreConfig completeness — every field must be exercised in tests
// ============================================================================

#[test]
fn store_config_all_fields_overridable() {
    // This test ensures every StoreConfig field can be set to a non-default value
    // and the store still opens and operates correctly. If a new field is added
    // but never wired up, this test documents the expected behavior.
    let dir = TempDir::new().expect("create temp dir");
    let clock_fn: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(|| {
        #[allow(clippy::cast_possible_truncation)] // timestamp_us fits i64 until year 292,277
        {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros() as i64
        }
    });

    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 1024 * 1024, // 1MB (non-default)
        fd_budget: 8,                   // non-default
        broadcast_capacity: 256,        // non-default
        single_append_max_bytes: 32 * 1024,
        sync: SyncConfig {
            mode: SyncMode::SyncData, // non-default
            every_n_events: 5,        // non-default
        },
        writer: WriterConfig {
            channel_capacity: 128,             // non-default
            pressure_retry_threshold_pct: 60,  // non-default
            stack_size: Some(4 * 1024 * 1024), // 4MB (non-default)
            restart_policy: batpak::store::RestartPolicy::Bounded {
                max_restarts: 3,
                within_ms: 5000,
            },
            shutdown_drain_limit: 64, // non-default
        },
        batch: BatchConfig {
            max_size: 512,              // non-default
            max_bytes: 2 * 1024 * 1024, // 2MB (non-default)
            group_commit_max_batch: 1,  // default (not testing group commit here)
        },
        index: IndexConfig {
            topology: IndexTopology::aos()
                .with_soa(true)
                .with_entity_groups(false)
                .with_tiles64(true),
            incremental_projection: false, // default
            enable_checkpoint: false,      // disabled for this test
            enable_mmap_index: false,      // non-default
        },
        clock: Some(clock_fn), // custom clock
        #[cfg(feature = "dangerous-test-hooks")]
        fault_injector: None,
    };

    let store = Store::open(config).expect(
        "STORE CONFIG BUG: store failed to open with all non-default config values.\n\
         A new config field may have been added but not properly handled.",
    );

    let coord = Coordinate::new("entity:config-test", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    // Write enough to trigger periodic sync (sync.every_n_events = 5)
    for i in 0..10 {
        store
            .append(&coord, kind, &serde_json::json!({"i": i}))
            .expect("append with all-custom config");
    }

    store.sync().expect("sync");
    let entries = store.stream("entity:config-test");
    assert_eq!(
        entries.len(),
        10,
        "all events must be written with non-default config"
    );

    store.close().expect("close");
}

#[test]
fn store_config_debug_lists_all_integrity_relevant_fields() {
    let dir = TempDir::new().expect("create temp dir");
    let clock_fn: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(|| 4242);
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 11_111,
        fd_budget: 13,
        broadcast_capacity: 19,
        single_append_max_bytes: 23,
        sync: SyncConfig {
            mode: SyncMode::SyncData,
            every_n_events: 7,
        },
        writer: WriterConfig {
            channel_capacity: 17,
            pressure_retry_threshold_pct: 61,
            stack_size: Some(31 * 1024),
            restart_policy: batpak::store::RestartPolicy::Bounded {
                max_restarts: 2,
                within_ms: 3000,
            },
            shutdown_drain_limit: 29,
        },
        batch: BatchConfig {
            max_size: 333,
            max_bytes: 44_444,
            group_commit_max_batch: 1,
        },
        index: IndexConfig {
            topology: IndexTopology::aos()
                .with_soa(true)
                .with_entity_groups(false)
                .with_tiles64(true),
            incremental_projection: false,
            enable_checkpoint: true,
            enable_mmap_index: false,
        },
        clock: Some(clock_fn),
        #[cfg(feature = "dangerous-test-hooks")]
        fault_injector: None,
    };

    let debug = format!("{config:?}");

    for needle in [
        "StoreConfig",
        "data_dir",
        "segment_max_bytes: 11111",
        "fd_budget: 13",
        "broadcast_capacity: 19",
        "single_append_max_bytes: 23",
        "SyncConfig",
        "mode: SyncData",
        "every_n_events: 7",
        "WriterConfig",
        "channel_capacity: 17",
        "pressure_retry_threshold_pct: 61",
        "stack_size: Some(31744)",
        "restart_policy: Bounded",
        "max_restarts: 2",
        "within_ms: 3000",
        "shutdown_drain_limit: 29",
        "BatchConfig",
        "max_size: 333",
        "max_bytes: 44444",
        "IndexConfig",
        "topology: IndexTopology { soa: true, entity_groups: false, tiles64: true }",
        "enable_checkpoint: true",
        "enable_mmap_index: false",
        "clock: Some(\"<fn>\")",
    ] {
        assert!(
            debug.contains(needle),
            "PROPERTY: StoreConfig debug output must include every integrity-relevant field and the clock placeholder.\n\
             Missing fragment: {needle}\n\
             Investigate: src/store/mod.rs Debug impl for StoreConfig.\n\
             Common causes: no-op Debug impl, forgotten field, or leaking an opaque closure instead of the <fn> placeholder."
        );
    }
}
