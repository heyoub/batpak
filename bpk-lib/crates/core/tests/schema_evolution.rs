//! Event payload schema-evolution proofs.
//!
//! PROVES: INV-EVENT-PAYLOAD-DECODE-BACKCOMPAT — the single decode seam keeps
//! decoding frozen payload bytes as schemas evolve: additive-with-default is
//! absorbed by serde, non-additive change is repaired by a registered `Upcast`
//! chain, and a future version is a hard `FutureVersion` error.
//! CATCHES: a struct edit that silently breaks decode of historical bytes;
//! an upcaster regression; a content-hash/signature shift caused by the new
//! header field.
//! SEEDED: append-only `.hex` fixtures under `tests/golden/payloads/`.
//! DEFENDS: FM-010 (Semantic Drift).
//! INVARIANTS: INV-EVENT-PAYLOAD-DECODE-BACKCOMPAT,
//! ART-EVENT-PAYLOAD-FROZEN-GOLDENS, ART-SCHEMA-EVOLUTION-DOC.
//!
//! Frozen fixtures are the *payload* msgpack bytes (journal / layer-1 form),
//! decoded with the CURRENT decoder. Regeneration is APPEND-ONLY: under the
//! sentinel `GOLDEN_UPDATE=I_KNOW_WHAT_IM_DOING` a missing fixture is written;
//! an EXISTING fixture is NEVER overwritten (proof-of-compat bytes must not be
//! silently mutated — bump the version and freeze `__vN+1` instead):
//!
//!   GOLDEN_UPDATE=I_KNOW_WHAT_IM_DOING cargo test --test schema_evolution

use batpak::canonical;
use batpak::event::upcast::UpcastError;
use batpak::event::{DecodeTyped, Event, EventHeader, EventKind, TypedDecodeError, Upcast};
// Crate-root re-export brings BOTH the `EventPayload` trait and its derive macro
// into scope (serde-style), so `#[derive(EventPayload)]` resolves here.
use batpak::register_upcast;
use batpak::EventPayload;
use serde::{Deserialize, Serialize};
use std::io::Write;

// ─── Frozen-decode fixture helper ───────────────────────────────────────────

fn payloads_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/payloads")
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Vec<u8> {
    let s = s.trim();
    assert!(s.len().is_multiple_of(2), "odd-length hex fixture");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
        .collect()
}

/// Decode the frozen `<kind>__v<N>.hex` payload bytes with the current decoder
/// at stored version `version` and assert equality with `expected`.
///
/// On the append-only update path, if the fixture is ABSENT it is written from
/// the current encoding of `expected`; if it is PRESENT it is left untouched
/// (regeneration must bump the version and write a new file).
fn assert_frozen_decode<T>(fixture: &str, version: u16, expected: &T)
where
    T: EventPayload + PartialEq + std::fmt::Debug,
{
    let path = payloads_dir().join(fixture);
    let updating = std::env::var("GOLDEN_UPDATE").as_deref() == Ok("I_KNOW_WHAT_IM_DOING");

    if !path.exists() {
        assert!(
            updating,
            "frozen payload fixture {} not found. To create it (append-only), run \
             GOLDEN_UPDATE=I_KNOW_WHAT_IM_DOING cargo test --test schema_evolution",
            path.display()
        );
        let bytes = canonical::to_bytes(expected).expect("encode fixture payload");
        std::fs::create_dir_all(payloads_dir()).expect("create payloads dir");
        std::fs::write(&path, hex_encode(&bytes)).expect("write frozen fixture");
        let _ = writeln!(
            std::io::stderr(),
            "⚠ GOLDEN_UPDATE: wrote NEW frozen payload fixture {} (append-only; existing \
             fixtures are never overwritten). Inspect the diff before committing.",
            path.display()
        );
        return;
    }

    let bytes = hex_decode(&std::fs::read_to_string(&path).expect("read frozen fixture"));
    // Decode through the SINGLE seam at the recorded stored version: this is the
    // real proof that historical bytes still decode into the current struct.
    let header = EventHeader::new(
        1,
        1,
        None,
        0,
        batpak::coordinate::DagPosition::root(),
        0,
        T::KIND,
    )
    .with_payload_version(version);
    let event: Event<Vec<u8>> = Event::new(header, bytes);
    let decoded: T = event.decode_typed::<T>().unwrap_or_else(|e| {
        assert!(
            std::hint::black_box(false),
            "frozen fixture {fixture} failed current decode: {e}"
        );
        unreachable!("the decode assertion above always fails on error")
    });
    assert_eq!(
        &decoded, expected,
        "SCHEMA DRIFT: frozen fixture {fixture} decoded to a different value than expected. \
         If the change is intentional and non-additive, bump PAYLOAD_VERSION, add an Upcast, \
         and freeze a __vN+1 fixture — do not edit this one."
    );
}

