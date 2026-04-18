// justifies: wire-format golden tests assert via panic, intentionally group timestamp digits for readability, and emit a stderr warning when GOLDEN_UPDATE rewrites fixtures.
#![allow(
    clippy::panic,
    clippy::inconsistent_digit_grouping,
    clippy::print_stderr
)]
//! Wire format golden tests.
//! Verifies MessagePack serialization matches known-good byte sequences.
//!
//! PROVES: LAW-005 (Architecture Freeze — wire format stability)
//! DEFENDS: FM-010 (Semantic Drift — byte-level determinism prevents silent serde changes)
//! INVARIANTS: INV-TYPE (round-trip fidelity), INV-MIG (backward compatibility)
//!
//! Anti-almost-correctness: This test would have caught the Arc<str> serialization
//! failure (the missing `serde 'rc'` feature flag that broke `Coordinate`
//! deserialization through msgpack — see CHANGELOG for v0.1.x→0.2.x) — golden
//! tests serialize a Coordinate containing Arc<str>.
//!
//! To regenerate golden files, set the sentinel env var EXACTLY as shown — any other
//! value (including "1" or "true") is treated as absent and goldens will NOT be updated:
//!
//!   GOLDEN_UPDATE=I_KNOW_WHAT_IM_DOING cargo test wire_format
//!
//! Inspect `git diff tests/golden/` carefully before committing regenerated goldens.

use batpak::outcome::wait::{CompensationAction, WaitCondition};
use batpak::prelude::*;
use batpak::wire::{option_u128_bytes, u128_bytes, vec_u128_bytes};
use serde::Serialize;

fn golden_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn check_or_update_golden(name: &str, actual_bytes: &[u8]) {
    let path = golden_dir().join(name);
    let actual_hex = hex_encode(actual_bytes);

    // Require the exact sentinel to prevent a stray GOLDEN_UPDATE=1 from silently
    // overwriting golden files. Any value other than "I_KNOW_WHAT_IM_DOING" is ignored.
    let updating = std::env::var("GOLDEN_UPDATE").as_deref() == Ok("I_KNOW_WHAT_IM_DOING");
    if updating {
        eprintln!(
            "⚠ GOLDEN_UPDATE: regenerating golden files in {}. Inspect the diff before committing.",
            golden_dir().display()
        );
        std::fs::write(&path, &actual_hex)
            .unwrap_or_else(|e| panic!("Failed to write golden file {}: {}", path.display(), e));
        return;
    }

    let expected_hex = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!(
            "Golden file {} not found: {}. Run GOLDEN_UPDATE=I_KNOW_WHAT_IM_DOING cargo test wire_format to create it.",
            path.display(), e
        ));

    assert_eq!(
        actual_hex.trim(),
        expected_hex.trim(),
        "WIRE FORMAT DRIFT: {} bytes differ from golden file {}. \
         If this is intentional, run GOLDEN_UPDATE=I_KNOW_WHAT_IM_DOING cargo test wire_format. \
         If not, investigate: src/wire.rs and serde derives.",
        name,
        path.display()
    );
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Serialize)]
struct BatchPayloadShape {
    alpha: u8,
    beta: u16,
}

// --- Coordinate round-trip ---

#[test]
fn coordinate_msgpack_round_trip() {
    let coord = Coordinate::new("entity:test", "scope:test").expect("valid coord");
    let bytes = rmp_serde::to_vec_named(&coord).expect("serialize Coordinate");

    // Round-trip
    let decoded: Coordinate = rmp_serde::from_slice(&bytes).expect("deserialize Coordinate");
    assert_eq!(
        coord, decoded,
        "ROUND-TRIP FAILED: Coordinate serialization/deserialization mismatch. \
         This exercises the Arc<str> serde 'rc' feature (Phase 1.1)."
    );

    check_or_update_golden("coordinate_v1.hex", &bytes);
}

