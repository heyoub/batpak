//! Stage E2 coverage for crypto-shred KEY-AWARE LIVE DELIVERY.
//!
//! Stage C made an encrypted event's on-disk payload ciphertext and Stage E1
//! made single-event read / projection / compaction key-aware. E2 closes the
//! last read consumer: LIVE DELIVERY. Before E2 a reactor built its delivered
//! event from the stored (ciphertext) bytes — an encrypted event was either
//! misdecoded or silently dropped. These tests prove the E2 contract:
//!
//!   * [`Store::read_delivery_payload`] — the core-boundary decrypt primitive —
//!     yields the PLAINTEXT bytes for a readable event and a
//!     [`DeliveryPayload::Shredded`] marker for a crypto-shredded one.
//!   * A reactor over an ENCRYPTED entity DELIVERS the decrypted event (JSON and
//!     raw-msgpack lanes).
//!   * A SHREDDED event is SKIPPED loudly (never delivered as ciphertext) and the
//!     cursor still advances past it, so delivery ordering stays coherent and the
//!     loop never stalls or re-loops.
//!
//! Gated behind `payload-encryption` (the whole file compiles out of a default
//! build; the plaintext, no-keyset delivery path is covered byte-identically by
//! the ungated `react_loop_typed` / `react_loop_multi_raw` suites).
//!
//! INVARIANTS: INV-CRYPTO-SHRED-SCOPE-DESTROYS-PLAINTEXT — a crypto-shredded
//! payload is never delivered as plaintext or ciphertext; a readable encrypted
//! payload is delivered decrypted.
#![cfg(feature = "payload-encryption")]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use batpak::event::StoredEvent;
use batpak::store::{DeliveryPayload, KeyScopeGranularity, ShredScope, Store, StoreConfig};
use batpak_testkit::prelude::*;

const GRAN: KeyScopeGranularity = KeyScopeGranularity::PerEntity;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 9, type_id = 1)]
struct Secret {
    note: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 9, type_id = 7)]
struct SecretRaw {
    note: String,
}

#[derive(Debug)]
struct NeverFails;
impl std::fmt::Display for NeverFails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "never")
    }
}
impl std::error::Error for NeverFails {}

/// Reactor that records the plaintext `note` of every delivered `Secret`.
struct CollectReactor {
    notes: Arc<Mutex<Vec<String>>>,
}

impl TypedReactive<Secret> for CollectReactor {
    type Error = NeverFails;
    fn react(
        &mut self,
        event: &StoredEvent<Secret>,
        _out: &mut ReactionBatch,
        _witness: Option<&batpak::store::AtLeastOnce>,
    ) -> Result<(), Self::Error> {
        self.notes
            .lock()
            .expect("notes lock")
            .push(event.event.payload.note.clone());
        Ok(())
    }
}

/// Raw-lane multi reactor: records the plaintext `note` of every delivered
/// `SecretRaw` decoded from the raw MessagePack lane.
#[derive(MultiEventReactor)]
#[batpak(input = RawMsgpackInput, error = NeverFails)]
#[batpak(event = SecretRaw, handler = on_secret)]
struct RawCollectReactor {
    notes: Arc<Mutex<Vec<String>>>,
}

impl RawCollectReactor {
    fn on_secret(
        &mut self,
        event: &StoredEvent<SecretRaw>,
        _out: &mut ReactionBatch,
        _witness: Option<&batpak::store::AtLeastOnce>,
    ) -> Result<(), NeverFails> {
        self.notes
            .lock()
            .expect("notes lock")
            .push(event.event.payload.note.clone());
        Ok(())
    }
}

fn open_encrypted(dir: &std::path::Path) -> Store {
    Store::open(StoreConfig::new(dir).with_payload_encryption(GRAN)).expect("open encrypted store")
}

fn wait_for<F: Fn() -> bool>(cond: F, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    cond()
}

#[test]
fn read_delivery_payload_decrypts_then_reports_shredded() {
    // The core-boundary decrypt primitive: a readable event yields its PLAINTEXT
    // MessagePack bytes; after the scope key is destroyed the SAME event yields a
    // `Shredded` marker — never the ciphertext.
    let dir = tempfile::tempdir().expect("tmpdir");
    let store = open_encrypted(dir.path());
    let coord = Coordinate::new("entity:delivery-primitive", "scope:e2").expect("coord");
    let payload = Secret {
        note: "attack at dawn".to_owned(),
    };
    let receipt = store.append_typed(&coord, &payload).expect("append");

    let readable = store
        .read_delivery_payload(receipt.event_id)
        .expect("read_delivery_payload readable");
    assert!(
        matches!(&readable, DeliveryPayload::Readable(_)),
        "a readable encrypted event delivers as Readable, never Shredded"
    );
    if let DeliveryPayload::Readable(stored) = readable {
        assert_eq!(
            stored.event.header.event_id, receipt.event_id,
            "the readable delivery payload keeps the event identity"
        );
        let decoded: Secret =
            rmp_serde::from_slice(&stored.event.payload).expect("decode plaintext bytes");
        assert_eq!(
            decoded, payload,
            "delivery yields the DECRYPTED plaintext, decodable to the original payload"
        );
        let expected = batpak::encoding::to_bytes(&payload).expect("encode plaintext");
        assert_eq!(
            stored.event.payload, expected,
            "the delivered bytes are exactly the plaintext MessagePack (not ciphertext)"
        );
    }

    assert!(
        store
            .shred_scope(ShredScope::Entity(&coord))
            .expect("shred_scope"),
        "shredding a live scope destroys its key"
    );

    let shredded = store
        .read_delivery_payload(receipt.event_id)
        .expect("read_delivery_payload shredded");
    assert!(
        matches!(&shredded, DeliveryPayload::Shredded { event_id } if *event_id == receipt.event_id),
        "after the key is destroyed the SAME event delivers as Shredded with its id — \
         never as readable/ciphertext"
    );
}