// ─── Versioned self-test kind: a real exercising caller for the upcaster ─────

/// Current (v2) shape. v1 stored `{ count: u64 }`; v2 renames it to `total`
/// and adds a non-defaulted `label` — a non-additive change that REQUIRES the
/// upcaster (serde alone cannot rename or fill `label`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xE, type_id = 0xC02, version = 2)]
struct UpcastCounterV2 {
    total: u64,
    label: String,
}

/// The v1 -> v2 migration: rename `count` -> `total`, synthesize `label`.
struct UpcastCounterV1ToV2;

impl Upcast for UpcastCounterV1ToV2 {
    const KIND: EventKind = UpcastCounterV2::KIND;
    const FROM_VERSION: u16 = 1;

    fn upcast(value: rmpv::Value) -> Result<rmpv::Value, UpcastError> {
        let rmpv::Value::Map(mut map) = value else {
            return Err(UpcastError::ValueCodec(format!(
                "v1 UpcastCounter payload was not a map: {value:?}"
            )));
        };
        // Rename `count` -> `total`.
        for (k, _v) in map.iter_mut() {
            if k.as_str() == Some("count") {
                *k = rmpv::Value::String("total".into());
            }
        }
        // Add the new non-defaulted field.
        map.push((
            rmpv::Value::String("label".into()),
            rmpv::Value::String("legacy".into()),
        ));
        Ok(rmpv::Value::Map(map))
    }
}

register_upcast!(UpcastCounterV1ToV2);

/// The frozen v1 payload: msgpack of `{ "count": 7 }`. Append-only; this file
/// is the proof that v1-on-disk bytes still upcast+decode under the v2 decoder.
fn freeze_upcast_counter_v1() {
    // We freeze the v1 *shape* explicitly (the v2 struct can no longer produce
    // it), so encode a one-field map by hand through the canonical encoder.
    #[derive(Serialize)]
    struct UpcastCounterV1 {
        count: u64,
    }
    let v1 = UpcastCounterV1 { count: 7 };
    let path = payloads_dir().join("e_c02__v1.hex");
    let updating = std::env::var("GOLDEN_UPDATE").as_deref() == Ok("I_KNOW_WHAT_IM_DOING");
    if !path.exists() {
        assert!(
            updating,
            "frozen v1 fixture {} not found; run GOLDEN_UPDATE=I_KNOW_WHAT_IM_DOING cargo test --test schema_evolution",
            path.display()
        );
        let bytes = canonical::to_bytes(&v1).expect("encode v1 payload");
        std::fs::create_dir_all(payloads_dir()).expect("create payloads dir");
        std::fs::write(&path, hex_encode(&bytes)).expect("write v1 fixture");
        let _ = writeln!(
            std::io::stderr(),
            "⚠ GOLDEN_UPDATE: wrote NEW frozen v1 fixture {}",
            path.display()
        );
    }
}

#[test]
fn upcast_chain_lifts_frozen_v1_into_current_v2_struct() {
    freeze_upcast_counter_v1();
    let path = payloads_dir().join("e_c02__v1.hex");
    if !path.exists() {
        // Only reachable on a first-ever GOLDEN_UPDATE run; the fixture is now
        // written and the assertion runs on the next ordinary invocation.
        return;
    }
    let bytes = hex_decode(&std::fs::read_to_string(&path).expect("read v1 fixture"));

    // Stored at version 1; current decoder is version 2 → chain must run.
    let header = EventHeader::new(
        1,
        1,
        None,
        0,
        batpak::coordinate::DagPosition::root(),
        0,
        UpcastCounterV2::KIND,
    )
    .with_payload_version(1);
    let event: Event<Vec<u8>> = Event::new(header, bytes);
    let decoded: UpcastCounterV2 = event
        .decode_typed::<UpcastCounterV2>()
        .expect("v1 bytes must upcast and decode into v2");
    assert_eq!(
        decoded,
        UpcastCounterV2 {
            total: 7,
            label: "legacy".to_owned()
        },
        "PROPERTY: registered v1->v2 Upcast must rename count->total and synthesize label"
    );
}

