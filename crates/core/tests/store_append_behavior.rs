// justifies: INV-TEST-PANIC-AS-ASSERTION, INV-MACRO-BOUNDED-CAST; advanced store tests rely on unwrap/panic as assertion style, spawn threads for concurrency probes, and narrow bounded test data into target types that the fixture guarantees fit.
#![allow(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    clippy::cast_possible_truncation,
    clippy::needless_borrows_for_generic_args,
    clippy::panic
)]
//! Advanced Store append and append-option integration tests.

use batpak::prelude::*;
use batpak::store::{Store, StoreConfig, StoreError, StoreStats, SyncConfig};
use batpak::typestate::Transition;
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

// Test-local EventPayload used by the apply_transition test. FREEZE-7 removed
// `Transition::new(kind, payload)`, so transitions can no longer be built from
// a raw `serde_json::Value`; the payload type must impl `EventPayload`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, batpak::EventPayload)]
#[batpak(category = 0x0A, type_id = 1)]
struct PublishedDoc {
    title: String,
    from: String,
    to: String,
}

#[path = "support/small_store.rs"]
mod small_store_support;

fn test_store() -> (Store, TempDir) {
    small_store_support::small_segment_store().expect("small segment store")
}

// --- append_reaction ---

#[test]
fn append_reaction_links_causation() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:react", "scope:test").expect("valid coord");
    let kind_cmd = EventKind::custom(0xF, 1);
    let kind_evt = EventKind::custom(0xF, 2);

    // Root cause event
    let root = store
        .append(&coord, kind_cmd, &serde_json::json!({"cmd": "create"}))
        .expect("root append");

    // Reaction event linked to root
    let reaction = store
        .append_reaction(
            &coord,
            kind_evt,
            &serde_json::json!({"evt": "created"}),
            root.event_id,
            root.event_id,
        )
        .expect("reaction append");

    // Verify: reaction has different event_id
    assert_ne!(
        root.event_id, reaction.event_id,
        "PROPERTY: append_reaction must produce a new unique event_id distinct from its cause.\n\
         Investigate: src/store/mod.rs append_reaction.\n\
         Common causes: event_id generation reuses the cause ID, hash collision in tiny test set.\n\
         Run: cargo test --test store_advanced append_reaction_links_causation"
    );

    // Verify: can retrieve both
    let root_stored = store.get(root.event_id).expect("get root");
    let react_stored = store.get(reaction.event_id).expect("get reaction");
    assert_eq!(
        root_stored.event.event_kind(),
        kind_cmd,
        "PROPERTY: root event must retain its original EventKind after being stored.\n\
         Investigate: src/store/mod.rs append, src/store/segment/mod.rs write_frame.\n\
         Common causes: event_kind field not serialised, wrong frame read back.\n\
         Run: cargo test --test store_advanced append_reaction_links_causation"
    );
    assert_eq!(
        react_stored.event.event_kind(),
        kind_evt,
        "PROPERTY: reaction event must retain its EventKind (kind_evt) after storage.\n\
         Investigate: src/store/mod.rs append_reaction, src/store/segment/mod.rs write_frame.\n\
         Common causes: reaction inherits cause kind instead of its own, serialisation bug.\n\
         Run: cargo test --test store_advanced append_reaction_links_causation"
    );

    store.close().expect("close");
}

// --- CAS failure ---

#[test]
fn cas_fails_on_wrong_sequence() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:cas-fail", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    store
        .append(&coord, kind, &serde_json::json!({"x": 1}))
        .expect("first");
    store
        .append(&coord, kind, &serde_json::json!({"x": 2}))
        .expect("second");

    // CAS with stale expected_sequence (clock 0, but actual is now 1)
    let opts = batpak::store::AppendOptions {
        expected_sequence: Some(0),
        ..Default::default()
    };
    let result = store.append_with_options(&coord, kind, &serde_json::json!({"x": 3}), opts);
    let err = result.expect_err(
        "PROPERTY: append_with_options must return Err when expected_sequence is stale (CAS failure).\
         Investigate: src/store/mod.rs append_with_options CAS check.\
         Common causes: sequence comparison uses wrong field, CAS check skipped under lock."
    );
    assert!(
        matches!(err, StoreError::SequenceMismatch { .. }),
        "PROPERTY: CAS failure must surface as StoreError::SequenceMismatch, got {err:?}"
    );

    store.close().expect("close");
}

// --- Idempotency ---

#[test]
fn idempotency_returns_same_receipt() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:idemp", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let key: u128 = 0xDEAD_BEEF_CAFE_BABE_1234_5678_9ABC_DEF0;
    let opts = batpak::store::AppendOptions {
        idempotency_key: Some(key),
        ..Default::default()
    };

    let r1 = store
        .append_with_options(&coord, kind, &serde_json::json!({"x": 1}), opts.clone())
        .expect("first append");

    // Second append with same key should return same receipt
    let r2 = store
        .append_with_options(&coord, kind, &serde_json::json!({"x": 2}), opts)
        .expect("idempotent append");

    assert_eq!(
        r1.event_id, r2.event_id,
        "PROPERTY: append_with_options with the same idempotency_key must return the same event_id.\n\
         Investigate: src/store/mod.rs append_with_options idempotency check.\n\
         Common causes: idempotency key not stored after first write, key lookup hash collision.\n\
         Run: cargo test --test store_advanced idempotency_returns_same_receipt"
    );

    // Only 1 event should exist
    let stats: StoreStats = store.stats();
    assert_eq!(
        stats.event_count, 2,
        "PROPERTY: idempotent appends must not increase event_count beyond the lifecycle event plus one stored user event.\n\
         Investigate: src/store/mod.rs append_with_options idempotency check.\n\
         Common causes: idempotency key lookup misses in-memory cache, duplicate written to segment.\n\
         Run: cargo test --test store_advanced idempotency_returns_same_receipt"
    );

    store.close().expect("close");
}

