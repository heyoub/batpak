//! Flush/load unit proofs for the durable crypto-shred keyset: round-trip,
//! shred-survives-restart, and fail-closed-on-corruption.
//!
//! Internal (in-crate) rather than an integration test because `flush`/`load`
//! take the `pub(crate)` `StoreFs` seam. The crash-safety proof lives beside this
//! in `crash_tests.rs` (it needs the `dangerous-test-hooks` `SimFs`).

use super::{corrupt, KEYSET_MAGIC, KEYSET_VERSION};
use crate::coordinate::Coordinate;
use crate::event::EventKind;
use crate::id::EventId;
use crate::store::file_classification::KEYSET_FILENAME;
use crate::store::keyscope::{scope_for, KeyScope, KeyScopeGranularity, KeyStore};
// Route the corrupt-file forging through the platform seam (the structural
// direct-fs-contact ratchet forbids raw `std::fs` in store `src/`).
use crate::store::platform::fs::{read as fs_read, write_derivative_file_atomically};
use crate::store::StoreError;

const GRAN: KeyScopeGranularity = KeyScopeGranularity::PerEntity;
const NONCE: [u8; 24] = [0x33; 24];

fn scope(entity: &str) -> KeyScope {
    let coord = Coordinate::new(entity, "scope:persist").expect("coordinate");
    scope_for(
        GRAN,
        &coord,
        EventKind::custom(0xF, 1),
        EventId::from(1u128),
    )
}

#[test]
fn flush_then_load_recovers_keys_and_a_pre_flush_ciphertext_opens() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let scope_a = scope("entity:a");
    let scope_b = scope("entity:b");

    let mut store = KeyStore::new(GRAN);
    let ciphertext = store
        .get_or_create(&scope_a)
        .expect("mint A")
        .seal(&NONCE, b"aad", b"secret payload")
        .expect("seal under A");
    let _ = store.get_or_create(&scope_b).expect("mint B");
    store.flush(dir.path()).expect("flush keyset");

    // Reload from the same dir: both keys recover, and the ciphertext sealed
    // BEFORE the flush opens under the reloaded key A.
    let reloaded = KeyStore::load(dir.path(), GRAN).expect("load keyset");
    assert_eq!(reloaded.granularity(), GRAN, "granularity round-trips");
    assert!(reloaded.get(&scope_b).is_some(), "key B recovered");
    let recovered = reloaded
        .get(&scope_a)
        .expect("key A recovered")
        .open(&NONCE, b"aad", &ciphertext)
        .expect("reloaded key A opens the pre-flush ciphertext");
    assert_eq!(recovered.as_slice(), b"secret payload");
}

#[test]
fn absent_keyset_loads_an_empty_store_not_a_corruption() {
    let dir = tempfile::tempdir().expect("tmpdir");
    // First open: no keyset file yet → empty store, NOT an error.
    let loaded = KeyStore::load(dir.path(), GRAN).expect("absent keyset → empty store");
    assert!(
        loaded.get(&scope("entity:none")).is_none(),
        "an absent keyset rehydrates empty"
    );
}

#[test]
fn shred_survives_restart_destroyed_key_is_absent_and_ciphertext_unrecoverable() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let target = scope("entity:shred-me");
    let keep = scope("entity:keep");

    // Seal a ciphertext under the target scope, and mint a second key we keep.
    let mut store = KeyStore::new(GRAN);
    let ciphertext = store
        .get_or_create(&target)
        .expect("mint target")
        .seal(&NONCE, b"aad", b"data to shred")
        .expect("seal target");
    let _ = store.get_or_create(&keep).expect("mint keep");
    store.flush(dir.path()).expect("flush v1");

    // Crypto-shred: destroy the target key, then flush the shredded keyset.
    assert!(store.destroy(&target), "destroy removes the target key");
    store.flush(dir.path()).expect("flush shredded keyset");

    // Restart: reload from disk. The destroyed scope's key is ABSENT and its old
    // ciphertext is unrecoverable — the shred is durable across the restart. The
    // untouched key survives.
    let reloaded = KeyStore::load(dir.path(), GRAN).expect("reload after shred");
    assert!(
        reloaded.get(&target).is_none(),
        "PROPERTY: the shredded scope's key is absent after restart"
    );
    assert!(
        reloaded.get(&keep).is_some(),
        "PROPERTY: an untouched key survives the shred flush"
    );
    // Even minting a fresh key for the same scope cannot open the old ciphertext.
    let mut reloaded = reloaded;
    let refreshed = reloaded.get_or_create(&target).expect("re-mint target");
    assert!(
        matches!(
            refreshed.open(&NONCE, b"aad", &ciphertext),
            Err(crate::store::KeyStoreError::Open)
        ),
        "PROPERTY: post-shred ciphertext is permanently unrecoverable across restart"
    );
}

