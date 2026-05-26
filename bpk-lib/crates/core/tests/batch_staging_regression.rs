//! PROVES: INV-PREPARED-BATCH-STAGING-EQUIVALENCE.

mod support;
use batpak::store::{BatchAppendItem, CausationRef, Store, StoreConfig};
use support::prelude::*;
use tempfile::TempDir;

const KIND: EventKind = EventKind::custom(0xF, 0x66);
const BATCH_LEN: usize = 96;

fn test_config(dir: &TempDir) -> StoreConfig {
    StoreConfig::new(dir.path())
        .with_group_commit_max_batch(16)
        .with_sync_every_n_events(512)
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false)
}

fn coord_parts(idx: usize) -> (String, String) {
    (
        format!("entity:batch-stage:{}", idx % 6),
        format!("scope:batch-stage:{}", idx % 3),
    )
}

fn reused_coordinate_batch() -> Vec<BatchAppendItem> {
    let templates: Vec<_> = (0..6)
        .map(|idx| {
            let (entity, scope) = coord_parts(idx);
            Coordinate::new(entity, scope).expect("template coordinate")
        })
        .collect();

    (0..BATCH_LEN)
        .map(|idx| {
            BatchAppendItem::new(
                templates[idx % templates.len()].clone(),
                KIND,
                &serde_json::json!({"batch": "reused", "i": idx, "bucket": idx % 4}),
                AppendOptions::new()
                    .with_idempotency(batpak::id::IdempotencyKey::from(0xA000 + idx as u128)),
                CausationRef::None,
            )
            .expect("reused batch item")
        })
        .collect()
}

fn fresh_coordinate_batch() -> Vec<BatchAppendItem> {
    (0..BATCH_LEN)
        .map(|idx| {
            let (entity, scope) = coord_parts(idx);
            BatchAppendItem::new(
                Coordinate::new(entity, scope).expect("fresh coordinate"),
                KIND,
                &serde_json::json!({"batch": "fresh", "i": idx, "bucket": idx % 4}),
                AppendOptions::new()
                    .with_idempotency(batpak::id::IdempotencyKey::from(0xB000 + idx as u128)),
                CausationRef::None,
            )
            .expect("fresh batch item")
        })
        .collect()
}

fn duplicate_heavy_batch() -> Vec<BatchAppendItem> {
    (0usize..72)
        .map(|idx| {
            let entity = if idx % 2 == 0 {
                "entity:dup:a"
            } else {
                "entity:dup:b"
            };
            let scope = if idx % 3 == 0 {
                "scope:dup:x"
            } else {
                "scope:dup:y"
            };
            BatchAppendItem::new(
                Coordinate::new(entity, scope).expect("duplicate-heavy coordinate"),
                KIND,
                &serde_json::json!({"i": idx, "entity": entity, "scope": scope}),
                AppendOptions::new()
                    .with_idempotency(batpak::id::IdempotencyKey::from(0xC000 + idx as u128)),
                CausationRef::None,
            )
            .expect("duplicate-heavy item")
        })
        .collect()
}

fn snapshot(store: &Store) -> Vec<(String, String, u64)> {
    let mut rows: Vec<_> = store
        .by_fact(KIND)
        .into_iter()
        .map(|entry| {
            (
                entry.coord().entity().to_owned(),
                entry.coord().scope().to_owned(),
                entry.global_sequence(),
            )
        })
        .collect();
    rows.sort_by_key(|(_, _, sequence)| *sequence);
    rows
}

fn assert_contiguous_sequences(rows: &[(String, String, u64)], label: &str) {
    let sequences: Vec<_> = rows.iter().map(|(_, _, sequence)| *sequence).collect();
    assert!(
        !sequences.is_empty(),
        "PROPERTY: {label} should have at least one visible sequence"
    );
    let first_sequence = sequences[0];
    assert_eq!(
        sequences,
        (first_sequence..first_sequence + rows.len() as u64).collect::<Vec<_>>(),
        "{label} should publish contiguous visible sequences"
    );
}