#[test]
fn batch_append_item_uses_named_msgpack_payloads() {
    let coord = Coordinate::new("entity:test", "scope:test").expect("valid coord");
    let payload = BatchPayloadShape { alpha: 7, beta: 42 };
    let options = AppendOptions::default();
    let causation = CausationRef::None;
    let item = BatchAppendItem::new(
        coord,
        EventKind::custom(0xF, 9),
        &payload,
        options,
        causation,
    )
    .expect("serialize batch payload");
    let expected = rmp_serde::to_vec_named(&payload).expect("serialize expected named payload");
    let item_payload_bytes = item.payload_bytes();
    let item_options = item.options();
    let item_causation = item.causation();

    assert_eq!(
        item_payload_bytes,
        expected.as_slice(),
        "WIRE FORMAT: BatchAppendItem payload encoding drifted from named MessagePack.\n\
         Investigate: src/store/append.rs BatchAppendItem::new.\n\
         Common causes: rmp_serde::to_vec used instead of to_vec_named.\n\
         Run: cargo test --test wire_format batch_append_item_uses_named_msgpack_payloads"
    );
    assert_eq!(item_options.expected_sequence, options.expected_sequence);
    assert_eq!(item_options.idempotency_key, options.idempotency_key);
    assert_eq!(item_options.correlation_id, options.correlation_id);
    assert_eq!(item_options.causation_id, options.causation_id);
    assert_eq!(item_options.position_hint, options.position_hint);
    assert_eq!(item_options.flags, options.flags);
    assert_eq!(item_causation, causation);
}

#[test]
fn batch_append_item_from_msgpack_bytes_preserves_raw_payload() {
    let coord = Coordinate::new("entity:test", "scope:test").expect("valid coord");
    let payload = BatchPayloadShape { alpha: 1, beta: 2 };
    let encoded = rmp_serde::to_vec_named(&payload).expect("encode named payload");
    let options = AppendOptions::default();
    let causation = CausationRef::None;
    let item = BatchAppendItem::from_msgpack_bytes(
        coord.clone(),
        EventKind::custom(0xF, 7),
        encoded.clone(),
        options,
        causation,
    );
    let item_coord = item.coord();
    let item_kind = item.kind();
    let item_payload_bytes = item.payload_bytes();
    let item_options = item.options();
    let item_causation = item.causation();

    assert_eq!(item_coord, &coord);
    assert_eq!(item_kind, EventKind::custom(0xF, 7));
    assert_eq!(item_payload_bytes, encoded.as_slice());
    assert_eq!(item_options.expected_sequence, options.expected_sequence);
    assert_eq!(item_options.idempotency_key, options.idempotency_key);
    assert_eq!(item_options.correlation_id, options.correlation_id);
    assert_eq!(item_options.causation_id, options.causation_id);
    assert_eq!(item_options.position_hint, options.position_hint);
    assert_eq!(item_options.flags, options.flags);
    assert_eq!(item_causation, causation);
}

// --- EventHeader ---

#[test]
fn event_header_msgpack_golden() {
    let header = EventHeader::new(
        0x0123456789ABCDEF_0123456789ABCDEF_u128, // event_id
        0x0123456789ABCDEF_0123456789ABCDEF_u128, // correlation_id
        None,                                     // causation_id
        1700000000_000000_i64,                    // timestamp_us
        DagPosition::root(),
        42, // payload_size
        EventKind::custom(0xF, 1),
    );

    let bytes = rmp_serde::to_vec_named(&header).expect("serialize EventHeader");

    // Round-trip
    let decoded: EventHeader = rmp_serde::from_slice(&bytes).expect("deserialize EventHeader");
    assert_eq!(
        header, decoded,
        "WIRE FORMAT: EventHeader round-trip mismatch.\n\
         Investigate: src/event/header.rs serde derives, src/wire.rs u128_bytes.\n\
         Common causes: u128 serialization changed, field added/removed.\n\
         Run: GOLDEN_UPDATE=I_KNOW_WHAT_IM_DOING cargo test wire_format"
    );

    check_or_update_golden("event_header_v1.hex", &bytes);
}

