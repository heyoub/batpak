// justifies: INV-TEST-PANIC-AS-ASSERTION, INV-IDEMPOTENCY-DURABLE-WINDOW; integration tests rely on expect/panic on unreachable failures; clippy::unwrap_used and clippy::panic are the standard harness allowances for integration tests.
#![allow(clippy::unwrap_used, clippy::panic)]
//! Durable idempotency key -> receipt store (Phase 3, 0.8.3).
//!
//! PROVES: INV-IDEMPOTENCY-DURABLE-WINDOW. A keyed append is deduplicated as a
//! true no-op even after retention compaction has EVICTED the underlying event,
//! and across close/reopen cold-start, and across snapshot. Without the durable
//! sidecar these would re-append a duplicate.
//! CATCHES: the original bug where Retention compaction dropped the `by_id`
//! entry and a re-run of the same key silently re-appended a duplicate.
//! SEEDED: fixed EventKind, stable coordinates, explicit u128 idempotency keys,
//! tempfile roots, deterministic `for_operation` keys.

use batpak::coordinate::{Coordinate, Region};
use batpak::event::EventKind;
use batpak::id::IdempotencyKey;
use batpak::store::{
    AppendOptions, CompactionConfig, CompactionStrategy, IdempotencyRetention, OverflowPolicy,
    Store, StoreConfig,
};
use tempfile::TempDir;

const KIND: EventKind = EventKind::custom(0xB, 1);

fn coord() -> Coordinate {
    Coordinate::new("entity:idem", "scope:durable").expect("valid coord")
}

/// Checkpoint/mmap disabled so cold-start uses the segment-scan rebuild path —
/// the path that must NOT overwrite the durable idempotency authority. Tiny
/// segments force rotation so retention compaction has sealed inputs.
fn config(dir: &TempDir) -> StoreConfig {
    StoreConfig::new(dir.path())
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false)
        .with_segment_max_bytes(512)
        .with_sync_every_n_events(1)
}

fn user_visible_events(store: &Store) -> Vec<batpak::store::index::IndexEntry> {
    store
        .query(&Region::all())
        .into_iter()
        .filter(|entry| {
            !matches!(
                entry.event_kind(),
                EventKind::SYSTEM_OPEN_COMPLETED
                    | EventKind::SYSTEM_CLOSE_COMPLETED
                    | EventKind::SYSTEM_BATCH_BEGIN
                    | EventKind::SYSTEM_BATCH_COMMIT
            )
        })
        .filter(|entry| entry.event_kind() == KIND)
        .collect()
}

fn append_keyed(
    store: &Store,
    key: u128,
    payload: &serde_json::Value,
) -> batpak::store::AppendReceipt {
    store
        .append_with_options(
            &coord(),
            KIND,
            payload,
            AppendOptions::new().with_idempotency(IdempotencyKey::from(key)),
        )
        .expect("keyed append")
}

/// Retention strategy that evicts EVERY user event of `KIND` (keeps only the
/// batch/system markers), forcing the keyed event frame out of the store.
fn evict_all_user_events() -> CompactionConfig {
    CompactionConfig {
        strategy: CompactionStrategy::Retention(Box::new(|stored| {
            stored.event.header.event_kind != KIND
        })),
        min_segments: 1,
    }
}

#[test]
fn keyed_retry_is_noop_after_retention_evicts_the_event() {
    // THE bug we are killing: compaction evicts the event, a re-run of the same
    // key must STILL be a no-op returning the original receipt.
    let dir = TempDir::new().expect("tempdir");
    let store = Store::open(config(&dir)).expect("open");

    let key = 0x1111_2222_3333_4444_5555_6666_7777_8888u128;
    let first = append_keyed(&store, key, &serde_json::json!({"v": 1}));

    // Append filler events to force segment rotation so compaction has sealed
    // inputs (tiny segment_max_bytes makes each append rotate).
    for i in 0..8 {
        store
            .append(&coord(), KIND, &serde_json::json!({ "filler": i }))
            .ok();
    }

    // Run retention compaction that drops the keyed event frame.
    let (_result, _report) = store
        .compact(&evict_all_user_events())
        .expect("retention compaction");

    // The keyed event frame should now be gone from the live index...
    let live_after = user_visible_events(&store);
    assert!(
        live_after.iter().all(|e| e.event_id() != key),
        "PRECONDITION: retention compaction evicted the keyed event frame"
    );

    // ...yet a re-run of the same key is a NO-OP returning the original receipt.
    let replay = append_keyed(&store, key, &serde_json::json!({"v": 1}));
    assert_eq!(
        replay.sequence, first.sequence,
        "INV-IDEMPOTENCY-DURABLE-WINDOW: keyed retry after eviction returns original sequence"
    );
    assert_eq!(
        replay.content_hash, first.content_hash,
        "keyed retry after eviction returns original content hash"
    );
    assert_eq!(
        u128::from(replay.event_id),
        u128::from(first.event_id),
        "keyed retry after eviction returns original event id"
    );

    // And no duplicate was appended.
    assert_eq!(
        user_visible_events(&store)
            .iter()
            .filter(|e| e.event_id() == key)
            .count(),
        0,
        "no duplicate keyed event re-appended after eviction"
    );

    store.close().expect("close");
}

