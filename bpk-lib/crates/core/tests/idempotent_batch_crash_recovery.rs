// justifies: INV-TEST-PANIC-AS-ASSERTION, INV-BATCH-CRASH-RECOVERY; tests in tests/idempotent_batch_crash_recovery.rs rely on expect/panic on unreachable failures; clippy::unwrap_used and clippy::panic are the standard harness allowances for integration tests.
#![allow(clippy::unwrap_used, clippy::panic)]
//! Idempotent batch shape + replay recovery.
//!
//! PROVES: fully keyed batch is replayable after close+reopen without duplicate events;
//! heterogeneous keyed/unkeyed batches fail fast with `StoreError::IdempotencyPartialBatch`.
//! CATCHES: silent acceptance of partial idempotency-key replay in batch preflight; off-by-one
//! enforcement of `batch.max_bytes` at the writer boundary.
//! SEEDED: fixed `EventKind`, stable coordinates, explicit u128 idempotency keys, `tempfile` roots.
//!
//! [INV-GROUP-COMMIT-IDEMPOTENCY] A batch that mixes keyed and unkeyed items is
//! rejected synchronously with `StoreError::IdempotencyPartialBatch` before
//! any frame reaches disk. A fully-keyed batch survives close+reopen and is
//! replayable without producing duplicate events.

use batpak::coordinate::{Coordinate, Region};
use batpak::event::EventKind;
use batpak::store::{AppendOptions, BatchAppendItem, CausationRef, Store, StoreConfig, StoreError};
use tempfile::TempDir;

const KIND: EventKind = EventKind::custom(0xB, 1);

fn coord() -> Coordinate {
    Coordinate::new("entity:idem", "scope:batch").expect("valid coord")
}

fn config(dir: &TempDir) -> StoreConfig {
    StoreConfig::new(dir.path())
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false)
        .with_sync_every_n_events(1)
}

fn user_visible_events(store: &Store) -> Vec<batpak::store::index::IndexEntry> {
    store
        .query(&Region::all())
        .into_iter()
        .filter(|entry| {
            !matches!(
                entry.event_kind(),
                EventKind::SYSTEM_OPEN_COMPLETED | EventKind::SYSTEM_CLOSE_COMPLETED
            )
        })
        .collect()
}

fn byte_counted_batch(coord: &Coordinate) -> Vec<BatchAppendItem> {
    vec![
        BatchAppendItem::new(
            coord.clone(),
            KIND,
            &serde_json::json!({"part": "abc"}),
            AppendOptions::new(),
            CausationRef::None,
        )
        .expect("first byte-counted item"),
        BatchAppendItem::new(
            coord.clone(),
            KIND,
            &serde_json::json!({"part": "defg"}),
            AppendOptions::new(),
            CausationRef::None,
        )
        .expect("second byte-counted item"),
    ]
}

fn batch_payload_bytes(items: &[BatchAppendItem]) -> u32 {
    items
        .iter()
        .map(|item| item.payload_bytes().len())
        .sum::<usize>()
        .try_into()
        .expect("test payload length fits u32")
}

#[test]
fn partial_keys_rejected_synchronously() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(config(&dir)).expect("open store");
    let coord = coord();

    // Item 0: carries an idempotency key.
    // Item 1: does NOT carry one. The batch has a heterogeneous shape and
    // must be rejected before any frame hits disk.
    let items = vec![
        BatchAppendItem::new(
            coord.clone(),
            KIND,
            &serde_json::json!({"step": 0}),
            AppendOptions::new().with_idempotency(0xAAAA_BBBB_CCCC_DDDD),
            CausationRef::None,
        )
        .expect("keyed item"),
        BatchAppendItem::new(
            coord.clone(),
            KIND,
            &serde_json::json!({"step": 1}),
            AppendOptions::new(),
            CausationRef::None,
        )
        .expect("unkeyed item"),
    ];

    let result = store.append_batch(items);
    let err = match result {
        Ok(_) => panic!(
            "PROPERTY: a batch that mixes keyed and unkeyed items must be rejected synchronously"
        ),
        Err(err) => err,
    };
    assert!(
        matches!(err, StoreError::IdempotencyPartialBatch { .. }),
        "PROPERTY: partial-key batch must route to StoreError::IdempotencyPartialBatch, got {err:?}"
    );

    // Nothing must be visible to readers.
    let visible = user_visible_events(&store);
    assert!(
        visible.is_empty(),
        "PROPERTY: a rejected partial-key batch must leave the store empty; found {} visible events",
        visible.len()
    );

    store.close().expect("close store");
}

