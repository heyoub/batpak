//! Wire format golden tests.
//! Verifies MessagePack serialization matches known-good byte sequences.
//! [SPEC:tests/wire_format.rs]
//!
//! Anti-almost-correctness: This test would have caught the Arc<str> serialization
//! failure (Phase 1.1) — golden tests serialize a Coordinate containing Arc<str>.
//!
//! Run with GOLDEN_UPDATE=1 cargo test wire_format to regenerate golden files.

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
    assert_eq!(header, decoded);

    check_or_update_golden("event_header_v1.hex", &bytes);
}

// --- EventKind encoding ---

#[test]
fn event_kind_category_type_encoding() {
    let kind = EventKind::custom(0xF, 0xABC);
    assert_eq!(kind.category(), 0xF);
    assert_eq!(kind.type_id(), 0xABC);

    // Verify round-trip through serde
    let bytes = rmp_serde::to_vec_named(&kind).expect("serialize EventKind");
    let decoded: EventKind = rmp_serde::from_slice(&bytes).expect("deserialize EventKind");
    assert_eq!(kind, decoded);
}

// --- Outcome serialization ---

#[test]
fn outcome_ok_round_trip() {
    let outcome: Outcome<i32> = Outcome::Ok(42);
    let json = serde_json::to_string(&outcome).expect("serialize Outcome");
    let decoded: Outcome<i32> = serde_json::from_str(&json).expect("deserialize Outcome");
    assert_eq!(outcome, decoded);
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
    assert_eq!(outcome, decoded);
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
    assert_eq!(outcome, decoded);
}
