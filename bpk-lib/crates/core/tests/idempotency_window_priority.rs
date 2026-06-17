// justifies: INV-TEST-PANIC-AS-ASSERTION, INV-IDEMPOTENCY-DURABLE-WINDOW; integration tests rely on expect/panic and cast tiny synthetic counters/keys to u64 where truncation is impossible; these are standard harness allowances.
#![allow(clippy::unwrap_used, clippy::panic, clippy::cast_possible_truncation)]
//! Window-priority hybrid growth bound (Phase 3, 0.8.3).
//!
//! PROVES: INV-IDEMPOTENCY-DURABLE-WINDOW. The window is the inviolable
//! correctness guarantee: a retry of a key that is WITHIN the window is ALWAYS
//! a no-op regardless of load — the soft `max_keys` cap can never evict a
//! within-window key. Out-of-window keys ARE trimmed (by window-aging and the
//! cap). When within-window keys ALONE exceed `max_keys` (a key-rate spike),
//! the window wins: the store temporarily exceeds `max_keys` and the answer
//! stays correct.
//! CATCHES: a cap that crosses into within-window territory and makes a recent
//! keyed retry re-append (the SQLite property violation).
//! SEEDED: fixed EventKind, stable coordinate, deterministic per-iteration keys.

use batpak::coordinate::Coordinate;
use batpak::event::EventKind;
use batpak::id::IdempotencyKey;
use batpak::store::{
    AppendOptions, IdempotencyRetention, OverflowPolicy, Store, StoreConfig, StoreError,
};
use tempfile::TempDir;

const KIND: EventKind = EventKind::custom(0xB, 2);

fn coord() -> Coordinate {
    Coordinate::new("entity:win", "scope:priority").expect("valid coord")
}

fn append_keyed(store: &Store, key: u128) -> batpak::store::AppendReceipt {
    store
        .append_with_options(
            &coord(),
            KIND,
            &serde_json::json!({"k": key as u64}),
            AppendOptions::new().with_idempotency(IdempotencyKey::from(key)),
        )
        .expect("keyed append")
}

#[test]
fn within_window_retry_is_always_noop_even_when_cap_is_exceeded() {
    // Small window + SMALLER cap. Drive a burst that makes within-window keys
    // exceed the cap. Every within-window key must still no-op.
    let dir = TempDir::new().expect("tempdir");
    let keep_sequences = 64u64;
    let max_keys = 8u64;
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_segment_max_bytes(512)
            .with_sync_every_n_events(1)
            .with_idempotency_retention(IdempotencyRetention::Hybrid {
                keep_sequences,
                max_keys,
            })
            .with_idempotency_overflow(OverflowPolicy::Warn),
    )
    .expect("open");

    // Burst: append `burst` distinct keys within a tight sequence span so they
    // are all within the window. burst >> max_keys forces the residual
    // pigeonhole (within-window keys alone exceed the cap).
    let burst = 40usize;
    let mut originals = Vec::with_capacity(burst);
    for i in 0..burst {
        let key = 0xA000_0000_0000_0000_0000_0000_0000_0000u128 + i as u128;
        originals.push((key, append_keyed(&store, key)));
    }

    // The store should have exceeded the soft cap (window wins on correctness).
    assert!(
        store.durable_idempotency_key_count() as u64 > max_keys,
        "within-window key-rate spike makes the store exceed max_keys (window wins): count={}, max_keys={}",
        store.durable_idempotency_key_count(),
        max_keys
    );

    // EVERY within-window key must still be a no-op (returns its original
    // receipt). The cap did NOT evict any within-window key.
    for (key, original) in &originals {
        let replay = append_keyed(&store, *key);
        assert_eq!(
            replay.sequence, original.sequence,
            "INV-IDEMPOTENCY-DURABLE-WINDOW: within-window key {key:#x} must always no-op regardless of load"
        );
        assert_eq!(
            u128::from(replay.event_id),
            u128::from(original.event_id),
            "within-window retry returns original event id"
        );
    }

    store.close().expect("close");
}