// --- EventKind encoding ---

#[test]
fn event_kind_category_type_encoding() {
    let kind = EventKind::custom(0xF, 0xABC);
    assert_eq!(
        kind.category(),
        0xF,
        "WIRE FORMAT: EventKind category extraction wrong.\n\
         Investigate: src/event/kind.rs category() bit shifting.\n\
         Common causes: bit mask/shift direction changed.\n\
         Run: cargo test --test wire_format event_kind_category_type_encoding"
    );
    assert_eq!(
        kind.type_id(),
        0xABC,
        "WIRE FORMAT: EventKind type_id extraction wrong.\n\
         Investigate: src/event/kind.rs type_id() bit masking.\n\
         Common causes: mask width changed.\n\
         Run: cargo test --test wire_format event_kind_category_type_encoding"
    );

    // Verify round-trip through serde
    let bytes = rmp_serde::to_vec_named(&kind).expect("serialize EventKind");
    let decoded: EventKind = rmp_serde::from_slice(&bytes).expect("deserialize EventKind");
    assert_eq!(
        kind, decoded,
        "WIRE FORMAT: EventKind serde round-trip mismatch.\n\
         Investigate: src/event/kind.rs serde derives.\n\
         Common causes: internal u16 representation changed.\n\
         Run: cargo test --test wire_format event_kind_category_type_encoding"
    );
}

// --- Outcome serialization ---

#[test]
fn outcome_ok_round_trip() {
    let outcome: Outcome<i32> = Outcome::Ok(42);
    let json = serde_json::to_string(&outcome).expect("serialize Outcome");
    let decoded: Outcome<i32> = serde_json::from_str(&json).expect("deserialize Outcome");
    assert_eq!(
        outcome, decoded,
        "WIRE FORMAT: Outcome::Ok JSON round-trip mismatch.\n\
         Investigate: src/outcome/mod.rs serde adjacent tagging.\n\
         Common causes: tag/content attribute changed.\n\
         Run: cargo test --test wire_format outcome_ok_round_trip"
    );
}

#[test]
fn outcome_err_round_trip() {
    let outcome: Outcome<i32> = Outcome::Err(OutcomeError {
        kind: ErrorKind::NotFound,
        message: "not found".into(),
        compensation: None,
        retryable: false,
    });
    let json = serde_json::to_string(&outcome).expect("serialize");
    let decoded: Outcome<i32> = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(
        outcome, decoded,
        "WIRE FORMAT: Outcome::Err JSON round-trip mismatch.\n\
         Investigate: src/outcome/mod.rs, src/outcome/error.rs serde derives.\n\
         Common causes: OutcomeError field added without serde default.\n\
         Run: cargo test --test wire_format outcome_err_round_trip"
    );
}

#[test]
fn outcome_batch_round_trip() {
    let outcome: Outcome<i32> = Outcome::Batch(vec![
        Outcome::Ok(1),
        Outcome::Ok(2),
        Outcome::Err(OutcomeError {
            kind: ErrorKind::Internal,
            message: "fail".into(),
            compensation: None,
            retryable: true,
        }),
    ]);
    let json = serde_json::to_string(&outcome).expect("serialize");
    let decoded: Outcome<i32> = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(
        outcome, decoded,
        "WIRE FORMAT: Outcome::Batch JSON round-trip mismatch.\n\
         Investigate: src/outcome/mod.rs Batch variant serde.\n\
         Common causes: recursive Batch serialization broken.\n\
         Run: cargo test --test wire_format outcome_batch_round_trip"
    );
}

