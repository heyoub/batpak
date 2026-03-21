//! Wire format golden tests.
//! Verifies MessagePack serialization matches known-good byte sequences.
//! [SPEC:tests/wire_format.rs]
//!
//! Anti-almost-correctness: This test would have caught the Arc<str> serialization
//! failure (Phase 1.1) — golden tests serialize a Coordinate containing Arc<str>.
//!
//! Run with GOLDEN_UPDATE=1 cargo test wire_format to regenerate golden files.

use free_batteries::outcome::wait::{CompensationAction, WaitCondition};
use free_batteries::prelude::*;

fn golden_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn check_or_update_golden(name: &str, actual_bytes: &[u8]) {
    let path = golden_dir().join(name);
    let actual_hex = hex_encode(actual_bytes);

    if std::env::var("GOLDEN_UPDATE").is_ok() {
        std::fs::write(&path, &actual_hex)
            .unwrap_or_else(|e| panic!("Failed to write golden file {}: {}", path.display(), e));
        return;
    }

    let expected_hex = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!(
            "Golden file {} not found: {}. Run GOLDEN_UPDATE=1 cargo test wire_format to create it.",
            path.display(), e
        ));

    assert_eq!(
        actual_hex.trim(),
        expected_hex.trim(),
        "WIRE FORMAT DRIFT: {} bytes differ from golden file {}. \
         If this is intentional, run GOLDEN_UPDATE=1 cargo test wire_format. \
         If not, investigate: src/wire.rs and serde derives.",
        name,
        path.display()
    );
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
         Run: GOLDEN_UPDATE=1 cargo test wire_format"
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

// --- Committed<T> golden test ---
// [SPEC:WIRE FORMAT DECISIONS] Committed.event_id uses #[serde(with = "crate::wire::u128_bytes")]

#[test]
fn committed_msgpack_golden() {
    let committed = Committed {
        payload: "test_payload".to_string(),
        event_id: 0x0123456789ABCDEF_0123456789ABCDEF_u128,
        sequence: 42,
        hash: [0xAB; 32],
    };

    let bytes = rmp_serde::to_vec_named(&committed).expect("serialize Committed");

    // Round-trip
    let decoded: Committed<String> = rmp_serde::from_slice(&bytes).expect("deserialize Committed");
    assert_eq!(
        committed.event_id, decoded.event_id,
        "WIRE FORMAT: Committed.event_id round-trip mismatch.\n\
         Investigate: src/pipeline/mod.rs #[serde(with = \"crate::wire::u128_bytes\")].\n\
         Common causes: u128_bytes serialize/deserialize changed.\n\
         Run: GOLDEN_UPDATE=1 cargo test wire_format committed_msgpack_golden"
    );
    assert_eq!(
        committed.payload, decoded.payload,
        "WIRE FORMAT: Committed.payload round-trip mismatch.\n\
         Investigate: src/pipeline/mod.rs Committed<T> serde.\n\
         Common causes: generic T serialization broken.\n\
         Run: GOLDEN_UPDATE=1 cargo test wire_format committed_msgpack_golden"
    );
    assert_eq!(
        committed.sequence, decoded.sequence,
        "WIRE FORMAT: Committed.sequence mismatch."
    );
    assert_eq!(
        committed.hash, decoded.hash,
        "WIRE FORMAT: Committed.hash mismatch."
    );

    check_or_update_golden("committed_v1.hex", &bytes);
}

// --- WaitCondition golden test ---
// [SPEC:WIRE FORMAT DECISIONS] WaitCondition::Event.event_id uses u128_bytes

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
         Run: GOLDEN_UPDATE=1 cargo test wire_format wait_condition_msgpack_golden"
    );

    check_or_update_golden("wait_condition_v1.hex", &bytes);
}

// --- CompensationAction golden test ---
// [SPEC:WIRE FORMAT DECISIONS] CompensationAction::Rollback.event_ids uses vec_u128_bytes

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
         Run: GOLDEN_UPDATE=1 cargo test wire_format compensation_action_msgpack_golden"
    );

    check_or_update_golden("compensation_action_v1.hex", &bytes);
}
