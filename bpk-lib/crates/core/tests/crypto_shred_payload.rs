//! Stage C/D public-surface coverage for crypto-shred encrypt-on-append +
//! decrypt-on-read: round-trip, `verify_chain` over ciphertext, the shred →
//! `Shredded` payoff, the durability fence, and the byte-identical plaintext
//! path. Stage D adds the explicit `Store::shred_scope` erasure op, the
//! system-events plaintext carve-out, and the sibling-scope isolation proof.
//! Gated behind `payload-encryption` (the whole file compiles out of a default
//! build).
//!
//! INVARIANTS: INV-CRYPTO-SHRED-SCOPE-DESTROYS-PLAINTEXT — crypto-shredding a
//! scope destroys its plaintext (payloads read `Shredded`, unrecoverable) while
//! `verify_chain` stays intact and the hash chain is unbroken; system events
//! stay plaintext (never encrypted, never shreddable) and a non-shredded sibling
//! scope still decrypts.
#![cfg(feature = "payload-encryption")]

use batpak::coordinate::{Coordinate, DagPosition};
use batpak::event::{EventHeader, EventKind, PayloadEncryption};
use batpak::id::EventId;
use batpak::store::{
    scope_for, AppendOptions, BatchAppendItem, CausationRef, KeyScopeGranularity, KeyStore,
    ReadDisposition, ReceiptVerification, ShredScope, SigningKey, Store, StoreConfig, StoreError,
};

const GRAN: KeyScopeGranularity = KeyScopeGranularity::PerEntity;
const KIND: EventKind = EventKind::DATA;

fn open_encrypted(dir: &std::path::Path) -> Store {
    Store::open(StoreConfig::new(dir).with_payload_encryption(GRAN)).expect("open encrypted store")
}

#[test]
fn encrypt_append_round_trips_and_on_disk_payload_is_ciphertext() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let store = open_encrypted(dir.path());
    let coord = Coordinate::new("entity:round-trip", "scope:c").expect("coord");
    let payload = serde_json::json!({ "secret": "attack at dawn", "n": 7 });

    let receipt = store
        .append(&coord, KIND, &payload)
        .expect("encrypted append");

    // Read back through the key-aware surface → ORIGINAL plaintext.
    let got = store.get(receipt.event_id).expect("get decrypts");
    assert_eq!(
        got.event.payload, payload,
        "round-trip yields the plaintext"
    );

    // The disposition surface reports a readable payload as Present.
    let disposition = store
        .get_shreddable(receipt.event_id)
        .expect("get_shreddable");
    assert!(
        matches!(&disposition, ReadDisposition::Present(_)),
        "a readable event reports Present, not Shredded"
    );
    if let ReadDisposition::Present(stored) = disposition {
        assert_eq!(
            stored.event.payload, payload,
            "Present carries the plaintext"
        );
    }

    // On disk the payload is CIPHERTEXT — not the plaintext MessagePack — and the
    // header carries the encryption metadata that drives the read path.
    let plaintext_msgpack = batpak::encoding::to_bytes(&payload).expect("encode plaintext");
    let raw = store
        .read_raw(receipt.event_id)
        .expect("read raw ciphertext");
    assert_ne!(
        raw.event.payload, plaintext_msgpack,
        "the stored payload must be ciphertext, never the plaintext bytes"
    );
    let meta: &PayloadEncryption = raw
        .event
        .header
        .payload_encryption
        .as_ref()
        .expect("encrypted event stamps PayloadEncryption in the header");
    assert_eq!(meta.nonce.len(), 24, "XChaCha20 nonce is 192-bit");
    assert!(
        !meta.keyscope_id.is_empty(),
        "the scope id the read path looks the key up under is present"
    );
    // The content hash the receipt commits to is the hash of the STORED bytes
    // (ciphertext): a full recompute over disk agrees, proving event_hash is
    // computed over the ciphertext, not the plaintext.
    assert!(
        store.verify_chain().expect("verify_chain").is_intact(),
        "receipt/chain hashes match the stored ciphertext"
    );
    assert_ne!(receipt.content_hash, [0u8; 32]);
}

#[test]
fn verify_chain_is_intact_over_encrypted_events() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let store = open_encrypted(dir.path());
    let coord = Coordinate::new("entity:chain", "scope:c").expect("coord");
    for i in 0..5 {
        let _ = store
            .append(&coord, KIND, &serde_json::json!({ "i": i }))
            .expect("encrypted append");
    }
    let report = store.verify_chain().expect("verify_chain");
    assert!(
        report.is_intact(),
        "the hash chain verifies over ciphertext (it hashes the stored bytes): {report:?}"
    );
    assert!(report.events_checked >= 5);
}