#[test]
fn json_lane_runs_the_same_upcast_chain() {
    // The JSON lane converts serde_json::Value -> rmpv and runs the identical
    // registered chain, proving lane-neutrality.
    let v1_json = serde_json::json!({ "count": 9 });
    let header = EventHeader::new(
        2,
        2,
        None,
        0,
        batpak::coordinate::DagPosition::root(),
        0,
        UpcastCounterV2::KIND,
    )
    .with_payload_version(1);
    let event: Event<serde_json::Value> = Event::new(header, v1_json);
    let decoded: UpcastCounterV2 = event
        .decode_typed::<UpcastCounterV2>()
        .expect("v1 JSON must upcast and decode into v2");
    assert_eq!(
        decoded,
        UpcastCounterV2 {
            total: 9,
            label: "legacy".to_owned()
        }
    );
}

#[test]
fn future_version_is_a_hard_error_on_both_lanes() {
    // stored version 3 > current 2 → FutureVersion, no downcaster.
    let current = serde_json::json!({ "total": 1, "label": "x" });
    for stored in [3u16, 9u16] {
        let header = EventHeader::new(
            3,
            3,
            None,
            0,
            batpak::coordinate::DagPosition::root(),
            0,
            UpcastCounterV2::KIND,
        )
        .with_payload_version(stored);
        let json_event: Event<serde_json::Value> = Event::new(header.clone(), current.clone());
        let err = json_event
            .decode_typed::<UpcastCounterV2>()
            .expect_err("future version must be rejected");
        assert!(
            matches!(
                err,
                TypedDecodeError::FutureVersion { stored: s, current: 2, .. } if s == stored
            ),
            "PROPERTY: stored>current must be FutureVersion, got {err:?}"
        );

        let raw_bytes = canonical::to_bytes(&UpcastCounterV2 {
            total: 1,
            label: "x".to_owned(),
        })
        .expect("encode current");
        let raw_event: Event<Vec<u8>> = Event::new(header, raw_bytes);
        let err = raw_event
            .decode_typed::<UpcastCounterV2>()
            .expect_err("future version must be rejected on raw lane");
        assert!(matches!(err, TypedDecodeError::FutureVersion { .. }));
    }
}

#[test]
fn legacy_version_zero_decodes_tolerantly_as_current() {
    // version 0 = legacy/untyped sentinel → tolerant decode, NO upcast attempt.
    let bytes = canonical::to_bytes(&UpcastCounterV2 {
        total: 42,
        label: "kept".to_owned(),
    })
    .expect("encode current-shape payload");
    let header = EventHeader::new(
        4,
        4,
        None,
        0,
        batpak::coordinate::DagPosition::root(),
        0,
        UpcastCounterV2::KIND,
    ); // payload_version defaults to 0
    assert_eq!(header.payload_version, 0);
    let event: Event<Vec<u8>> = Event::new(header, bytes);
    let decoded: UpcastCounterV2 = event
        .decode_typed::<UpcastCounterV2>()
        .expect("version-0 frame must decode tolerantly as current");
    assert_eq!(decoded.total, 42);
}

#[test]
fn equal_version_decodes_without_upcast() {
    let payload = UpcastCounterV2 {
        total: 5,
        label: "now".to_owned(),
    };
    let bytes = canonical::to_bytes(&payload).expect("encode");
    let header = EventHeader::new(
        5,
        5,
        None,
        0,
        batpak::coordinate::DagPosition::root(),
        0,
        UpcastCounterV2::KIND,
    )
    .with_payload_version(2);
    let event: Event<Vec<u8>> = Event::new(header, bytes);
    let decoded: UpcastCounterV2 = event.decode_typed::<UpcastCounterV2>().expect("decode v2");
    assert_eq!(decoded, payload);
}

#[test]
fn upcast_registry_surface_is_linked_for_the_test_kind() {
    // Names the __private registration surface directly (mirrors
    // event_payload_surface.rs for the kind registry) so the inventory-backed
    // step lookup has an explicit, witnessed reference.
    let steps: Vec<&batpak::__private::UpcastRegistration> =
        batpak::__private::upcast_steps_for(UpcastCounterV2::KIND.as_raw_u16());
    assert!(
        steps.iter().any(|reg| reg.from_version == 1),
        "PROPERTY: register_upcast! must link a v1 step for the test kind via inventory"
    );
}

// ─── Additive-with-default safety + frozen fixture for a plain kind ──────────

