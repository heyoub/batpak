//! Stage B public-surface coverage for the durable crypto-shred keyset: the
//! `KeyStore::flush` / `KeyStore::load` round-trip and the `Store::open`
//! cold-start rehydration hook (with its fail-closed refusal).
//!
//! Gated behind `payload-encryption` (the whole file compiles out of a default
//! build). The fault-injecting crash-safety proof lives in-crate
//! (`store::keyscope::persist::crash_tests`) because it needs the
//! `dangerous-test-hooks` `SimFs`; everything here is exercised through the
//! public API only.
#![cfg(feature = "payload-encryption")]

use batpak::coordinate::Coordinate;
use batpak::event::EventKind;
use batpak::id::EventId;
use batpak::store::{
    scope_for, KeyScope, KeyScopeGranularity, KeyStore, Store, StoreConfig, StoreError,
};

/// Name of the on-disk keyset artifact (kept in sync with
/// `store::file_classification::KEYSET_FILENAME`; not public API, so pinned here).
const KEYSET_FILENAME: &str = "keyset.fbatk";
const GRAN: KeyScopeGranularity = KeyScopeGranularity::PerEntity;

fn scope(entity: &str) -> KeyScope {
    let coord = Coordinate::new(entity, "scope:persist-it").expect("coordinate");
    scope_for(
        GRAN,
        &coord,
        EventKind::custom(0xF, 1),
        EventId::from(1u128),
    )
}

#[test]
fn keystore_flush_then_load_round_trips_through_the_public_api() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let target = scope("entity:one");

    let mut store = KeyStore::new(GRAN);
    let ciphertext = store
        .get_or_create(&target)
        .expect("mint")
        .seal(&[0x7; 24], b"aad", b"public round-trip")
        .expect("seal");
    let _ = store.get_or_create(&scope("entity:two")).expect("mint 2");
    store.flush(dir.path()).expect("flush keyset");

    let reloaded = KeyStore::load(dir.path(), GRAN).expect("load keyset");
    assert_eq!(reloaded.key_count(), 2, "both keys recover");
    assert_eq!(
        reloaded
            .get(&target)
            .expect("target key recovered")
            .open(&[0x7; 24], b"aad", &ciphertext)
            .expect("reloaded key opens the pre-flush ciphertext")
            .as_slice(),
        b"public round-trip",
    );
}

#[test]
fn open_with_encryption_cold_start_loads_a_flushed_keyset() {
    let dir = tempfile::tempdir().expect("tmpdir");

    // Flush a keyset with two keys directly, then open the store with encryption
    // and prove the cold-start hook rehydrated those keys.
    let mut key_store = KeyStore::new(GRAN);
    let _ = key_store.get_or_create(&scope("entity:a")).expect("mint a");
    let _ = key_store.get_or_create(&scope("entity:b")).expect("mint b");
    key_store.flush(dir.path()).expect("flush keyset");

    let config = StoreConfig::new(dir.path()).with_payload_encryption(GRAN);
    let store = Store::open(config).expect("open encrypted store over a flushed keyset");
    let loaded = store.payload_key_count();
    // The two flushed keys are rehydrated (the Stage B cold-start property). Stage
    // D's system-events plaintext carve-out means the SYSTEM_OPEN_COMPLETED
    // lifecycle event a mutable open appends is NOT encrypted, so it mints NO key:
    // the live keyset holds exactly the two rehydrated keys.
    assert_eq!(
        loaded,
        Some(2),
        "PROPERTY: Store::open cold-start rehydrated the two flushed keys, and the \
         plaintext SYSTEM_OPEN_COMPLETED lifecycle append mints no key"
    );
    store.close().expect("close");
}

#[test]
fn open_with_encryption_on_a_fresh_dir_loads_an_empty_keyset() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let config = StoreConfig::new(dir.path()).with_payload_encryption(GRAN);
    // First open of an encrypted store: no keyset file yet, so cold-start loads an
    // EMPTY key store and the open SUCCEEDS. Under Stage D's system-events plaintext
    // carve-out the SYSTEM_OPEN_COMPLETED lifecycle event a mutable open appends is
    // NOT encrypted, so it mints NO key — the live keyset stays empty until the
    // first USER append.
    let store = Store::open(config).expect("fresh encrypted store opens with an empty keyset");
    let loaded = store.payload_key_count();
    assert_eq!(
        loaded,
        Some(0),
        "empty keyset loaded; the plaintext SYSTEM_OPEN_COMPLETED lifecycle append mints no key"
    );
    store.close().expect("close");
}

#[test]
fn open_with_encryption_fails_closed_on_a_corrupt_keyset() {
    let dir = tempfile::tempdir().expect("tmpdir");
    // Forge a garbage keyset artifact in the store dir before opening.
    std::fs::write(
        dir.path().join(KEYSET_FILENAME),
        b"this is not a valid keyset file",
    )
    .expect("write corrupt keyset");

    let config = StoreConfig::new(dir.path()).with_payload_encryption(GRAN);
    // `.err()` drops the (non-Debug) `Store` on the impossible Ok path, keeping
    // the Debug-printable error for the assertion message.
    let opened = Store::open(config).err();
    assert!(
        matches!(opened, Some(StoreError::KeysetCorrupt { .. })),
        "PROPERTY: a corrupt keyset must fail the open closed (KeysetCorrupt), never start empty; got {opened:?}"
    );
}

#[test]
fn open_without_encryption_ignores_the_keyset_file_entirely() {
    let dir = tempfile::tempdir().expect("tmpdir");
    // A garbage keyset file is present, but the store is opened WITHOUT
    // `with_payload_encryption`, so the cold-start hook does nothing and never
    // reads it.
    std::fs::write(
        dir.path().join(KEYSET_FILENAME),
        b"this is not a valid keyset file",
    )
    .expect("write corrupt keyset");

    let config = StoreConfig::new(dir.path());
    assert_eq!(config.payload_encryption(), None, "encryption is opt-in");
    let store =
        Store::open(config).expect("without encryption the keyset file is ignored, open succeeds");
    let loaded = store.payload_key_count();
    assert_eq!(loaded, None, "no encryption configured → no keyset held");
    store.close().expect("close");
}