#[test]
fn out_of_window_keys_are_trimmed_across_compaction() {
    // Window-aging: keys older than the window are trimmed (the cap region is a
    // pure free win). We drive the frontier well past a small window, then
    // compact (which applies eviction at the tail) and assert the early keys
    // are gone while a recent within-window key still no-ops.
    let dir = TempDir::new().expect("tempdir");
    let keep_sequences = 4u64;
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_segment_max_bytes(512)
            .with_sync_every_n_events(1)
            .with_idempotency_retention(IdempotencyRetention::Window { keep_sequences })
            .with_idempotency_overflow(OverflowPolicy::Warn),
    )
    .expect("open");

    // Old key recorded near the start of the sequence timeline.
    let old_key = 0xDEAD_0000_0000_0000_0000_0000_0000_0001u128;
    append_keyed(&store, old_key);

    // Drive the frontier far past the window with many fresh keys.
    for i in 0..40u128 {
        let key = 0xBEEF_0000_0000_0000_0000_0000_0000_0000u128 + i;
        append_keyed(&store, key);
    }

    let recent_key = 0xCAFE_0000_0000_0000_0000_0000_0000_0001u128;
    let recent = append_keyed(&store, recent_key);

    let count_before = store.durable_idempotency_key_count();

    // Compaction applies window-priority eviction at the tail.
    store
        .compact(&batpak::store::CompactionConfig {
            strategy: batpak::store::CompactionStrategy::Merge,
            min_segments: 1,
        })
        .expect("compact");

    let count_after = store.durable_idempotency_key_count();
    assert!(
        count_after < count_before,
        "window-aging trimmed out-of-window keys at compaction: before={count_before}, after={count_after}"
    );

    // The recent within-window key still no-ops.
    let replay = append_keyed(&store, recent_key);
    assert_eq!(
        replay.sequence, recent.sequence,
        "INV-IDEMPOTENCY-DURABLE-WINDOW: recent within-window key remains a no-op after aging"
    );

    store.close().expect("close");
}

#[test]
fn fail_closed_overflow_refuses_new_key_but_keeps_existing_noops() {
    // With FailClosed, a GENUINELY NEW key over the cap is refused — but
    // already-recorded keys (which are within-window) still no-op. Correctness
    // over disk.
    let dir = TempDir::new().expect("tempdir");
    // Generous-enough cap to clear the keyed lifecycle (open) event, then fill
    // up to the cap with our own keys.
    let max_keys = 16u64;
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_segment_max_bytes(512)
            .with_sync_every_n_events(1)
            .with_idempotency_retention(IdempotencyRetention::Hybrid {
                keep_sequences: 1_000_000,
                max_keys,
            })
            .with_idempotency_overflow(OverflowPolicy::FailClosed),
    )
    .expect("open");

    // Fill exactly up to the cap with our own keys (accounting for any keyed
    // lifecycle events already recorded at open).
    let mut originals = Vec::new();
    let mut next = 0x5000_0000_0000_0000_0000_0000_0000_0000u128;
    while (store.durable_idempotency_key_count() as u64) < max_keys {
        let key = next;
        next += 1;
        originals.push((key, append_keyed(&store, key)));
    }
    assert_eq!(store.durable_idempotency_key_count() as u64, max_keys);
    assert!(
        !originals.is_empty(),
        "recorded at least one of our own keys"
    );

    // A new key over the cap is refused.
    let new_key = 0x6000_0000_0000_0000_0000_0000_0000_0001u128;
    let err = store
        .append_with_options(
            &coord(),
            KIND,
            &serde_json::json!({"over": true}),
            AppendOptions::new().with_idempotency(IdempotencyKey::from(new_key)),
        )
        .expect_err("new key over cap must be refused under FailClosed");
    assert!(
        matches!(err, StoreError::IdempotencyOverflowFailClosed { .. }),
        "FailClosed refuses the new keyed append: {err:?}"
    );

    // Existing within-window keys still no-op (never evicted).
    for (key, original) in &originals {
        let replay = append_keyed(&store, *key);
        assert_eq!(
            replay.sequence, original.sequence,
            "existing within-window key stays a no-op even under FailClosed cap"
        );
    }

    store.close().expect("close");
}
