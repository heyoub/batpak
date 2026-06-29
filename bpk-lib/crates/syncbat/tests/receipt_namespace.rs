//! PROVES: INV-SYNCBAT-DISPATCH-RECEIPTS
//! CATCHES: invalid syncbat receipt-extension namespaces and extension value plumbing drift.
//! SEEDED: fixed extension field/value examples.

use batpak::store::{AppendOptions, ExtensionKey};
use syncbat::{receipt_extension_key, receipt_extension_value, SYNCBAT_EXTENSION_NAMESPACE};

#[test]
fn syncbat_receipt_extension_namespace_is_typed_and_validated() {
    assert_eq!(SYNCBAT_EXTENSION_NAMESPACE, "syncbat");

    let key = receipt_extension_key("run").expect("syncbat.run key is valid");
    assert_eq!(key.as_key().as_str(), "syncbat.run");
}

#[test]
fn syncbat_receipt_extension_value_flows_through_batpak_append_options() {
    let key = receipt_extension_key("run").expect("syncbat.run key is valid");
    let value = receipt_extension_value([1_u8, 2, 3]);

    let opts = AppendOptions::new().with_receipt_extension(key, value);
    let raw_key = ExtensionKey::new("syncbat.run").expect("raw key matches typed key");
    assert_eq!(opts.extensions.get(&raw_key), Some(&vec![1, 2, 3]));
}

#[test]
fn syncbat_receipt_extension_key_rejects_nested_fields() {
    assert!(
        receipt_extension_key("run.id").is_err(),
        "PROPERTY: syncbat receipt extension keys reserve dotted nesting for batpak metadata"
    );
}