// --- Subscription (push-based) ---

// --- apply_transition: typestate through the store ---

batpak::define_state_machine!(document_state_seal, DocumentState { Draft, Published });

#[test]
fn apply_transition_persists_event() {
    let (store, _dir) = test_store();
    let coord = Coordinate::new("entity:transition", "scope:test").expect("valid coord");

    // Simulate: Draft -> Published transition with a payload. FREEZE-7:
    // `Transition::from_payload` derives the event kind from `P::KIND`, so
    // the kind tested here comes from `PublishedDoc` rather than a separate
    // argument.
    let kind = <PublishedDoc as batpak::EventPayload>::KIND;
    let transition = Transition::<Draft, Published, PublishedDoc>::from_payload(PublishedDoc {
        title: "hello".into(),
        from: "draft".into(),
        to: "published".into(),
    });

    let receipt = store
        .apply_transition(&coord, transition)
        .expect("apply_transition");

    // Verify: event persisted and retrievable
    let stored = store.get(receipt.event_id).expect("get transition event");
    assert_eq!(
        stored.event.event_kind(),
        kind,
        "PROPERTY: apply_transition must persist the EventKind carried by the Transition.\n\
         Investigate: src/store/mod.rs apply_transition, src/typestate/mod.rs Transition.\n\
         Common causes: transition payload serialised without kind, wrong kind written to frame.\n\
         Run: cargo test --test store_advanced apply_transition_persists_event"
    );
    assert_eq!(
        stored.coordinate, coord,
        "PROPERTY: apply_transition must persist the event under the supplied Coordinate.\n\
         Investigate: src/store/mod.rs apply_transition.\n\
         Common causes: coordinate not forwarded to inner append call, coordinate field swapped.\n\
         Run: cargo test --test store_advanced apply_transition_persists_event"
    );

    store.close().expect("close");
}
// ===== AppendOptions builder tests: with_correlation + with_causation =====
// These pub methods were orphans — defined but never called anywhere in the
// codebase. build.rs allowlisted them with TODOs. These tests close the gap.
// PROVES: LAW-003 (No Orphan Infrastructure)
// DEFENDS: FM-007 (Island Syndrome — pub items must connect to tests)

#[test]
fn with_correlation_sets_header_correlation_id() {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 4096,
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("entity:corr", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let custom_corr: u128 = 0xDEAD_BEEF_CAFE_BABE_1234_5678_9ABC_DEF0;
    let opts = AppendOptions::new().with_correlation(custom_corr);
    let receipt = store
        .append_with_options(&coord, kind, &"corr_test", opts)
        .expect("append with correlation");

    let event = store.get(receipt.event_id).expect("get event");
    assert_eq!(
        event.event.header.correlation_id, custom_corr,
        "WITH_CORRELATION: correlation_id on stored event should match the value \
         set via AppendOptions::with_correlation().\n\
         Investigate: src/store/mod.rs append_with_options → writer.rs AppendGuards.\n\
         Common causes: correlation_id not propagated from AppendOptions to EventHeader."
    );
}

#[test]
fn with_causation_sets_header_causation_id() {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 4096,
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("entity:caus", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let custom_cause: u128 = 0x1111_2222_3333_4444_5555_6666_7777_8888;
    let opts = AppendOptions::new().with_causation(custom_cause);
    let receipt = store
        .append_with_options(&coord, kind, &"cause_test", opts)
        .expect("append with causation");

    let event = store.get(receipt.event_id).expect("get event");
    assert_eq!(
        event.event.header.causation_id,
        Some(custom_cause),
        "WITH_CAUSATION: causation_id on stored event should match the value \
         set via AppendOptions::with_causation().\n\
         Investigate: src/store/mod.rs append_with_options → writer.rs AppendGuards.\n\
         Common causes: causation_id not propagated from AppendOptions to EventHeader."
    );
}

#[test]
fn with_correlation_and_causation_combined() {
    let dir = TempDir::new().expect("create temp dir");
    let config = StoreConfig {
        data_dir: dir.path().to_path_buf(),
        segment_max_bytes: 4096,
        sync: SyncConfig {
            every_n_events: 1,
            ..SyncConfig::default()
        },
        ..StoreConfig::new("")
    };
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("entity:both", "scope:test").expect("valid coord");
    let kind = EventKind::custom(0xF, 1);

    let corr: u128 = 0xAAAA_BBBB_CCCC_DDDD_EEEE_FFFF_0000_1111;
    let cause: u128 = 0x2222_3333_4444_5555_6666_7777_8888_9999;
    let opts = AppendOptions::new()
        .with_correlation(corr)
        .with_causation(cause);
    let receipt = store
        .append_with_options(&coord, kind, &"both_test", opts)
        .expect("append with both");

    let event = store.get(receipt.event_id).expect("get event");
    assert_eq!(
        event.event.header.correlation_id, corr,
        "COMBINED: correlation_id should be set when both with_correlation and with_causation used."
    );
    assert_eq!(
        event.event.header.causation_id,
        Some(cause),
        "COMBINED: causation_id should be set when both with_correlation and with_causation used."
    );

    // Variance: default append should NOT have our custom IDs
    let default_receipt = store
        .append(&coord, kind, &"default_test")
        .expect("default append");
    let default_event = store.get(default_receipt.event_id).expect("get default");
    assert_ne!(
        default_event.event.header.correlation_id, corr,
        "VARIANCE: default append should auto-generate a different correlation_id."
    );
    assert_eq!(
        default_event.event.header.causation_id, None,
        "VARIANCE: default append should have None causation_id."
    );
}
