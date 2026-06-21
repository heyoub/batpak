//! Append-path unit tests for `AppendOptions`, `CausationRef`, `ExtensionKey`,
//! and the receipt-extension length accounting.
//!
//! Extracted from the inline `mod tests` island in `store/append.rs` to stay
//! within the inline-test-island budget; behavior is unchanged.

use super::*;

#[test]
fn with_causation_zero_is_noop() {
    let opts = AppendOptions::default().with_causation(CausationId::from(0u128));
    assert_eq!(
        opts.causation_id, None,
        "0 is the wire sentinel — must not become Some(0)"
    );
}

#[test]
fn with_causation_nonzero_is_recorded() {
    let opts = AppendOptions::default().with_causation(CausationId::from(42u128));
    assert_eq!(opts.causation_id, Some(CausationId::from(42u128)));
}

#[test]
fn causation_ref_absolute_zero_resolves_to_none() {
    let result = CausationRef::Absolute(0).resolve(None, 0, |_| unreachable!());
    assert_eq!(
        result.expect("resolve must not error"),
        None,
        "Absolute(0) must resolve to None"
    );
}

#[test]
fn causation_ref_absolute_nonzero_resolves_to_some() {
    let result = CausationRef::Absolute(99).resolve(None, 0, |_| unreachable!());
    assert_eq!(result.expect("resolve must not error"), Some(99));
}

#[test]
fn extension_key_reserved_constructor_allows_batpak_namespace() {
    let key = ExtensionKey::reserved("batpak.signing.downgrade");
    assert_eq!(key.as_str(), "batpak.signing.downgrade");
}

#[test]
fn extension_key_rejects_keys_over_max_length() {
    let too_long = format!("acme.{}", "a".repeat(252));
    assert_eq!(ExtensionKey::new(too_long), Err(ExtensionKeyError::TooLong));
}

#[test]
fn extension_key_error_preserves_display_and_error_trait() {
    fn assert_error_trait<E: std::error::Error>() {}

    assert_error_trait::<ExtensionKeyError>();
    assert_eq!(
        ExtensionKeyError::InvalidNamespaceFormat.to_string(),
        "extension key must have exactly one non-empty namespace separator"
    );
}

#[test]
fn uses_options_fallback_only_for_none_variant() {
    // A `match self { None => true, _ => false }` mutated to `true`/`false`
    // would break exactly one of these.
    assert!(
        CausationRef::None.uses_options_fallback(),
        "None must defer to the options causation field"
    );
    assert!(
        !CausationRef::Absolute(7).uses_options_fallback(),
        "Absolute carries its own causation; no fallback"
    );
    assert!(
        !CausationRef::PriorItem(0).uses_options_fallback(),
        "PriorItem carries its own causation; no fallback"
    );
}

#[test]
fn causation_ref_none_resolves_to_the_fallback() {
    // None returns the fallback verbatim, not a substituted value.
    assert_eq!(
        CausationRef::None
            .resolve(Some(55), 3, |_| unreachable!())
            .expect("resolve"),
        Some(55),
        "None must thread the fallback through unchanged"
    );
    assert_eq!(
        CausationRef::None
            .resolve(None, 3, |_| unreachable!())
            .expect("resolve"),
        None,
        "None with no fallback resolves to None"
    );
}

#[test]
fn causation_ref_prior_item_resolves_to_referenced_event_id() {
    // Valid: prior_idx (1) < item_index (2). The closure result is returned.
    let resolved = CausationRef::PriorItem(1)
        .resolve(None, 2, |idx| {
            assert_eq!(idx, 1, "must call the closure with the referenced index");
            0xABCD
        })
        .expect("resolve must succeed for an earlier reference");
    assert_eq!(resolved, Some(0xABCD));
}

#[test]
fn causation_ref_prior_item_rejects_self_or_forward_reference() {
    // prior_idx == item_index is a self-reference; the `>=` bound must reject it.
    let same = CausationRef::PriorItem(2).resolve(None, 2, |_| unreachable!());
    assert!(
        matches!(same, Err(StoreError::InvalidCausation { .. })),
        "PriorItem must not reference itself (>= bound)"
    );
    // prior_idx > item_index is a forward reference; also rejected.
    let forward = CausationRef::PriorItem(5).resolve(None, 2, |_| unreachable!());
    assert!(
        matches!(forward, Err(StoreError::InvalidCausation { .. })),
        "PriorItem must not reference a later item"
    );
}

