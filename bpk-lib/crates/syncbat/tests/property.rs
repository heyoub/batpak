//! Property-based tests for the syncbat substrate boundaries.
//!
//! Hand-written fixture tests cover the obvious shapes; property
//! tests exercise the surface across arbitrary inputs so we catch
//! shapes the fixtures missed.
//!
//! PROVES:
//!   - OperationName::new accepts the netbat grammar and rejects
//!     anything outside it.
//!   - OperationDescriptor::validate is deterministic across arbitrary
//!     valid name + schema-ref + receipt-kind triples.
//!   - RegisterOperationRowV1 round-trips through canonical
//!     MessagePack: encode then decode equals the original.
//!
//! Property tests run 256 cases by default (see CI's PROPTEST_CASES
//! env var which can lift this floor in stress runs).

use proptest::prelude::*;

use syncbat::{EffectClass, OperationDescriptor, OperationName, RegisterOperationRowV1};

// ─── arbitrary generators ──────────────────────────────────────────────────

/// Generator for OperationName-grammar-valid strings:
/// `[A-Za-z0-9._-]+`, no leading/trailing `.`, no `..`, 1..=128 bytes.
fn arb_operation_name() -> impl Strategy<Value = String> {
    // Build dot-joined segments where each segment is 1..=16 chars of
    // the allowed alphabet (no dots inside the segment). Dot-joining
    // 1..=4 segments keeps the total well under the 128-byte cap and
    // guarantees no leading/trailing dot, no `..`.
    proptest::collection::vec("[A-Za-z0-9_-]{1,16}", 1..=4).prop_map(|segments| segments.join("."))
}

/// Generator for schema-ref / receipt-kind strings using the same
/// stable-token grammar as OperationName.
fn arb_stable_token() -> impl Strategy<Value = String> {
    arb_operation_name()
}

/// Generator for an arbitrary EffectClass discriminant.
fn arb_effect_class() -> impl Strategy<Value = EffectClass> {
    prop_oneof![
        Just(EffectClass::Persist),
        Just(EffectClass::Inspect),
        Just(EffectClass::Compute),
    ]
}

// ─── OperationName grammar ─────────────────────────────────────────────────

proptest! {
    /// Every name produced by arb_operation_name must be accepted by
    /// OperationName::new and round-trip its as_str view unchanged.
    #[test]
    fn operation_name_accepts_grammar_valid_names(name in arb_operation_name()) {
        let parsed = OperationName::new(name.clone())
            .expect("arb_operation_name should produce a grammar-valid name");
        prop_assert_eq!(parsed.as_str(), name.as_str());
    }

    /// Names containing a space, slash, or colon must be rejected at
    /// any position. (The grammar is `[A-Za-z0-9._-]+`.)
    #[test]
    fn operation_name_rejects_illegal_characters_anywhere(
        prefix in "[A-Za-z0-9_-]{1,8}",
        bad_char in prop_oneof![Just(' '), Just('/'), Just(':'), Just('@'), Just('$'), Just('+')],
        suffix in "[A-Za-z0-9_-]{1,8}",
    ) {
        let candidate = format!("{prefix}{bad_char}{suffix}");
        prop_assert!(
            OperationName::new(candidate.clone()).is_err(),
            "OperationName::new must reject illegal character in {candidate:?}",
        );
    }

    /// Names with a `..` substring must be rejected anywhere they appear.
    #[test]
    fn operation_name_rejects_consecutive_dots(
        a in "[A-Za-z0-9_-]{1,8}",
        b in "[A-Za-z0-9_-]{1,8}",
    ) {
        let candidate = format!("{a}..{b}");
        prop_assert!(
            OperationName::new(candidate.clone()).is_err(),
            "OperationName::new must reject consecutive dots in {candidate:?}",
        );
    }

    /// Names longer than the byte cap must be rejected.
    #[test]
    fn operation_name_rejects_overlong_names(len in 129_usize..=256_usize) {
        // Pad with `a` chars — well inside the alphabet, just too long.
        let candidate = "a".repeat(len);
        prop_assert!(
            OperationName::new(candidate.clone()).is_err(),
            "OperationName::new must reject overlong name of {len} bytes",
        );
    }
}

// ─── OperationDescriptor::validate ─────────────────────────────────────────