#[test]
fn committed_api_contract() {
    struct TestBypassReason;

    impl batpak::pipeline::BypassReason for TestBypassReason {
        fn name(&self) -> &'static str {
            "wire-format-test"
        }

        fn justification(&self) -> &'static str {
            "exercise committed proof through the public pipeline surface"
        }
    }

    let event_id: u128 = 0x0102_0304_0506_0708_090A_0B0C_0D0E_0F10;
    let sequence: u64 = 42;
    let mut hash = [0u8; 32];
    hash[0] = 0xAB;
    hash[31] = 0xCD;

    let meta = match CommitMetadata::new(event_id, sequence, hash) {
        Ok(meta) => meta,
        Err(err) => panic!("test constructs known-valid commit metadata: {err:?}"),
    };
    assert_eq!(meta.event_id(), event_id);
    assert_eq!(meta.sequence(), sequence);
    assert_eq!(meta.hash(), hash);

    let committed: Committed<&'static str> = Pipeline::<()>::commit_bypass(
        Pipeline::<()>::bypass(Proposal::new("payload"), &TestBypassReason),
        |_| Ok::<_, StoreError>(meta),
    )
    .expect("public commit path should construct Committed");
    let payload = committed.payload();
    let committed_event_id = committed.event_id();
    let committed_sequence = committed.sequence();
    let committed_hash = committed.hash();
    assert_eq!(payload, &"payload");
    assert_eq!(committed_event_id, event_id);
    assert_eq!(committed_sequence, sequence);
    assert_eq!(committed_hash, &hash);

    let (payload, meta2) = committed.into_parts();
    assert_eq!(payload, "payload");
    let meta2_event_id = meta2.event_id();
    let meta2_sequence = meta2.sequence();
    let meta2_hash = meta2.hash();
    assert_eq!(meta2_event_id, event_id);
    assert_eq!(meta2_sequence, sequence);
    assert_eq!(meta2_hash, hash);
}

#[test]
fn commit_metadata_from_append_receipt_zeroes_hash() {
    let receipt = batpak::store::AppendReceipt {
        event_id: 0xDEAD,
        sequence: 7,
        disk_pos: batpak::store::DiskPos::new(3, 128, 64),
    };

    let meta = match CommitMetadata::from_append_receipt(receipt) {
        Ok(meta) => meta,
        Err(err) => panic!("test constructs known-valid receipt metadata: {err:?}"),
    };
    assert_eq!(meta.event_id(), 0xDEAD);
    assert_eq!(meta.sequence(), 7);
    assert_eq!(
        meta.hash(),
        [0u8; 32],
        "CommitMetadata::from_append_receipt must not fabricate a content hash",
    );
}

#[test]
fn commit_metadata_genesis_reserves_zero_sequence_legally() {
    let hash = [0x11; 32];
    let meta = CommitMetadata::genesis(0xBEEF, hash);
    let event_id = meta.event_id();
    let sequence = meta.sequence();
    let returned_hash = meta.hash();
    let is_genesis = meta.is_genesis();

    assert_eq!(event_id, 0xBEEF);
    assert_eq!(sequence, 0);
    assert_eq!(returned_hash, hash);
    assert!(is_genesis, "genesis metadata must mark itself as genesis");
    meta.validate().expect("genesis metadata should validate");
}

#[test]
fn u128_bytes_round_trips_through_direct_helper_calls() {
    let value = 0xDEADBEEFCAFEBABE_1234567890ABCDEF_u128;
    let mut encoded = Vec::new();
    let mut serializer = rmp_serde::encode::Serializer::new(&mut encoded);
    u128_bytes::serialize(&value, &mut serializer).expect("serialize u128 helper");

    let mut deserializer = rmp_serde::decode::Deserializer::new(&encoded[..]);
    let decoded = u128_bytes::deserialize(&mut deserializer).expect("deserialize u128 helper");
    assert_eq!(decoded, value);
}