/// A plain v1 kind whose frozen bytes must keep decoding. Demonstrates the
/// `assert_frozen_decode` helper against batpak's own derive surface.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xE, type_id = 0xC01)]
struct FrozenSample {
    amount: u64,
    note: String,
}

#[test]
fn frozen_sample_v1_still_decodes_with_current_decoder() {
    assert_frozen_decode::<FrozenSample>(
        "e_c01__v1.hex",
        1,
        &FrozenSample {
            amount: 100,
            note: "hello".to_owned(),
        },
    );
}

// ─── PAYLOAD_VERSION stamping contract ──────────────────────────────────────

#[test]
fn derive_version_key_defaults_to_one_and_overrides() {
    assert_eq!(
        FrozenSample::PAYLOAD_VERSION,
        1,
        "PROPERTY: derive without version key defaults to PAYLOAD_VERSION = 1"
    );
    assert_eq!(
        UpcastCounterV2::PAYLOAD_VERSION,
        2,
        "PROPERTY: #[batpak(version = 2)] sets PAYLOAD_VERSION = 2"
    );
}

#[test]
fn payload_version_does_not_move_content_hash_or_signature() {
    use batpak::store::{Store, StoreConfig};

    // content_hash = blake3(payload bytes); the header payload_version rides
    // OUTSIDE the hashed region (src/store/write/writer/append.rs step 5,
    // src/event/hash.rs) and outside the signature cover (src/store/signing.rs:
    // cover_bytes = event_id + sequence + coord + kind + prev_hash +
    // content_hash + extensions). So a versioned typed append and an unversioned
    // untyped append of the SAME payload bytes must yield identical content_hash
    // AND identical signature.
    let dir = tempfile::TempDir::new().expect("tempdir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");

    let typed_coord =
        batpak::coordinate::Coordinate::new("hash:typed", "scope:test").expect("coord");
    let untyped_coord =
        batpak::coordinate::Coordinate::new("hash:untyped", "scope:test").expect("coord");

    let payload = UpcastCounterV2 {
        total: 7,
        label: "stable".to_owned(),
    };

    // Typed append → header.payload_version = 2.
    let typed = store
        .append_typed(&typed_coord, &payload)
        .expect("typed append");
    // Untyped append of the SAME logical payload → header.payload_version = 0.
    let untyped = store
        .append(&untyped_coord, UpcastCounterV2::KIND, &payload)
        .expect("untyped append");

    // Confirm the version stamping actually differed.
    assert_eq!(
        store
            .get(typed.event_id)
            .expect("get")
            .event
            .header
            .payload_version,
        2
    );
    assert_eq!(
        store
            .get(untyped.event_id)
            .expect("get")
            .event
            .header
            .payload_version,
        0
    );

    // Both are the first (genesis) event on their own entity chain, so prev_hash
    // is all-zero for both → content_hash and signature are directly comparable.
    assert_eq!(
        typed.content_hash, untyped.content_hash,
        "PROPERTY: content_hash covers payload bytes only; stamping payload_version must not move it"
    );
    assert_eq!(
        typed.signature, untyped.signature,
        "PROPERTY: the signature cover excludes payload_version; stamping it must not move the signature"
    );

    store.close().expect("close store");
}

#[test]
fn typed_append_stamps_payload_version_untyped_stamps_zero() {
    use batpak::store::{Store, StoreConfig};
    let dir = tempfile::TempDir::new().expect("tempdir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let coord = batpak::coordinate::Coordinate::new("entity:ver", "scope:test").expect("coord");

    // Typed append stamps T::PAYLOAD_VERSION.
    let typed_receipt = store
        .append_typed(
            &coord,
            &UpcastCounterV2 {
                total: 1,
                label: "v".to_owned(),
            },
        )
        .expect("typed append");
    let typed_event = store.get(typed_receipt.event_id).expect("get typed event");
    assert_eq!(
        typed_event.event.header.payload_version, 2,
        "PROPERTY: append_typed must stamp T::PAYLOAD_VERSION into the header"
    );

    // Untyped append stamps 0 (legacy / app-managed sentinel).
    let untyped_receipt = store
        .append(
            &coord,
            UpcastCounterV2::KIND,
            &serde_json::json!({ "total": 2, "label": "u" }),
        )
        .expect("untyped append");
    let untyped_event = store
        .get(untyped_receipt.event_id)
        .expect("get untyped event");
    assert_eq!(
        untyped_event.event.header.payload_version, 0,
        "PROPERTY: untyped append must leave payload_version = 0 (documented)"
    );

    store.close().expect("close store");
}
