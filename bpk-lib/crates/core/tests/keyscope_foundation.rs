//! Stage A foundation coverage for the crypto-shred KeyScope / KeyStore surface.
//!
//! Gated behind `payload-encryption`: the whole module is compiled out of a
//! default build. It exercises the public key-store API end to end — scope
//! derivation, lazy minting, seal/open round-trips, wrong-key rejection, and the
//! destroy (crypto-shred) primitive — through the re-exported surface.
#![cfg(feature = "payload-encryption")]

use batpak::coordinate::Coordinate;
use batpak::event::EventKind;
use batpak::id::EventId;
use batpak::store::{
    scope_for, KeyScope, KeyScopeGranularity, KeyStore, KeyStoreError, PayloadKey, StoreConfig,
};

fn coord(entity: &str) -> Coordinate {
    Coordinate::new(entity, "scope:test").expect("coordinate")
}

#[test]
fn store_config_payload_encryption_is_opt_in() {
    // Default: disabled (today's plaintext-payload behavior).
    let plain = StoreConfig::new(".");
    assert_eq!(plain.payload_encryption(), None, "disabled by default");

    // Opt in and read the configured granularity back.
    let encrypted = StoreConfig::new(".").with_payload_encryption(KeyScopeGranularity::PerTypeId);
    assert_eq!(
        encrypted.payload_encryption(),
        Some(KeyScopeGranularity::PerTypeId),
        "with_payload_encryption records the granularity",
    );

    // Debug must not leak key material — the config holds only the granularity.
    let rendered = format!("{encrypted:?}");
    assert!(
        rendered.contains("payload_encryption"),
        "debug surfaces the setting: {rendered}"
    );
}

#[test]
fn scope_for_is_deterministic_and_granularity_sensitive() {
    let kind = EventKind::custom(0xF, 1);
    let id = EventId::from(3u128);

    // Deterministic: identical inputs yield identical, comparable scopes.
    let scope: KeyScope = scope_for(KeyScopeGranularity::PerEntity, &coord("a"), kind, id);
    let again = scope_for(KeyScopeGranularity::PerEntity, &coord("a"), kind, id);
    assert_eq!(scope, again, "scope derivation must be deterministic");

    // Distinct where it should be: a different entity under PerEntity splits.
    let other = scope_for(KeyScopeGranularity::PerEntity, &coord("b"), kind, id);
    assert_ne!(scope, other, "distinct entities must not share a scope");

    // Different granularities never collide.
    let per_event = scope_for(KeyScopeGranularity::PerEvent, &coord("a"), kind, id);
    assert_ne!(scope, per_event, "granularities must not collide");
}

#[test]
fn key_store_mints_seals_and_opens_through_the_public_api() {
    let mut store = KeyStore::new(KeyScopeGranularity::PerEntity);
    assert_eq!(
        store.granularity(),
        KeyScopeGranularity::PerEntity,
        "granularity accessor reflects construction"
    );

    let scope = scope_for(
        KeyScopeGranularity::PerEntity,
        &coord("entity:round-trip"),
        EventKind::custom(0xF, 1),
        EventId::from(1u128),
    );
    let nonce = [0x2Au8; 24];
    let aad = b"header-bytes";
    let plaintext = b"confidential payload";

    // Mint on first use, seal.
    let ciphertext = {
        let key = store.get_or_create(&scope).expect("mint key");
        key.seal(&nonce, aad, plaintext).expect("seal")
    };
    assert_ne!(
        ciphertext.as_slice(),
        plaintext,
        "payload must be encrypted"
    );

    // A present key is retrievable without minting.
    let fetched: Option<&PayloadKey> = store.get(&scope);
    assert!(fetched.is_some(), "get returns the minted key");

    // Second get_or_create returns the SAME key: open recovers the plaintext.
    let recovered = {
        let key = store.get_or_create(&scope).expect("reuse key");
        key.open(&nonce, aad, &ciphertext).expect("open")
    };
    assert_eq!(
        recovered.as_slice(),
        plaintext,
        "round-trip recovers plaintext"
    );
}

#[test]
fn open_rejects_wrong_key_nonce_and_aad() {
    let mut store = KeyStore::new(KeyScopeGranularity::PerEvent);
    let kind = EventKind::custom(0xF, 1);
    let scope_a = scope_for(
        KeyScopeGranularity::PerEvent,
        &coord("a"),
        kind,
        EventId::from(1u128),
    );
    let scope_b = scope_for(
        KeyScopeGranularity::PerEvent,
        &coord("a"),
        kind,
        EventId::from(2u128),
    );
    let nonce = [0x11u8; 24];
    let aad = b"aad";

    let ciphertext = store
        .get_or_create(&scope_a)
        .expect("mint a")
        .seal(&nonce, aad, b"payload")
        .expect("seal");

    // Wrong key (a different scope's key) fails without panicking.
    let wrong_key_result: Result<Vec<u8>, KeyStoreError> = store
        .get_or_create(&scope_b)
        .expect("mint b")
        .open(&nonce, aad, &ciphertext);
    assert!(wrong_key_result.is_err(), "wrong key must fail");

    // Wrong nonce and wrong aad also fail (re-borrow scope_a's key each time).
    let wrong_nonce = [0x22u8; 24];
    assert!(
        store
            .get_or_create(&scope_a)
            .expect("key a")
            .open(&wrong_nonce, aad, &ciphertext)
            .is_err(),
        "wrong nonce must fail"
    );
    assert!(
        store
            .get_or_create(&scope_a)
            .expect("key a")
            .open(&nonce, b"other-aad", &ciphertext)
            .is_err(),
        "wrong aad must fail"
    );
}

#[test]
fn destroy_shreds_the_key_and_prior_ciphertext() {
    let mut store = KeyStore::new(KeyScopeGranularity::PerEntity);
    let scope = scope_for(
        KeyScopeGranularity::PerEntity,
        &coord("entity:shred"),
        EventKind::custom(0xF, 1),
        EventId::from(1u128),
    );
    let nonce = [0x77u8; 24];

    let ciphertext = store
        .get_or_create(&scope)
        .expect("mint")
        .seal(&nonce, b"", b"shred me")
        .expect("seal");

    assert!(store.destroy(&scope), "destroy removes an existing key");
    assert!(store.get(&scope).is_none(), "get after destroy is None");
    assert!(
        !store.destroy(&scope),
        "destroying an absent scope returns false"
    );

    // A fresh key minted for the same scope cannot recover the old payload.
    let reopened = store
        .get_or_create(&scope)
        .expect("re-mint")
        .open(&nonce, b"", &ciphertext);
    assert!(
        reopened.is_err(),
        "post-shred key must not open old ciphertext"
    );
}