#[test]
fn option_u128_bytes_round_trips_some_and_none_through_direct_helper_calls() {
    let some = Some(0xAABBCCDDEEFF0011_2233445566778899_u128);
    let mut encoded_some = Vec::new();
    let mut serializer_some = rmp_serde::encode::Serializer::new(&mut encoded_some);
    option_u128_bytes::serialize(&some, &mut serializer_some)
        .expect("serialize option<u128> helper");
    let mut deserializer_some = rmp_serde::decode::Deserializer::new(&encoded_some[..]);
    let decoded_some = option_u128_bytes::deserialize(&mut deserializer_some)
        .expect("deserialize option<u128> helper");
    assert_eq!(decoded_some, some);

    let none: Option<u128> = None;
    let mut encoded_none = Vec::new();
    let mut serializer_none = rmp_serde::encode::Serializer::new(&mut encoded_none);
    option_u128_bytes::serialize(&none, &mut serializer_none)
        .expect("serialize none option<u128> helper");
    let mut deserializer_none = rmp_serde::decode::Deserializer::new(&encoded_none[..]);
    let decoded_none = option_u128_bytes::deserialize(&mut deserializer_none)
        .expect("deserialize none option<u128> helper");
    assert_eq!(decoded_none, none);
}

#[test]
fn vec_u128_bytes_round_trips_through_direct_helper_calls() {
    let values = vec![
        0x1111111111111111_2222222222222222_u128,
        0x3333333333333333_4444444444444444_u128,
        0x5555555555555555_6666666666666666_u128,
    ];
    let mut encoded = Vec::new();
    let mut serializer = rmp_serde::encode::Serializer::new(&mut encoded);
    vec_u128_bytes::serialize(&values, &mut serializer).expect("serialize vec<u128> helper");

    let mut deserializer = rmp_serde::decode::Deserializer::new(&encoded[..]);
    let decoded =
        vec_u128_bytes::deserialize(&mut deserializer).expect("deserialize vec<u128> helper");
    assert_eq!(decoded, values);
}

// --- WaitCondition golden test ---

#[test]
fn wait_condition_msgpack_golden() {
    let condition = WaitCondition::Event {
        event_id: 0xDEADBEEFCAFEBABE_1234567890ABCDEF_u128,
    };

    let bytes = rmp_serde::to_vec_named(&condition).expect("serialize WaitCondition");

    // Round-trip
    let decoded: WaitCondition = rmp_serde::from_slice(&bytes).expect("deserialize WaitCondition");
    assert_eq!(
        condition, decoded,
        "WIRE FORMAT: WaitCondition::Event round-trip mismatch.\n\
         Investigate: src/outcome/wait.rs #[serde(with = \"crate::wire::u128_bytes\")].\n\
         Common causes: u128_bytes serde helper changed.\n\
         Run: GOLDEN_UPDATE=I_KNOW_WHAT_IM_DOING cargo test wire_format wait_condition_msgpack_golden"
    );

    check_or_update_golden("wait_condition_v1.hex", &bytes);
}

// --- CompensationAction golden test ---

#[test]
fn compensation_action_msgpack_golden() {
    let action = CompensationAction::Rollback {
        event_ids: vec![
            0x1111111111111111_2222222222222222_u128,
            0x3333333333333333_4444444444444444_u128,
        ],
    };

    let bytes = rmp_serde::to_vec_named(&action).expect("serialize CompensationAction");

    // Round-trip
    let decoded: CompensationAction =
        rmp_serde::from_slice(&bytes).expect("deserialize CompensationAction");
    assert_eq!(
        action, decoded,
        "WIRE FORMAT: CompensationAction::Rollback round-trip mismatch.\n\
         Investigate: src/outcome/wait.rs #[serde(with = \"crate::wire::vec_u128_bytes\")].\n\
         Common causes: vec_u128_bytes helper serialize/deserialize changed.\n\
         Run: GOLDEN_UPDATE=I_KNOW_WHAT_IM_DOING cargo test wire_format compensation_action_msgpack_golden"
    );

    check_or_update_golden("compensation_action_v1.hex", &bytes);
}
