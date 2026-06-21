//! Store isomorphism property laws.
//!
//! PROVES: INV-STORE-ISOMORPHISM-LAWS and INV-IMPORT-CONTENT-ISOMORPHISM.
//! Durable identity seams round-trip generated values or preserve raw bytes:
//! DagPosition MessagePack, raw append payload bytes, and content hashes.
//! CATCHES: accidental schema drift in DagPosition encoding and any raw import
//! path that re-serializes or mutates payload bytes before hashing.
//! SEEDED: proptest-generated HLC/lane/depth fields, bounded JSON payloads,
//! tempfile-backed stores, fixed coordinates and EventKinds.

mod support;
use batpak::coordinate::DagPosition;
use batpak::event::hash::compute_hash;
use batpak::store::{AppendOptions, BatchAppendItem, CausationRef, Store, StoreConfig};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use serde_json::{Map, Value};
use support::prelude::*;
use tempfile::TempDir;

#[path = "common/proptest.rs"]
mod proptest_support;

fn arb_json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| Value::Number(n.into())),
        "[a-zA-Z0-9 _:-]{0,24}".prop_map(Value::String),
    ];

    leaf.prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            proptest::collection::btree_map("[a-zA-Z0-9_:-]{1,12}", inner, 0..4).prop_map(
                |items| {
                    let mut map = Map::new();
                    for (key, value) in items {
                        map.insert(key, value);
                    }
                    Value::Object(map)
                }
            ),
        ]
    })
}

fn prop_result<T, E: std::fmt::Display>(
    result: Result<T, E>,
    context: &'static str,
) -> Result<T, TestCaseError> {
    result.map_err(|err| TestCaseError::fail(format!("{context}: {err}")))
}

proptest! {
    #![proptest_config(proptest_support::cfg(256))]

    #[test]
    fn dag_position_msgpack_roundtrip_is_identity(
        wall_ms in any::<u64>(),
        counter in any::<u16>(),
        depth in any::<u32>(),
        lane in any::<u32>(),
        sequence in any::<u32>(),
    ) {
        let original = DagPosition::with_hlc(wall_ms, counter, depth, lane, sequence);
        let bytes = prop_result(batpak::encoding::to_bytes(&original), "encode DagPosition")?;
        let decoded: DagPosition =
            prop_result(batpak::encoding::from_bytes(&bytes), "decode DagPosition")?;
        prop_assert_eq!(decoded, original);
    }

    #[test]
    fn raw_msgpack_append_preserves_payload_bytes_and_content_hash(value in arb_json_value()) {
        let bytes = prop_result(batpak::encoding::to_bytes(&value), "encode generated json")?;
        let dir = prop_result(TempDir::new(), "temp dir")?;
        let store = Store::open(
            StoreConfig::new(dir.path())
                .with_enable_checkpoint(false)
                .with_enable_mmap_index(false),
        );
        let store = prop_result(store, "open store")?;
        let coord = prop_result(
            Coordinate::new("entity:isomorphism:payload", "scope:isomorphism"),
            "coord",
        )?;
        let kind = EventKind::custom(0xF, 0x91);
        let item = BatchAppendItem::from_msgpack_bytes(
            coord,
            kind,
            bytes.clone(),
            AppendOptions::default(),
            CausationRef::None,
        );

        let receipts = prop_result(store.append_batch(vec![item]), "append raw batch")?;
        prop_assert_eq!(receipts.len(), 1);
        let receipt = &receipts[0];
        prop_assert_eq!(receipt.content_hash, compute_hash(&bytes));
        let raw = prop_result(store.read_raw(receipt.event_id), "read raw")?;
        prop_assert_eq!(raw.event.payload, bytes);
        prop_assert_eq!(raw.event.header.content_hash, receipt.content_hash);
        prop_result(store.close(), "close store")?;
    }
}