#[test]
fn append_receipt_signature_verifies_over_an_encrypted_event() {
    // The signing cover is `event_id + sequence + coord + kind + prev_hash +
    // content_hash + extensions` (store/signing.rs::cover_bytes) — it takes NO
    // header, so the `payload_encryption` header field is outside it, exactly like
    // `payload_version`. With `content_hash = blake3(ciphertext)`, a receipt over
    // an encrypted event must still verify as Signed (the signature covers the
    // stored ciphertext's hash, unchanged by the encryption metadata).
    let dir = tempfile::tempdir().expect("tmpdir");
    let config = StoreConfig::new(dir.path())
        .with_payload_encryption(GRAN)
        .with_signing_key(SigningKey::from_bytes([0x42; 32]));
    let store = Store::open(config).expect("open signed encrypted store");
    let coord = Coordinate::new("entity:signed", "scope:c").expect("coord");

    let receipt = store
        .append(&coord, KIND, &serde_json::json!({ "signed": "secret" }))
        .expect("append");
    assert!(
        matches!(
            store.verify_append_receipt(&receipt),
            ReceiptVerification::Signed
        ),
        "the receipt signature must verify over the encrypted event's cover"
    );
    assert!(store.verify_chain().expect("verify_chain").is_intact());
}

#[test]
fn shredding_the_key_yields_shredded_and_keeps_the_chain_intact() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let coord = Coordinate::new("entity:shred", "scope:c").expect("coord");
    let payload = serde_json::json!({ "pii": "delete me cryptographically" });

    let event_id = {
        let store = open_encrypted(dir.path());
        let receipt = store.append(&coord, KIND, &payload).expect("append");
        // Sanity: readable BEFORE the shred.
        assert_eq!(
            store
                .get(receipt.event_id)
                .expect("pre-shred get")
                .event
                .payload,
            payload
        );
        receipt.event_id
    };

    // Crypto-shred: destroy the scope's key in the durable keyset (KeyStore is the
    // Stage A/B mechanism) and persist the destruction. PerEntity → the scope is a
    // function of the entity only.
    let scope = scope_for(GRAN, &coord, KIND, EventId::from(1u128));
    {
        let mut keyset = KeyStore::load(dir.path(), GRAN).expect("load keyset");
        assert!(
            keyset.destroy(&scope),
            "the scope key existed and was destroyed"
        );
        keyset.flush(dir.path()).expect("persist the shred");
    }

    // Reopen: the key is gone. The event is present in the chain, plaintext gone.
    let store = open_encrypted(dir.path());
    assert!(
        matches!(
            store.get_shreddable(event_id).expect("get_shreddable"),
            ReadDisposition::Shredded
        ),
        "a destroyed key must read as Shredded"
    );
    // The typed error surface says the same thing (NOT a corruption error).
    assert!(
        matches!(store.get(event_id), Err(StoreError::PayloadShredded { .. })),
        "get surfaces PayloadShredded for a shredded payload"
    );
    // The crypto-shred payoff: the chain is STILL intact — only the plaintext is
    // unrecoverable, the ciphertext and its hash identity survive.
    assert!(
        store.verify_chain().expect("verify_chain").is_intact(),
        "verify_chain stays intact after a shred"
    );
}

#[test]
fn durability_fence_persists_a_minted_key_before_the_append_is_acked() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let coord = Coordinate::new("entity:fence", "scope:c").expect("coord");
    let payload = serde_json::json!({ "durable": true });

    let event_id = {
        let store = open_encrypted(dir.path());
        // This append MINTS the entity's first key. The fence flushes the keyset
        // durable BEFORE the append is acknowledged — and `close`/drop does NOT
        // flush the keyset (only the idempotency store), so the ONLY reason the
        // key can survive a reopen is the fence having flushed it at mint.
        let receipt = store.append(&coord, KIND, &payload).expect("append");
        receipt.event_id
        // store dropped here — no explicit keyset flush on the close path.
    };

    let store = open_encrypted(dir.path());
    assert!(
        store.payload_key_count().unwrap_or(0) >= 1,
        "the keyset survived the reopen with at least the minted key"
    );
    // The decisive fence proof: the payload DECRYPTS after the reopen. `close`
    // does not flush the keyset, so the entity's key can only be on disk because
    // the append's fence flushed it durable at mint — otherwise this event would
    // read as Shredded (a spontaneous, unintended crypto-shred of live data).
    assert_eq!(
        store
            .get(event_id)
            .expect("decrypts after reopen")
            .event
            .payload,
        payload,
        "the payload decrypts under the survived key (no spontaneous shred)"
    );
}