#[test]
fn encoded_receipt_extensions_len_is_zero_for_empty_and_positive_otherwise() {
    let empty = BTreeMap::new();
    assert_eq!(
        encoded_receipt_extensions_len(&empty).expect("len"),
        0,
        "empty extensions encode to a zero-length budget (early return)"
    );

    let mut populated = BTreeMap::new();
    populated.insert(ExtensionKey::new("acme.k").expect("key"), vec![1u8, 2, 3]);
    let len = encoded_receipt_extensions_len(&populated).expect("len");
    assert!(
        len > 0,
        "non-empty extensions must report a positive encoded length, got {len}"
    );
}

#[test]
fn checked_append_bytes_sums_payload_and_extension_lengths() {
    let empty = BTreeMap::new();
    assert_eq!(
        checked_append_bytes(64, &empty).expect("bytes"),
        64,
        "with no extensions the total equals the payload length"
    );

    let mut populated = BTreeMap::new();
    populated.insert(ExtensionKey::new("acme.k").expect("key"), vec![9u8; 10]);
    let ext_len = encoded_receipt_extensions_len(&populated).expect("len");
    assert_eq!(
        checked_append_bytes(64, &populated).expect("bytes"),
        64 + ext_len,
        "total must be payload_len + extension_len, not just one term"
    );
}

#[test]
fn checked_payload_len_accepts_small_and_returns_exact_length() {
    let bytes = vec![0u8; 5];
    assert_eq!(
        checked_payload_len(&bytes).expect("len"),
        5,
        "must return the exact byte length"
    );
    assert_eq!(checked_payload_len(&[]).expect("len"), 0);
}

#[test]
fn extension_key_validation_rejects_each_bad_shape() {
    assert_eq!(ExtensionKey::new(""), Err(ExtensionKeyError::Empty));
    assert_eq!(
        ExtensionKey::new("café.key"),
        Err(ExtensionKeyError::NonAscii)
    );
    assert_eq!(
        ExtensionKey::new("nodot"),
        Err(ExtensionKeyError::InvalidNamespaceFormat),
        "missing separator must be rejected"
    );
    assert_eq!(
        ExtensionKey::new(".field"),
        Err(ExtensionKeyError::InvalidNamespaceFormat),
        "empty prefix must be rejected"
    );
    assert_eq!(
        ExtensionKey::new("prefix."),
        Err(ExtensionKeyError::InvalidNamespaceFormat),
        "empty field must be rejected"
    );
    assert_eq!(
        ExtensionKey::new("a.b.c"),
        Err(ExtensionKeyError::InvalidNamespaceFormat),
        "more than one separator must be rejected"
    );
    assert_eq!(
        ExtensionKey::new("batpak.reserved"),
        Err(ExtensionKeyError::ReservedNamespace),
        "the batpak namespace is reserved"
    );
    // The happy path must still succeed and round-trip the string.
    let ok = ExtensionKey::new("acme.thing").expect("valid key");
    assert_eq!(ok.as_str(), "acme.thing");
}

#[test]
fn batch_append_item_accessors_return_constructed_values() {
    let coord = Coordinate::new("entity:bi", "scope:bi").expect("coord");
    let kind = EventKind::custom(0xB, 3);
    let item = BatchAppendItem::from_msgpack_bytes(
        coord.clone(),
        kind,
        vec![7u8, 8, 9],
        AppendOptions::default(),
        CausationRef::Absolute(123),
    );
    assert_eq!(item.coord().entity(), coord.entity(), "coord accessor");
    assert_eq!(item.kind(), kind, "kind accessor");
    assert_eq!(item.payload_bytes(), &[7u8, 8, 9], "payload_bytes accessor");
    assert_eq!(item.causation(), CausationRef::Absolute(123), "causation");
}

#[test]
fn append_position_hint_new_assigns_lane_and_depth_in_order() {
    // A lane<->depth swap would flip these.
    let hint = AppendPositionHint::new(4, 9);
    assert_eq!(hint.lane, 4, "first arg is lane");
    assert_eq!(hint.depth, 9, "second arg is depth");
}

#[test]
fn signing_downgrade_extension_key_is_the_reserved_namespace() {
    assert_eq!(
        signing_downgrade_extension_key().as_str(),
        "batpak.signing.downgrade"
    );
}
