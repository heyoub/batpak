//! Unit tests for the crypto-shred KeyScope / KeyStore foundation.
//!
//! Split into a child file-module (rather than an inline `mod tests`) to stay
//! under the structural inline-test-island cap; as a child of `keyscope` it can
//! still reach the module's private `PayloadKey` constructor and `generate`.

use super::*;

fn coord(entity: &str) -> Coordinate {
    Coordinate::new(entity, "scope:test").expect("coordinate")
}

fn fixed_key(byte: u8) -> PayloadKey {
    PayloadKey(Zeroizing::new([byte; KEY_LEN]))
}

#[test]
fn scope_for_is_deterministic_per_granularity() {
    let coordinate = coord("entity:a");
    let kind = EventKind::custom(0xF, 1);
    let id = EventId::from(7u128);
    for granularity in [
        KeyScopeGranularity::PerEntity,
        KeyScopeGranularity::PerCategory,
        KeyScopeGranularity::PerTypeId,
        KeyScopeGranularity::PerEvent,
    ] {
        let first = scope_for(granularity, &coordinate, kind, id);
        let second = scope_for(granularity, &coordinate, kind, id);
        assert_eq!(first, second, "scope_for must be deterministic");
    }
}

#[test]
fn scope_for_distinguishes_by_the_relevant_field_only() {
    let kind = EventKind::custom(0xF, 1);
    let id = EventId::from(7u128);

    // PerEntity keys off the entity, not the kind/id.
    let entity_a = scope_for(KeyScopeGranularity::PerEntity, &coord("a"), kind, id);
    let entity_b = scope_for(KeyScopeGranularity::PerEntity, &coord("b"), kind, id);
    assert_ne!(
        entity_a, entity_b,
        "distinct entities must not share a scope"
    );
    let entity_a_other_kind = scope_for(
        KeyScopeGranularity::PerEntity,
        &coord("a"),
        EventKind::custom(0xE, 2),
        EventId::from(99u128),
    );
    assert_eq!(
        entity_a, entity_a_other_kind,
        "PerEntity ignores kind and id"
    );

    // PerCategory collapses type ids within a category but splits categories.
    let cat_f1 = scope_for(KeyScopeGranularity::PerCategory, &coord("a"), kind, id);
    let cat_f2 = scope_for(
        KeyScopeGranularity::PerCategory,
        &coord("a"),
        EventKind::custom(0xF, 2),
        id,
    );
    let cat_e1 = scope_for(
        KeyScopeGranularity::PerCategory,
        &coord("a"),
        EventKind::custom(0xE, 1),
        id,
    );
    assert_eq!(cat_f1, cat_f2, "same category shares a scope");
    assert_ne!(cat_f1, cat_e1, "distinct categories split");

    // PerTypeId splits on the full kind; PerEvent splits on the id.
    let type_f1 = scope_for(KeyScopeGranularity::PerTypeId, &coord("a"), kind, id);
    let type_f2 = scope_for(
        KeyScopeGranularity::PerTypeId,
        &coord("a"),
        EventKind::custom(0xF, 2),
        id,
    );
    assert_ne!(type_f1, type_f2, "distinct kinds split under PerTypeId");
    let evt_1 = scope_for(KeyScopeGranularity::PerEvent, &coord("a"), kind, id);
    let evt_2 = scope_for(
        KeyScopeGranularity::PerEvent,
        &coord("a"),
        kind,
        EventId::from(8u128),
    );
    assert_ne!(evt_1, evt_2, "distinct ids split under PerEvent");

    // Different granularities never collide (distinct discriminants).
    assert_ne!(entity_a, cat_f1);
    assert_ne!(cat_f1, type_f1);
    assert_ne!(type_f1, evt_1);
}

#[test]
fn default_granularity_is_per_entity() {
    assert_eq!(
        KeyScopeGranularity::default(),
        KeyScopeGranularity::PerEntity
    );
}

#[test]
fn seal_then_open_round_trips() {
    let key = fixed_key(0x11);
    let nonce = [0x22u8; NONCE_LEN];
    let aad = b"associated";
    let plaintext = b"top secret payload";
    let ciphertext = key.seal(&nonce, aad, plaintext).expect("seal");
    assert_ne!(
        ciphertext.as_slice(),
        plaintext,
        "payload must be encrypted"
    );
    let recovered = key.open(&nonce, aad, &ciphertext).expect("open");
    assert_eq!(
        recovered.as_slice(),
        plaintext,
        "round-trip must recover plaintext"
    );
}

#[test]
fn open_fails_on_wrong_key_nonce_or_aad() {
    let key = fixed_key(0x11);
    let nonce = [0x22u8; NONCE_LEN];
    let aad = b"aad";
    let ciphertext = key.seal(&nonce, aad, b"payload").expect("seal");

    let wrong_key = fixed_key(0x33);
    assert_eq!(
        wrong_key.open(&nonce, aad, &ciphertext),
        Err(KeyStoreError::Open),
        "wrong key must fail"
    );

    let wrong_nonce = [0x44u8; NONCE_LEN];
    assert_eq!(
        key.open(&wrong_nonce, aad, &ciphertext),
        Err(KeyStoreError::Open),
        "wrong nonce must fail"
    );

    assert_eq!(
        key.open(&nonce, b"other", &ciphertext),
        Err(KeyStoreError::Open),
        "wrong aad must fail"
    );

    let mut tampered = ciphertext.clone();
    if let Some(first) = tampered.first_mut() {
        *first ^= 0xFF;
    }
    assert_eq!(
        key.open(&nonce, aad, &tampered),
        Err(KeyStoreError::Open),
        "tampered ciphertext must fail"
    );
}