#[test]
fn encrypted_batch_round_trips_and_verifies() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let store = open_encrypted(dir.path());
    let coord = Coordinate::new("entity:batch", "scope:c").expect("coord");

    let items: Vec<BatchAppendItem> = (0..4)
        .map(|i| {
            BatchAppendItem::new(
                coord.clone(),
                KIND,
                &serde_json::json!({ "item": i }),
                AppendOptions::default(),
                CausationRef::None,
            )
            .expect("batch item")
        })
        .collect();
    let receipts = store.append_batch(items).expect("encrypted batch append");
    assert_eq!(receipts.len(), 4);

    for (i, receipt) in receipts.iter().enumerate() {
        let got = store.get(receipt.event_id).expect("batch item decrypts");
        assert_eq!(got.event.payload, serde_json::json!({ "item": i }));
        // On disk each batch payload is ciphertext.
        let raw = store.read_raw(receipt.event_id).expect("read raw");
        assert!(raw.event.header.payload_encryption.is_some());
    }
    assert!(
        store.verify_chain().expect("verify_chain").is_intact(),
        "verify_chain holds over an encrypted batch"
    );
}

#[test]
fn plaintext_none_config_leaves_frames_unencrypted_and_byte_identical() {
    let dir = tempfile::tempdir().expect("tmpdir");
    // NO `with_payload_encryption` → key_store is None → the plaintext path.
    let store = Store::open(StoreConfig::new(dir.path())).expect("open plaintext store");
    let coord = Coordinate::new("entity:plain", "scope:c").expect("coord");
    let payload = serde_json::json!({ "plain": "text", "v": 1 });

    let receipt = store
        .append(&coord, KIND, &payload)
        .expect("plaintext append");

    // The on-disk payload is the plaintext MessagePack — untouched.
    let plaintext_msgpack = batpak::encoding::to_bytes(&payload).expect("encode plaintext");
    let raw = store.read_raw(receipt.event_id).expect("read raw");
    assert_eq!(
        raw.event.payload, plaintext_msgpack,
        "with no encryption configured the stored payload is the plaintext bytes"
    );
    assert!(
        raw.event.header.payload_encryption.is_none(),
        "no encryption metadata is stamped on a plaintext event"
    );
    assert_eq!(
        store.get(receipt.event_id).expect("get").event.payload,
        payload
    );

    // Byte-identity witness: a `None` header serializes WITHOUT the
    // `payload_encryption` key (skip_serializing_if), so its named-map frame bytes
    // are identical to a build compiled without the field at all.
    let header = EventHeader::new(1, 1, None, 0, DagPosition::new(0, 0, 1), 0, KIND);
    assert!(header.payload_encryption.is_none());
    let bytes = batpak::encoding::to_bytes(&header).expect("encode header");
    assert!(
        !contains_subslice(&bytes, b"payload_encryption"),
        "a None header must omit the payload_encryption map key entirely"
    );
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

// ── Stage D: the explicit erasure op + system carve-out + sibling isolation ──

/// Witness for INV-CRYPTO-SHRED-SCOPE-DESTROYS-PLAINTEXT. Crypto-shredding a
/// scope via the explicit `Store::shred_scope` op makes every user payload in
/// that scope unrecoverable (reads `Shredded`) while `verify_chain` stays intact
/// and the hash chain is unbroken; the SYSTEM_OPEN_COMPLETED lifecycle event is
/// never encrypted (stays plaintext, not shreddable); and a non-shredded sibling
/// scope still decrypts.
#[test]
fn shred_scope_destroys_scope_plaintext_and_keeps_chain_intact() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let store = open_encrypted(dir.path());

    // System-events carve-out proof: opening the store appended a
    // SYSTEM_OPEN_COMPLETED lifecycle event, but that reserved kind is NOT
    // encrypted — it mints NO scope key. The keyset is empty until the first
    // USER append.
    assert_eq!(
        store.payload_key_count(),
        Some(0),
        "opening mints no key: the system open-completed event stays plaintext"
    );

    let secret = Coordinate::new("entity:secret", "scope:c").expect("coord");
    let sibling = Coordinate::new("entity:keep", "scope:c").expect("coord");

    // Two encrypted USER events in the scope we will shred, one in a sibling scope.
    let secret_a = store
        .append(&secret, KIND, &serde_json::json!({ "pii": "alice" }))
        .expect("append secret a");
    let secret_b = store
        .append(&secret, KIND, &serde_json::json!({ "pii": "bob" }))
        .expect("append secret b");
    let kept = store
        .append(&sibling, KIND, &serde_json::json!({ "keep": "me" }))
        .expect("append sibling");

    // PerEntity: the user appends minted exactly the two entity keys, and still no
    // key for the plaintext system event(s).
    assert_eq!(
        store.payload_key_count(),
        Some(2),
        "one key per user entity, none for system events"
    );

    // The SYSTEM_OPEN_COMPLETED lifecycle event is present and stored as PLAINTEXT
    // (no encryption metadata on its frame) — store mechanism, not user data.
    let system_entries = store.by_fact(EventKind::SYSTEM_OPEN_COMPLETED);
    let system_id = system_entries
        .first()
        .expect("mutable open appended a SYSTEM_OPEN_COMPLETED event")
        .event_id();
    assert!(
        store
            .read_raw(system_id)
            .expect("read system frame")
            .event
            .header
            .payload_encryption
            .is_none(),
        "a system event is never encrypted — no PayloadEncryption stamped on its frame"
    );

    // Pre-shred: every user payload decrypts.
    assert_eq!(
        store.get(secret_a.event_id).expect("pre a").event.payload,
        serde_json::json!({ "pii": "alice" })
    );
    assert_eq!(
        store.get(kept.event_id).expect("pre kept").event.payload,
        serde_json::json!({ "keep": "me" })
    );

    // THE ERASURE OP: crypto-shred the secret scope. PerEntity → an Entity selector.
    let destroyed = store
        .shred_scope(ShredScope::Entity(&secret))
        .expect("shred_scope succeeds");
    assert!(
        destroyed,
        "a live key existed for the scope and was destroyed"
    );
    assert_eq!(
        store.payload_key_count(),
        Some(1),
        "the secret scope key is gone; the sibling key remains"
    );

    // Payoff: both secret payloads are now unrecoverable, by BOTH the disposition
    // surface and the typed error surface.
    for id in [secret_a.event_id, secret_b.event_id] {
        assert!(
            matches!(
                store.get_shreddable(id).expect("get_shreddable"),
                ReadDisposition::Shredded
            ),
            "a shredded scope reads Shredded"
        );
        assert!(
            matches!(store.get(id), Err(StoreError::PayloadShredded { .. })),
            "a shredded scope surfaces PayloadShredded"
        );
    }

    // The system event STILL reads plaintext — never encrypted, so a user-scope
    // shred cannot touch it.
    assert!(
        matches!(
            store.get_shreddable(system_id).expect("system readable"),
            ReadDisposition::Present(_)
        ),
        "the plaintext system event is unaffected by a user-scope shred"
    );
    assert!(store
        .read_raw(system_id)
        .expect("read system frame after shred")
        .event
        .header
        .payload_encryption
        .is_none());

    // Sibling isolation: the non-shredded scope still decrypts.
    assert_eq!(
        store
            .get(kept.event_id)
            .expect("sibling still decrypts")
            .event
            .payload,
        serde_json::json!({ "keep": "me" })
    );

    // THE INVARIANT: verify_chain is STILL intact after the shred — the ciphertext
    // and its hash-chain identity survive on disk; only the plaintext is gone.
    assert!(
        store.verify_chain().expect("verify_chain").is_intact(),
        "the hash chain is unbroken after a crypto-shred"
    );
}