proptest! {
    /// validate() returns Ok for any descriptor built from grammar-valid
    /// strings via OperationDescriptor::owned, and the produced descriptor
    /// echoes the inputs verbatim.
    #[test]
    fn descriptor_validate_accepts_well_formed_inputs(
        name in arb_operation_name(),
        input_ref in arb_stable_token(),
        output_ref in arb_stable_token(),
        receipt_kind in arb_stable_token(),
        action in arb_effect_class(),
    ) {
        let descriptor = OperationDescriptor::owned(
            name.clone(),
            action,
            input_ref.clone(),
            output_ref.clone(),
            receipt_kind.clone(),
        );
        descriptor
            .validate()
            .expect("well-formed descriptor must validate");
        prop_assert_eq!(descriptor.name(), name.as_str());
        prop_assert_eq!(descriptor.input_schema_ref(), input_ref.as_str());
        prop_assert_eq!(descriptor.output_schema_ref(), output_ref.as_str());
        prop_assert_eq!(descriptor.receipt_kind(), receipt_kind.as_str());
        prop_assert_eq!(descriptor.effect, action);
    }

    /// validate() is idempotent: calling it twice on the same descriptor
    /// returns the same result. Anti-state regression.
    #[test]
    fn descriptor_validate_is_idempotent(
        name in arb_operation_name(),
        input_ref in arb_stable_token(),
        output_ref in arb_stable_token(),
        receipt_kind in arb_stable_token(),
    ) {
        let descriptor = OperationDescriptor::owned(
            name,
            EffectClass::Inspect,
            input_ref,
            output_ref,
            receipt_kind,
        );
        let first = descriptor.validate();
        let second = descriptor.validate();
        prop_assert_eq!(first.is_ok(), second.is_ok());
    }
}

// ─── RegisterOperationRowV1 round-trip ─────────────────────────────────────

proptest! {
    /// Every (action, descriptor) shape used by the register
    /// (from_descriptor/Put, update, delete, supersede) round-trips
    /// byte-for-byte through batpak's canonical MessagePack encoder.
    /// Catches encoder/decoder pair drift on the persisted substrate
    /// register row.
    #[test]
    fn register_operation_row_v1_roundtrips(
        name in arb_operation_name(),
        input_ref in arb_stable_token(),
        output_ref in arb_stable_token(),
        receipt_kind in arb_stable_token(),
        action_disc in 0_u8..=3_u8,
        superseded_name in arb_operation_name(),
    ) {
        let descriptor = OperationDescriptor::owned(
            name.clone(),
            EffectClass::Persist,
            input_ref,
            output_ref,
            receipt_kind,
        );
        let row = match action_disc {
            0 => RegisterOperationRowV1::from_descriptor(&descriptor),
            1 => RegisterOperationRowV1::update(&descriptor),
            2 => RegisterOperationRowV1::delete(name.clone()),
            _ => RegisterOperationRowV1::supersede(superseded_name, &descriptor),
        };
        let bytes = batpak::encoding::to_bytes(&row).expect("encode register row");
        let decoded: RegisterOperationRowV1 =
            batpak::encoding::from_bytes(&bytes).expect("decode register row");
        let re_encoded = batpak::encoding::to_bytes(&decoded).expect("re-encode");
        // Wire-byte identity proves the row's serialization is
        // injection-stable — every shape decodes back to a structurally
        // identical row.
        prop_assert_eq!(bytes, re_encoded);
    }

    /// Encoding the same row twice must produce byte-identical output.
    /// Catches non-determinism in the serializer (e.g. accidental
    /// HashMap iteration leaking into the wire format).
    #[test]
    fn register_operation_row_v1_encoding_is_deterministic(
        name in arb_operation_name(),
        receipt_kind in arb_stable_token(),
    ) {
        let descriptor = OperationDescriptor::owned(
            name,
            EffectClass::Inspect,
            "alpha",
            "beta",
            receipt_kind,
        );
        let row = RegisterOperationRowV1::from_descriptor(&descriptor);
        let a = batpak::encoding::to_bytes(&row).expect("encode 1");
        let b = batpak::encoding::to_bytes(&row).expect("encode 2");
        prop_assert_eq!(a, b);
    }
}