#[test]
fn reused_and_fresh_coordinate_batches_have_identical_visibility_after_reopen() {
    let reused_dir = TempDir::new().expect("reused temp dir");
    let fresh_dir = TempDir::new().expect("fresh temp dir");

    let reused_store = Store::open(test_config(&reused_dir)).expect("open reused store");
    let fresh_store = Store::open(test_config(&fresh_dir)).expect("open fresh store");

    let reused_receipts = reused_store
        .append_batch(reused_coordinate_batch())
        .expect("append reused batch");
    let fresh_receipts = fresh_store
        .append_batch(fresh_coordinate_batch())
        .expect("append fresh batch");
    assert_eq!(reused_receipts.len(), BATCH_LEN);
    assert_eq!(fresh_receipts.len(), BATCH_LEN);

    reused_store.sync().expect("sync reused");
    fresh_store.sync().expect("sync fresh");

    let reused_snapshot = snapshot(&reused_store);
    let fresh_snapshot = snapshot(&fresh_store);
    assert_eq!(
        reused_snapshot.len(),
        BATCH_LEN,
        "reused-coordinate batch should surface every event before reopen"
    );
    assert_eq!(
        fresh_snapshot.len(),
        BATCH_LEN,
        "fresh-coordinate batch should surface every event before reopen"
    );

    assert_eq!(
        reused_snapshot
            .iter()
            .map(|(entity, scope, _)| (entity.clone(), scope.clone()))
            .collect::<Vec<_>>(),
        fresh_snapshot
            .iter()
            .map(|(entity, scope, _)| (entity.clone(), scope.clone()))
            .collect::<Vec<_>>(),
        "batch staging must preserve the same visible entity/scope ordering regardless of coordinate construction pattern"
    );
    assert_contiguous_sequences(&reused_snapshot, "reused-coordinate batch");
    assert_contiguous_sequences(&fresh_snapshot, "fresh-coordinate batch");

    reused_store.close().expect("close reused");
    fresh_store.close().expect("close fresh");

    let reopened_reused = Store::open(test_config(&reused_dir)).expect("reopen reused store");
    let reopened_fresh = Store::open(test_config(&fresh_dir)).expect("reopen fresh store");
    assert_eq!(
        snapshot(&reopened_reused),
        reused_snapshot,
        "reused-coordinate batch should reopen with identical visible state"
    );
    assert_eq!(
        snapshot(&reopened_fresh),
        fresh_snapshot,
        "fresh-coordinate batch should reopen with identical visible state"
    );
    assert_eq!(
        reopened_reused.by_scope("scope:batch-stage:0").len(),
        reopened_fresh.by_scope("scope:batch-stage:0").len(),
        "scope query results should match across coordinate construction patterns"
    );

    reopened_reused.close().expect("close reopened reused");
    reopened_fresh.close().expect("close reopened fresh");
}

#[test]
fn duplicate_heavy_batch_preserves_scope_and_stream_queries_after_reopen() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(test_config(&dir)).expect("open store");

    let receipts = store
        .append_batch(duplicate_heavy_batch())
        .expect("append duplicate-heavy batch");
    assert_eq!(receipts.len(), 72);

    store.sync().expect("sync duplicate-heavy batch");
    let live_snapshot = snapshot(&store);
    assert_eq!(
        live_snapshot.len(),
        72,
        "duplicate-heavy batch should surface every event"
    );
    assert_eq!(
        store.by_entity("entity:dup:a").len(),
        36,
        "entity stream should preserve duplicate-heavy batch membership"
    );
    assert_eq!(
        store.by_entity("entity:dup:b").len(),
        36,
        "entity stream should preserve duplicate-heavy batch membership"
    );
    let scope_x = live_snapshot
        .iter()
        .filter(|(_, scope, _)| scope == "scope:dup:x")
        .count();
    let scope_y = live_snapshot
        .iter()
        .filter(|(_, scope, _)| scope == "scope:dup:y")
        .count();
    assert_eq!(
        scope_x, 24,
        "live snapshot should preserve scope x membership"
    );
    assert_eq!(
        scope_y, 48,
        "live snapshot should preserve scope y membership"
    );

    assert_contiguous_sequences(&live_snapshot, "duplicate-heavy batch");

    store.close().expect("close");

    let reopened = Store::open(test_config(&dir)).expect("reopen store");
    let reopened_snapshot = snapshot(&reopened);
    assert_eq!(reopened_snapshot.len(), 72);
    assert_eq!(reopened.by_entity("entity:dup:a").len(), 36);
    assert_eq!(reopened.by_entity("entity:dup:b").len(), 36);
    assert_eq!(
        reopened_snapshot
            .iter()
            .filter(|(_, scope, _)| scope == "scope:dup:x")
            .count(),
        24
    );
    assert_eq!(
        reopened_snapshot
            .iter()
            .filter(|(_, scope, _)| scope == "scope:dup:y")
            .count(),
        48
    );
    reopened.close().expect("close reopened");
}