/// A `shred_scope` selector that cannot address the configured granularity is a
/// typed refusal that destroys NOTHING (never a silent no-op or a mis-targeted
/// shred).
#[test]
fn shred_scope_rejects_a_mismatched_selector() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let store = open_encrypted(dir.path()); // PerEntity granularity
    let coord = Coordinate::new("entity:x", "scope:c").expect("coord");
    let receipt = store
        .append(&coord, KIND, &serde_json::json!({ "n": 1 }))
        .expect("append");
    assert_eq!(store.payload_key_count(), Some(1));

    // A Kind selector cannot address a PerEntity scope → typed mismatch.
    assert!(
        matches!(
            store.shred_scope(ShredScope::Kind(KIND)),
            Err(StoreError::ShredSelectorMismatch { .. })
        ),
        "a Kind selector on a PerEntity store is a typed mismatch"
    );
    // Nor can an Event selector.
    assert!(
        matches!(
            store.shred_scope(ShredScope::Event(EventId::from(7u128))),
            Err(StoreError::ShredSelectorMismatch { .. })
        ),
        "an Event selector on a PerEntity store is a typed mismatch"
    );

    // The refusal touched nothing: the key is intact and the payload still decrypts.
    assert_eq!(
        store.payload_key_count(),
        Some(1),
        "a mismatched selector shreds nothing"
    );
    assert_eq!(
        store
            .get(receipt.event_id)
            .expect("still decrypts")
            .event
            .payload,
        serde_json::json!({ "n": 1 })
    );
}

/// `shred_scope` on a store opened WITHOUT `payload_encryption` (no keyset) is a
/// typed configuration error, not a silent success.
#[test]
fn shred_scope_without_encryption_is_a_config_error() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open plaintext store");
    let coord = Coordinate::new("entity:plain", "scope:c").expect("coord");
    assert!(
        matches!(
            store.shred_scope(ShredScope::Entity(&coord)),
            Err(StoreError::Configuration(_))
        ),
        "shred_scope with no keyset configured is a typed Configuration error"
    );
}