#[test]
fn json_reactor_over_encrypted_entity_delivers_decrypted_and_skips_shredded_coherently() {
    // A shredded event appears FIRST in commit order; the reactor must skip it
    // loudly (never deliver it) yet still advance past it to deliver the three
    // live encrypted events decrypted — proving delivery stays coherent and never
    // stalls on the shredded head.
    let dir = tempfile::tempdir().expect("tmpdir");
    let store = Arc::new(open_encrypted(dir.path()));

    let doomed = Coordinate::new("entity:doomed", "scope:e2").expect("doomed coord");
    let _ = store
        .append_typed(
            &doomed,
            &Secret {
                note: "must-never-be-delivered".to_owned(),
            },
        )
        .expect("append doomed");

    let live = Coordinate::new("entity:live", "scope:e2").expect("live coord");
    for note in ["a", "b", "c"] {
        let _ = store
            .append_typed(
                &live,
                &Secret {
                    note: note.to_owned(),
                },
            )
            .expect("append live");
    }

    // Destroy the doomed scope's key: its one event becomes unrecoverable.
    assert!(
        store
            .shred_scope(ShredScope::Entity(&doomed))
            .expect("shred doomed"),
        "shredding the doomed scope destroys its key"
    );

    let notes = Arc::new(Mutex::new(Vec::<String>::new()));
    let reactor = CollectReactor {
        notes: Arc::clone(&notes),
    };
    let handle = store
        .react_loop_typed::<Secret, _>(&Region::all(), ReactorConfig::default(), reactor)
        .expect("spawn reactor");

    assert!(
        wait_for(
            || notes.lock().expect("notes lock").len() == 3,
            Duration::from_secs(3),
        ),
        "reactor must deliver all THREE live decrypted events despite the shredded head; got {:?}",
        notes.lock().expect("notes lock").clone()
    );

    handle.stop();
    handle
        .join()
        .expect("reactor stops cleanly, not via a shredded-induced error");

    let delivered = notes.lock().expect("notes lock").clone();
    assert_eq!(
        delivered,
        vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
        "delivered notes are the decrypted plaintext, in commit order, with the shredded event skipped"
    );
    assert!(
        !delivered.iter().any(|n| n == "must-never-be-delivered"),
        "the shredded event's plaintext is NEVER delivered"
    );
}

#[test]
fn raw_msgpack_reactor_over_encrypted_entity_delivers_decrypted() {
    // The raw-msgpack reactor lane decrypts at the core boundary too: without the
    // key-aware fetch the per-kind raw decode would see ciphertext and fail.
    let dir = tempfile::tempdir().expect("tmpdir");
    let store = Arc::new(open_encrypted(dir.path()));
    let live = Coordinate::new("entity:raw-live", "scope:e2").expect("raw live coord");
    for note in ["x", "y"] {
        let _ = store
            .append_typed(
                &live,
                &SecretRaw {
                    note: note.to_owned(),
                },
            )
            .expect("append raw live");
    }

    let notes = Arc::new(Mutex::new(Vec::<String>::new()));
    let reactor = RawCollectReactor {
        notes: Arc::clone(&notes),
    };
    let handle = store
        .react_loop_multi_raw(&Region::all(), ReactorConfig::default(), reactor)
        .expect("spawn raw reactor");

    assert!(
        wait_for(
            || notes.lock().expect("notes lock").len() == 2,
            Duration::from_secs(3),
        ),
        "raw reactor must deliver both decrypted events; got {:?}",
        notes.lock().expect("notes lock").clone()
    );

    handle.stop();
    handle.join().expect("raw reactor clean stop");

    let mut delivered = notes.lock().expect("notes lock").clone();
    delivered.sort();
    assert_eq!(
        delivered,
        vec!["x".to_owned(), "y".to_owned()],
        "the raw lane delivers DECRYPTED plaintext decoded per-kind, not ciphertext"
    );
}
