//! Receipt-verification and read-path unit tests.
//!
//! Extracted from the inline `mod tests` island in `store/read_api.rs` to stay
//! within the inline-test-island budget; behavior is unchanged.

use crate::coordinate::Coordinate;
use crate::event::EventKind;
use crate::store::index::DiskPos;
use crate::store::{ReceiptVerification, ReceiptVerificationError, Store, StoreConfig};
use tempfile::TempDir;

#[test]
fn append_receipt_verification_rejects_disk_position_tampering() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )
    .expect("open store");
    let coord = Coordinate::new("entity:receipt-disk-pos", "scope:test").expect("coord");
    let mut receipt = store
        .append(
            &coord,
            EventKind::custom(0xA, 20),
            &serde_json::json!({"n": 1}),
        )
        .expect("append");

    assert_eq!(
        store.verify_append_receipt(&receipt),
        ReceiptVerification::UnsignedAccepted
    );
    receipt.disk_pos = DiskPos::new(
        receipt.disk_pos.segment_id(),
        receipt.disk_pos.offset() + 1,
        receipt.disk_pos.length(),
    );

    assert_eq!(
        store.verify_append_receipt(&receipt),
        ReceiptVerification::Invalid(ReceiptVerificationError::DiskPositionMismatch),
        "disk position must match the committed index entry"
    );
}

#[test]
fn wire_append_receipt_verification_hydrates_disk_pos_from_index() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )
    .expect("open store");
    let coord = Coordinate::new("entity:wire-verify", "scope:test").expect("coord");
    let receipt = store
        .append(
            &coord,
            EventKind::custom(0xA, 22),
            &serde_json::json!({"n": 1}),
        )
        .expect("append");

    let verification = store.verify_append_receipt_wire_detailed(
        receipt.event_id,
        receipt.global_sequence,
        receipt.content_hash,
        receipt.key_id,
        receipt.signature,
        receipt.extensions.clone(),
    );
    assert_eq!(verification, ReceiptVerification::UnsignedAccepted);
}

mod mismatch_polarity {
    use super::super::{append_receipt_index_mismatch, denial_receipt_index_mismatch};
    use crate::coordinate::Coordinate;
    use crate::event::{EventKind, HashChain};
    use crate::id::EventId;
    use crate::store::index::interner::InternId;
    use crate::store::index::{DiskPos, IndexEntry};
    use crate::store::{AppendReceipt, DenialReceipt, ReceiptVerificationError as Err};
    use std::collections::BTreeMap;

    const EID: u128 = 0x1234;
    const SEQ: u64 = 77;
    const HASH: [u8; 32] = [0xAB; 32];

    fn entry(kind: EventKind) -> IndexEntry {
        IndexEntry {
            event_id: EID,
            correlation_id: EID,
            causation_id: None,
            coord: Coordinate::new("entity:m", "scope:m").expect("coord"),
            entity_id: InternId::sentinel(),
            scope_id: InternId::sentinel(),
            kind,
            wall_ms: 1,
            clock: 1,
            dag_lane: 0,
            dag_depth: 0,
            hash_chain: HashChain {
                prev_hash: [0u8; 32],
                event_hash: HASH,
            },
            disk_pos: DiskPos::new(2, 32, 16),
            global_sequence: SEQ,
            receipt_extensions: BTreeMap::new(),
        }
    }

    fn matching_append() -> AppendReceipt {
        AppendReceipt {
            event_id: EventId::from(EID),
            global_sequence: SEQ,
            disk_pos: DiskPos::new(2, 32, 16),
            content_hash: HASH,
            key_id: [0; 32],
            signature: None,
            extensions: BTreeMap::new(),
        }
    }

    fn matching_denial() -> DenialReceipt {
        DenialReceipt {
            event_id: EventId::from(EID),
            global_sequence: SEQ,
            disk_pos: DiskPos::new(2, 32, 16),
            content_hash: HASH,
            key_id: [0; 32],
            signature: None,
            extensions: BTreeMap::new(),
        }
    }

    #[test]
    fn append_mismatch_returns_none_when_all_fields_agree() {
        assert_eq!(
            append_receipt_index_mismatch(&matching_append(), &entry(EventKind::DATA)),
            None,
            "a fully consistent receipt must NOT be flagged"
        );
    }

    #[test]
    fn append_mismatch_flags_each_field_independently() {
        let e = entry(EventKind::DATA);

        let mut r = matching_append();
        r.event_id = EventId::from(EID + 1);
        assert_eq!(
            append_receipt_index_mismatch(&r, &e),
            Some(Err::EventIdMismatch)
        );

        let mut r = matching_append();
        r.global_sequence = SEQ + 1;
        assert_eq!(
            append_receipt_index_mismatch(&r, &e),
            Some(Err::SequenceMismatch)
        );

        let mut r = matching_append();
        r.disk_pos = DiskPos::new(2, 33, 16);
        assert_eq!(
            append_receipt_index_mismatch(&r, &e),
            Some(Err::DiskPositionMismatch)
        );

        let mut r = matching_append();
        r.content_hash = [0x00; 32];
        assert_eq!(
            append_receipt_index_mismatch(&r, &e),
            Some(Err::ContentHashMismatch)
        );

        let mut r = matching_append();
        r.extensions.insert(
            crate::store::ExtensionKey::new("acme.x").expect("k"),
            vec![1],
        );
        assert_eq!(
            append_receipt_index_mismatch(&r, &e),
            Some(Err::ExtensionsMismatch)
        );
    }

    #[test]
    fn denial_mismatch_requires_the_denial_kind() {
        // Non-denial kind is rejected first, before any field comparison.
        assert_eq!(
            denial_receipt_index_mismatch(&matching_denial(), &entry(EventKind::DATA)),
            Some(Err::DenialKindMismatch),
            "a non-SYSTEM_DENIAL entry must fail the kind guard"
        );
        // With the correct kind and matching fields, no mismatch.
        assert_eq!(
            denial_receipt_index_mismatch(&matching_denial(), &entry(EventKind::SYSTEM_DENIAL)),
            None,
            "matching denial receipt against a denial entry must pass"
        );
    }

    #[test]
    fn denial_mismatch_flags_each_field_independently() {
        let e = entry(EventKind::SYSTEM_DENIAL);

        let mut r = matching_denial();
        r.event_id = EventId::from(EID + 1);
        assert_eq!(
            denial_receipt_index_mismatch(&r, &e),
            Some(Err::EventIdMismatch)
        );

        let mut r = matching_denial();
        r.global_sequence = SEQ + 9;
        assert_eq!(
            denial_receipt_index_mismatch(&r, &e),
            Some(Err::SequenceMismatch)
        );

        let mut r = matching_denial();
        r.disk_pos = DiskPos::new(99, 32, 16);
        assert_eq!(
            denial_receipt_index_mismatch(&r, &e),
            Some(Err::DiskPositionMismatch)
        );

        let mut r = matching_denial();
        r.content_hash = [0x01; 32];
        assert_eq!(
            denial_receipt_index_mismatch(&r, &e),
            Some(Err::ContentHashMismatch)
        );
    }
}