/// Write a raw keyset file with the given bytes as its whole content, through
/// the platform seam (keeps the direct-fs-contact ratchet at zero for this file).
fn write_raw_keyset(dir: &std::path::Path, bytes: &[u8]) {
    write_derivative_file_atomically(dir, &dir.join(KEYSET_FILENAME), "test-forge-keyset", bytes)
        .expect("write raw keyset");
}

#[test]
fn corrupt_keyset_fails_closed_rather_than_starting_empty() {
    // (1) Garbage bytes (wrong magic) → hard error, NOT an empty store.
    let garbage_dir = tempfile::tempdir().expect("tmpdir");
    write_raw_keyset(garbage_dir.path(), b"not a real keyset file at all");
    assert!(
        matches!(
            KeyStore::load(garbage_dir.path(), GRAN),
            Err(StoreError::KeysetCorrupt { .. })
        ),
        "PROPERTY: a garbage keyset fails closed (KeysetCorrupt), never silently empty"
    );

    // (2) A truncated header (shorter than magic+version+crc) → fail closed.
    let short_dir = tempfile::tempdir().expect("tmpdir");
    write_raw_keyset(short_dir.path(), &KEYSET_MAGIC[..3]);
    assert!(
        matches!(
            KeyStore::load(short_dir.path(), GRAN),
            Err(StoreError::KeysetCorrupt { .. })
        ),
        "PROPERTY: a truncated keyset header fails closed"
    );

    // (3) A valid file body with a corrupted CRC → fail closed. Flush a real
    // keyset, then flip a byte inside the CRC field.
    let crc_dir = tempfile::tempdir().expect("tmpdir");
    let mut store = KeyStore::new(GRAN);
    let _ = store.get_or_create(&scope("entity:crc")).expect("mint");
    store.flush(crc_dir.path()).expect("flush");
    let path = crc_dir.path().join(KEYSET_FILENAME);
    let mut raw = fs_read(&path).expect("read flushed keyset");
    // CRC lives at bytes 8..12 (after magic(6)+version(2)); corrupt it.
    raw[8] ^= 0xFF;
    write_derivative_file_atomically(crc_dir.path(), &path, "test-corrupt-crc", &raw)
        .expect("rewrite with corrupted crc");
    assert!(
        matches!(
            KeyStore::load(crc_dir.path(), GRAN),
            Err(StoreError::KeysetCorrupt { .. })
        ),
        "PROPERTY: a CRC mismatch fails closed"
    );

    // (4) A granularity mismatch (persisted under PerEntity, loaded as PerEvent)
    // → fail closed: every derived scope would differ, an effective silent shred.
    let gran_dir = tempfile::tempdir().expect("tmpdir");
    let mut store = KeyStore::new(GRAN);
    let _ = store.get_or_create(&scope("entity:gran")).expect("mint");
    store.flush(gran_dir.path()).expect("flush");
    assert!(
        matches!(
            KeyStore::load(gran_dir.path(), KeyScopeGranularity::PerEvent),
            Err(StoreError::KeysetCorrupt { .. })
        ),
        "PROPERTY: a configured-vs-persisted granularity mismatch fails closed"
    );
}

#[test]
fn corrupt_helper_and_header_constants_are_stable() {
    // Format identity guard: magic + current version must not drift silently.
    assert_eq!(KEYSET_MAGIC, b"FBATKS");
    assert_eq!(KEYSET_VERSION, 1);
    assert!(matches!(
        corrupt("x".to_owned()),
        StoreError::KeysetCorrupt { .. }
    ));
    // Exercise the fail-closed Display render (guards `fmt_keyset_corrupt`): it
    // must carry the reason and the fail-closed posture, never key material.
    let rendered = corrupt("boom-reason".to_owned()).to_string();
    assert!(
        rendered.contains("boom-reason") && rendered.contains("refusing to open"),
        "KeysetCorrupt Display must carry the reason and the fail-closed posture: {rendered}"
    );
}
