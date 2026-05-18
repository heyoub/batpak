//! PROVES: INV-SYNCBAT-DISPATCH-RECEIPTS, INV-SYNCBAT-REGISTER-CATALOG-DETERMINISTIC
//! CATCHES: MessagePack byte drift in syncbat receipt envelopes and durable catalog rows.
//! SEEDED: deterministic descriptors, fixed hashes, and sorted extension drawers.
#![allow(clippy::panic)]

use syncbat::{
    EffectClass, OperationDescriptor, ReceiptEnvelope, ReceiptOutcome, RegisterOperationRowV1,
};

const RECEIPT_ENVELOPE_COMPLETED_HEX: &str = include_str!("golden/receipt_envelope_completed.hex");
const REGISTER_OPERATION_ROW_PUT_HEX: &str = include_str!("golden/register_operation_row_put.hex");

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn descriptor() -> OperationDescriptor {
    OperationDescriptor::new_with_title(
        "inventory.reserve",
        EffectClass::Persist,
        "schema.inventory.reserve.input.v1",
        "schema.inventory.reserve.output.v1",
        "receipt.inventory.reserve.v1",
        "Reserve Inventory",
    )
}

#[test]
fn receipt_envelope_completed_bytes_are_stable() {
    let envelope = ReceiptEnvelope::new(&descriptor(), ReceiptOutcome::Completed)
        .with_input_hash([0x11; 32])
        .with_output_hash([0x22; 32])
        .with_signed_extension("syncbat.signed.alpha", b"signed-a".to_vec())
        .with_signed_extension("syncbat.signed.beta", b"signed-b".to_vec())
        .with_local_extension("local.alpha", b"local-a".to_vec())
        .with_local_extension("local.beta", b"local-b".to_vec());

    let bytes = batpak::canonical::to_bytes(&envelope).expect("receipt envelope encodes");

    assert_eq!(hex(&bytes), RECEIPT_ENVELOPE_COMPLETED_HEX.trim());
}

#[test]
fn register_operation_put_row_bytes_are_stable() {
    let row = RegisterOperationRowV1::from_descriptor(&descriptor());
    let bytes = batpak::canonical::to_bytes(&row).expect("register row encodes");

    assert_eq!(hex(&bytes), REGISTER_OPERATION_ROW_PUT_HEX.trim());
}