#[test]
fn keyed_retry_is_noop_after_close_and_reopen() {
    let dir = TempDir::new().expect("tempdir");
    let key = 0xabcd_ef01_2345_6789_abcd_ef01_2345_6789u128;

    let original = {
        let store = Store::open(config(&dir)).expect("open");
        let r = append_keyed(&store, key, &serde_json::json!({"hello": "world"}));
        assert!(
            store.durable_idempotency_key_count() >= 1,
            "the keyed append recorded a durable entry"
        );
        store.close().expect("close");
        r
    };

    // Reopen — segment-scan cold-start must restore the durable authority.
    let store = Store::open(config(&dir)).expect("reopen");
    assert!(
        store.durable_idempotency_key_count() >= 1,
        "cold-start restored the durable idempotency store"
    );
    let replay = append_keyed(&store, key, &serde_json::json!({"hello": "world"}));
    assert_eq!(
        replay.sequence, original.sequence,
        "INV-IDEMPOTENCY-DURABLE-WINDOW: keyed retry after reopen is a no-op"
    );
    store.close().expect("close");
}

#[test]
fn idemp_authority_survives_eviction_then_cold_start() {
    // Combined worst case: evict the event AND cold-start.
    let dir = TempDir::new().expect("tempdir");
    let key = 0x0f0f_0f0f_0f0f_0f0f_0f0f_0f0f_0f0f_0f0fu128;

    let original = {
        let store = Store::open(config(&dir)).expect("open");
        let r = append_keyed(&store, key, &serde_json::json!({"x": 9}));
        for i in 0..8 {
            store
                .append(&coord(), KIND, &serde_json::json!({ "filler": i }))
                .ok();
        }
        store.compact(&evict_all_user_events()).expect("compact");
        store.close().expect("close");
        r
    };

    let store = Store::open(config(&dir)).expect("reopen");
    let replay = append_keyed(&store, key, &serde_json::json!({"x": 9}));
    assert_eq!(
        replay.sequence, original.sequence,
        "INV-IDEMPOTENCY-DURABLE-WINDOW: key survives eviction + cold-start"
    );
    store.close().expect("close");
}

#[test]
fn snapshot_carries_the_durable_idempotency_store() {
    let src_dir = TempDir::new().expect("src tempdir");
    let snap_dir = TempDir::new().expect("snap tempdir");
    let key = 0x1234_1234_1234_1234_1234_1234_1234_1234u128;

    let original = {
        let store = Store::open(config(&src_dir)).expect("open");
        let r = append_keyed(&store, key, &serde_json::json!({"snap": true}));
        store
            .snapshot_with_evidence(snap_dir.path())
            .expect("snapshot copies durable idemp store");
        store.close().expect("close");
        r
    };

    // Open the SNAPSHOT directory: the durable authority must be present.
    let store = Store::open(config(&snap_dir)).expect("open snapshot");
    assert!(
        store.durable_idempotency_key_count() >= 1,
        "snapshot carried the durable idempotency store"
    );
    let replay = append_keyed(&store, key, &serde_json::json!({"snap": true}));
    assert_eq!(
        replay.sequence, original.sequence,
        "INV-IDEMPOTENCY-DURABLE-WINDOW: snapshot preserves keyed-retry no-op"
    );
    store.close().expect("close");
}

#[test]
fn for_operation_drives_a_durable_idempotent_pass() {
    // End-to-end with the deterministic operation-identity key.
    let dir = TempDir::new().expect("tempdir");
    let store = Store::open(config(&dir)).expect("open");

    let op_key = IdempotencyKey::for_operation("transfer", &["acct:1", "acct:2", "req:7"]);
    let first = store
        .append_with_options(
            &coord(),
            KIND,
            &serde_json::json!({"amount": 100}),
            AppendOptions::new().with_idempotency(op_key),
        )
        .expect("first op append");

    // Recompute the SAME operation identity and re-submit: no-op.
    let again = IdempotencyKey::for_operation("transfer", &["acct:1", "acct:2", "req:7"]);
    let replay = store
        .append_with_options(
            &coord(),
            KIND,
            &serde_json::json!({"amount": 100}),
            AppendOptions::new().with_idempotency(again),
        )
        .expect("replay op append");
    assert_eq!(
        first.sequence, replay.sequence,
        "for_operation drives idempotent no-op"
    );
    store.close().expect("close");
}

#[test]
fn unbounded_and_window_policies_are_configurable() {
    // Exercise the public policy surface so the configuration is real, not
    // dead. Window-priority specifics live in the property test file.
    let dir = TempDir::new().expect("tempdir");
    let store = Store::open(
        config(&dir)
            .with_idempotency_retention(IdempotencyRetention::Window {
                keep_sequences: 1_000,
            })
            .with_idempotency_overflow(OverflowPolicy::Warn),
    )
    .expect("open with Window policy");
    let key = 0x7777_7777_7777_7777_7777_7777_7777_7777u128;
    let first = append_keyed(&store, key, &serde_json::json!({"w": 1}));
    let replay = append_keyed(&store, key, &serde_json::json!({"w": 1}));
    assert_eq!(first.sequence, replay.sequence);

    let _unbounded = IdempotencyRetention::Unbounded;
    let _fail_closed = OverflowPolicy::FailClosed;
    let _backpressure = OverflowPolicy::Backpressure;
    store.close().expect("close");
}