#[test]
fn get_or_create_mints_once_and_returns_the_same_key() {
    let mut store = KeyStore::new(KeyScopeGranularity::PerEntity);
    let scope = scope_for(
        KeyScopeGranularity::PerEntity,
        &coord("entity:mint"),
        EventKind::custom(0xF, 1),
        EventId::from(1u128),
    );
    let nonce = [0x01u8; NONCE_LEN];

    let ciphertext = {
        let key = store.get_or_create(&scope).expect("mint");
        key.seal(&nonce, b"", b"same-key?").expect("seal")
    };
    // A second get_or_create must return the SAME key: opening succeeds.
    let recovered = {
        let key = store.get_or_create(&scope).expect("reuse");
        key.open(&nonce, b"", &ciphertext)
            .expect("open with reused key")
    };
    assert_eq!(recovered.as_slice(), b"same-key?");
}

#[test]
fn destroy_removes_key_and_shreds_prior_ciphertext() {
    let mut store = KeyStore::new(KeyScopeGranularity::PerEvent);
    let scope = scope_for(
        KeyScopeGranularity::PerEvent,
        &coord("entity:shred"),
        EventKind::custom(0xF, 1),
        EventId::from(5u128),
    );
    let nonce = [0x09u8; NONCE_LEN];

    let ciphertext = {
        let key = store.get_or_create(&scope).expect("mint");
        key.seal(&nonce, b"", b"shred me").expect("seal")
    };

    assert!(store.get(&scope).is_some(), "key exists before destroy");
    assert!(store.destroy(&scope), "destroy removes an existing key");
    assert!(store.get(&scope).is_none(), "get after destroy is None");
    assert!(
        !store.destroy(&scope),
        "destroying an absent scope is false"
    );

    // A freshly minted key for the same scope cannot open the old ciphertext.
    let fresh = store.get_or_create(&scope).expect("re-mint");
    assert_eq!(
        fresh.open(&nonce, b"", &ciphertext),
        Err(KeyStoreError::Open),
        "post-shred key must not recover the old payload"
    );
}

#[test]
fn generate_yields_distinct_keys() {
    let a = PayloadKey::generate().expect("key a");
    let b = PayloadKey::generate().expect("key b");
    let nonce = [0u8; NONCE_LEN];
    let ciphertext = a.seal(&nonce, b"", b"probe").expect("seal");
    // Two independently generated keys are (overwhelmingly) distinct: b
    // cannot open a's ciphertext.
    assert_eq!(
        b.open(&nonce, b"", &ciphertext),
        Err(KeyStoreError::Open),
        "independent keys must differ"
    );
}

#[test]
fn payload_key_debug_does_not_leak_bytes() {
    let key = fixed_key(0xAB);
    let rendered = format!("{key:?}");
    assert!(
        !rendered.contains("ab"),
        "debug must not print hex key bytes: {rendered}"
    );
    assert!(
        !rendered.contains("171"),
        "debug must not print decimal key bytes: {rendered}"
    );
    assert!(
        rendered.contains("PayloadKey"),
        "debug still names the type: {rendered}"
    );
}

#[test]
fn payload_aad_binds_ciphertext_to_event_identity() {
    // The AAD binds coordinate + kind + event id, so a ciphertext sealed under
    // one event's identity cannot be opened under another's (relocation/tamper),
    // even with the SAME key and SAME nonce.
    let mut store = KeyStore::new(KeyScopeGranularity::PerEntity);
    let coordinate = coord("entity:aad");
    let kind = EventKind::custom(0xF, 1);
    let scope = scope_for(
        KeyScopeGranularity::PerEntity,
        &coordinate,
        kind,
        EventId::from(1u128),
    );
    let key = store.get_or_create(&scope).expect("mint");
    let nonce = [0x5u8; NONCE_LEN];

    let aad_event_1 = payload_aad(&coordinate, kind, EventId::from(1u128));
    let ciphertext = key
        .seal(&nonce, &aad_event_1, b"bound secret")
        .expect("seal");

    // A DIFFERENT event id → different AAD → authentication fails (tamper).
    let aad_event_2 = payload_aad(&coordinate, kind, EventId::from(2u128));
    assert_eq!(
        key.open(&nonce, &aad_event_2, &ciphertext),
        Err(KeyStoreError::Open),
        "relocating the ciphertext onto a different event id must fail to open"
    );
    // A DIFFERENT coordinate → different AAD → also fails.
    let other_coord = coord("entity:other");
    let aad_other = payload_aad(&other_coord, kind, EventId::from(1u128));
    assert_eq!(
        key.open(&nonce, &aad_other, &ciphertext),
        Err(KeyStoreError::Open),
        "relocating the ciphertext onto a different coordinate must fail to open"
    );
    // The correct identity still opens.
    assert_eq!(
        key.open(&nonce, &aad_event_1, &ciphertext).expect("open"),
        b"bound secret",
    );
}
