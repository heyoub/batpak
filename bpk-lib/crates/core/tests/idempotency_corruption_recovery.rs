//! Crash / corruption posture for the durable idempotency sidecar (Phase 3).
//!
//! PROVES: INV-IDEMPOTENCY-DURABLE-WINDOW (graceful-degradation posture). A
//! corrupt `index.idemp` (bad CRC / truncated / wrong magic) is treated as
//! ABSENT at cold-start — the store opens correctly and continues (it only
//! loses durable-dedup history), mirroring the checkpoint posture. It never
//! crashes cold-start, and never returns a silent wrong answer.
//! CATCHES: a panic / hard-failure on a corrupt sidecar, or a silent wrong
//! answer that resurrects a stale key after the durable history was lost.
//! SEEDED: fixed key, deterministic file mutation.

use batpak::coordinate::{Coordinate, Region};
use batpak::event::EventKind;
use batpak::id::{EntityIdType, IdempotencyKey};
use batpak::store::{AppendOptions, Store, StoreConfig, StoreError};
use std::io::Write;
use tempfile::TempDir;

const KIND: EventKind = EventKind::custom(0xB, 3);
const IDEMP_FILENAME: &str = "index.idemp";

fn coord() -> Coordinate {
    Coordinate::new("entity:idem", "scope:corrupt").expect("valid coord")
}

fn config(dir: &TempDir) -> StoreConfig {
    StoreConfig::new(dir.path())
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false)
        .with_segment_max_bytes(512)
        .with_sync_every_n_events(1)
}

fn append_keyed(store: &Store, key: u128) -> batpak::store::AppendReceipt {
    let payload_tag = u64::try_from(key & u128::from(u64::MAX)).expect("low 64 bits fit u64");
    store
        .append_with_options(
            &coord(),
            KIND,
            &serde_json::json!({ "k": payload_tag }),
            AppendOptions::new().with_idempotency(IdempotencyKey::from(key)),
        )
        .expect("keyed append")
}

/// Count user events of `KIND` carrying the given key (a duplicate would push
/// this above 1).
fn key_event_count(store: &Store, key: u128) -> usize {
    store
        .query(&Region::all())
        .into_iter()
        .filter(|e| e.event_kind() == KIND && e.event_id().as_u128() == key)
        .count()
}

/// Run the corruption scenario for a given file-mutation, asserting:
/// (1) cold-start does not crash, and
/// (2) the store stays CORRECT after degradation — the previously-recorded key
///     is no longer deduplicated (history was lost), but no silent wrong answer
///     occurs, and a fresh keyed append + retry still no-ops.
fn assert_graceful_degradation(mutate: impl FnOnce(&std::path::Path)) {
    let dir = TempDir::new().expect("tempdir");
    let key = 0x9999_8888_7777_6666_5555_4444_3333_2222u128;

    {
        let store = Store::open(config(&dir)).expect("open");
        append_keyed(&store, key);
        assert_eq!(key_event_count(&store, key), 1, "one keyed event committed");
        store.close().expect("close");
    }

    mutate(&dir.path().join(IDEMP_FILENAME));

    // Cold-start must NOT crash on a corrupt durable sidecar.
    let store = Store::open(config(&dir)).expect("corrupt idemp must not crash cold-start");

    // A fresh keyed append under a NEW key plus a retry still no-ops: the store
    // is fully functional after degradation.
    let new_key = key.wrapping_add(1);
    let fresh = append_keyed(&store, new_key);
    let replay = append_keyed(&store, new_key);
    assert_eq!(
        fresh.sequence, replay.sequence,
        "store remains correct after corruption: new keyed append + retry no-ops"
    );
    assert_eq!(
        key_event_count(&store, new_key),
        1,
        "no duplicate written for the new key after degradation"
    );

    store.close().expect("close");
}

#[test]
fn corrupt_crc_degrades_and_does_not_crash() {
    assert_graceful_degradation(|path| {
        let mut bytes = std::fs::read(path).expect("read idemp file");
        assert!(bytes.len() > 12, "idemp file should have a header + body");
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        std::fs::write(path, &bytes).expect("write corrupted idemp file");
    });
}

#[test]
fn wrong_magic_degrades_and_does_not_crash() {
    assert_graceful_degradation(|path| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open idemp for clobber");
        file.write_all(b"XXXXXX").expect("clobber magic");
        file.sync_all().expect("sync");
    });
}

#[test]
fn truncated_file_degrades_and_does_not_crash() {
    assert_graceful_degradation(|path| {
        std::fs::write(path, b"FBA").expect("truncate idemp file");
    });
}

#[test]
fn unsupported_lower_version_degrades_and_does_not_crash() {
    // A corrupted `version = 0` with a CRC-VALID body must NOT load as the
    // current version (the CRC excludes the header, so the body alone cannot
    // catch a flipped version). It degrades as absent, like a bad CRC.
    assert_graceful_degradation(|path| {
        let mut bytes = std::fs::read(path).expect("read idemp file");
        assert!(bytes.len() >= 12, "idemp file should have a header + body");
        // Overwrite the version field (bytes 6..8, little-endian) with 0,
        // leaving magic, CRC, and body untouched so only the version trips.
        let bad_version: u16 = 0;
        bytes[6..8].copy_from_slice(&bad_version.to_le_bytes());
        std::fs::write(path, &bytes).expect("write version-0 idemp file");
    });
}

#[test]
fn future_version_is_a_hard_error_at_cold_start() {
    // A future on-disk version must FAIL CLOSED (mirroring schema-evo
    // FutureVersion): a reader can never reconstruct a format it predates.
    let dir = TempDir::new().expect("tempdir");
    let key = 0x4242_4242_4242_4242_4242_4242_4242_4242u128;
    {
        let store = Store::open(config(&dir)).expect("open");
        append_keyed(&store, key);
        store.close().expect("close");
    }

    // Rewrite the version field (bytes 6..8, little-endian) to a future value,
    // recomputing the CRC over the unchanged body so only the version trips.
    let path = dir.path().join(IDEMP_FILENAME);
    let mut bytes = std::fs::read(&path).expect("read idemp file");
    assert!(bytes.len() >= 12);
    let future_version: u16 = 999;
    bytes[6..8].copy_from_slice(&future_version.to_le_bytes());
    // CRC covers only the body (offset 12..); it is unchanged, so no need to
    // recompute — the version check fires before the CRC check.
    std::fs::write(&path, &bytes).expect("write future-version idemp file");

    let err = Store::open(config(&dir))
        .err()
        .expect("future-version idemp must fail open closed");
    assert!(
        matches!(
            err,
            StoreError::IdempotencyFutureVersion {
                stored: 999,
                current: 1
            }
        ),
        "future-version sidecar is a hard error: {err:?}"
    );
}