#[test]
fn batch_max_bytes_accepts_exact_limit_and_rejects_one_byte_over() {
    let coord = coord();
    let items = byte_counted_batch(&coord);
    let exact_limit = batch_payload_bytes(&items);

    let exact_dir = TempDir::new().expect("temp dir");
    let exact_store =
        Store::open(config(&exact_dir).with_batch_max_bytes(exact_limit)).expect("open exact");
    let receipts = exact_store
        .append_batch(items.clone())
        .expect("PROPERTY: batch whose payload bytes equal max_bytes must be accepted");
    assert_eq!(
        receipts.len(),
        2,
        "PROPERTY: exact-limit batch must commit every item"
    );
    exact_store.close().expect("close exact");

    let over_dir = TempDir::new().expect("temp dir");
    let over_store =
        Store::open(config(&over_dir).with_batch_max_bytes(exact_limit - 1)).expect("open over");
    let err = match over_store.append_batch(items) {
        Ok(_) => panic!("PROPERTY: batch one byte over max_bytes must be rejected"),
        Err(err) => err,
    };
    match err {
        StoreError::BatchFailed { item_index, source } => {
            assert_eq!(
                item_index, 0,
                "PROPERTY: aggregate batch byte-limit failures are reported at batch index 0"
            );
            assert!(
                matches!(*source, StoreError::Configuration(_)),
                "PROPERTY: batch byte-limit failure must preserve Configuration source"
            );
        }
        other => panic!("PROPERTY: expected StoreError::BatchFailed, got {other:?}"),
    }
    over_store.close().expect("close over");
}

#[test]
fn idempotent_batch_replayable_without_duplicates() {
    // Use close+reopen as a crash proxy: the first append_batch writes
    // frames (BEGIN + items + COMMIT) and fsyncs. Reopening rebuilds the
    // index from disk. Submitting the same idempotent batch must deduplicate
    // via the idempotency-key index and return the original receipts.
    let dir = TempDir::new().expect("temp dir");
    let coord = coord();

    // Build a fully-keyed batch — both items carry unique idempotency keys.
    fn build_batch(coord: &Coordinate) -> Vec<BatchAppendItem> {
        vec![
            BatchAppendItem::new(
                coord.clone(),
                KIND,
                &serde_json::json!({"step": 0}),
                AppendOptions::new().with_idempotency(0x1111_1111_1111_1111),
                CausationRef::None,
            )
            .expect("keyed item 0"),
            BatchAppendItem::new(
                coord.clone(),
                KIND,
                &serde_json::json!({"step": 1}),
                AppendOptions::new().with_idempotency(0x2222_2222_2222_2222),
                CausationRef::None,
            )
            .expect("keyed item 1"),
        ]
    }

    // Phase 1: first submission on a fresh store.
    let first_receipts = {
        let store = Store::open(config(&dir)).expect("open store");
        let receipts = store
            .append_batch(build_batch(&coord))
            .expect("first batch submission must succeed");
        assert_eq!(receipts.len(), 2);
        let events = user_visible_events(&store);
        assert_eq!(
            events.len(),
            2,
            "PROPERTY: first batch submission must make both items visible"
        );
        store.close().expect("close store");
        receipts
    };

    // Phase 2: reopen, resubmit the same batch. The idempotency index must
    // recognise the keys and short-circuit to the cached receipts.
    {
        let store = Store::open(config(&dir)).expect("reopen store");
        let replay_receipts = store
            .append_batch(build_batch(&coord))
            .expect("replay submission must succeed");
        assert_eq!(
            replay_receipts.len(),
            2,
            "PROPERTY: replay must return one receipt per item"
        );

        for (orig, replay) in first_receipts.iter().zip(replay_receipts.iter()) {
            assert_eq!(
                orig.event_id, replay.event_id,
                "PROPERTY: idempotent replay must return the original event_id; \
                 first={:x} replay={:x} — a fresh UUID here means dedup failed.",
                orig.event_id, replay.event_id
            );
            assert_eq!(
                orig.sequence, replay.sequence,
                "PROPERTY: idempotent replay must return the original sequence"
            );
        }

        // Only two events must be visible — no duplicates were created.
        let events = user_visible_events(&store);
        assert_eq!(
            events.len(),
            2,
            "PROPERTY: idempotent replay must not duplicate events; found {} events",
            events.len()
        );

        store.close().expect("close store");
    }
}
