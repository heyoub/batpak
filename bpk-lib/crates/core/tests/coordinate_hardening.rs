// justifies: INV-TEST-PANIC-AS-ASSERTION; tests in tests/coordinate_hardening.rs rely on expect/panic on unreachable failures; clippy::unwrap_used and clippy::panic are the standard harness allowances for integration tests.
#![allow(clippy::unwrap_used, clippy::panic)]
//! Coordinate hardening coverage.
//!
//! [INV-COORD-HARDEN] `Coordinate::new` rejects every hostile or malformed
//! component (NUL, ASCII control chars, path-traversal) before a Coordinate
//! value ever exists, and every in-process construction path routes through
//! the same validator — including rmp_serde deserialization.

use batpak::coordinate::{Coordinate, CoordinateError};

/// A coordinate-component constructor that would be unsafe if it leaked past
/// the validator: a bare NUL embedded in the entity string. Kept in one place
/// so the failure reason is obvious when an assertion changes.
const MALICIOUS_ENTITY_WITH_NUL: &str = "entity\0hidden";
const MALICIOUS_SCOPE_WITH_NUL: &str = "scope\0hidden";

#[test]
fn rejects_nul_in_entity() {
    let result = Coordinate::new(MALICIOUS_ENTITY_WITH_NUL, "scope");
    assert!(
        matches!(result, Err(CoordinateError::NulByte)),
        "PROPERTY: entity containing a NUL byte must route to CoordinateError::NulByte, got {result:?}"
    );
}

#[test]
fn rejects_nul_in_scope() {
    let result = Coordinate::new("entity", MALICIOUS_SCOPE_WITH_NUL);
    assert!(
        matches!(result, Err(CoordinateError::NulByte)),
        "PROPERTY: scope containing a NUL byte must route to CoordinateError::NulByte, got {result:?}"
    );
}

#[test]
fn rejects_control_chars() {
    // Every byte in 0x00..=0x1F plus DEL (0x7F) must be rejected in either
    // component. NUL is rejected with the more specific NulByte variant; the
    // rest with ControlChar. We assert per-byte to pin span-quality of the
    // rejection and so a regression reports exactly which byte snuck through.
    for byte in 0u8..=0x1F {
        let forbidden = core::str::from_utf8(std::slice::from_ref(&byte))
            .expect("low ASCII bytes are valid UTF-8 singletons");
        let entity_field = format!("ent{forbidden}ity");
        let scope_field = format!("sco{forbidden}pe");

        let expected_variant = if byte == 0 {
            CoordinateError::NulByte
        } else {
            CoordinateError::ControlChar
        };

        let err_entity = Coordinate::new(&entity_field, "scope")
            .expect_err("entity with control byte must be rejected");
        assert_eq!(
            err_entity, expected_variant,
            "PROPERTY: control byte {byte:#04x} in entity must map to {expected_variant:?}, got {err_entity:?}"
        );
        let err_scope = Coordinate::new("entity", &scope_field)
            .expect_err("scope with control byte must be rejected");
        assert_eq!(
            err_scope, expected_variant,
            "PROPERTY: control byte {byte:#04x} in scope must map to {expected_variant:?}, got {err_scope:?}"
        );
    }

    // DEL (0x7F) is the second forbidden region; validated as ControlChar.
    let del = char::from(0x7F).to_string();
    assert_eq!(
        Coordinate::new(format!("ent{del}ity"), "scope").unwrap_err(),
        CoordinateError::ControlChar,
        "PROPERTY: DEL (0x7F) in entity must map to ControlChar"
    );
    assert_eq!(
        Coordinate::new("entity", format!("sco{del}pe")).unwrap_err(),
        CoordinateError::ControlChar,
        "PROPERTY: DEL (0x7F) in scope must map to ControlChar"
    );
}

#[test]
fn rejects_path_traversal() {
    let cases = [
        ("entity/with/slash", "scope"),
        ("entity", "scope/with/slash"),
        ("..", "scope"),
        ("entity", ".."),
        ("entity/..", "scope"),
        ("entity", "../escape"),
    ];

    for (entity, scope) in cases {
        let err = match Coordinate::new(entity, scope) {
            Ok(_) => panic!("must produce an Err for entity={entity:?} scope={scope:?}; got Ok"),
            Err(err) => err,
        };
        assert_eq!(
            err, CoordinateError::PathTraversal,
            "PROPERTY: path-traversal token in entity={entity:?} scope={scope:?} must route to PathTraversal, got {err:?}"
        );
    }
}

#[test]
fn deserialize_malicious_bytes_rejected() {
    // Manually build a msgpack-encoded CoordinateWire whose entity contains a
    // NUL byte. CoordinateWire is crate-private, so we hand-construct the
    // equivalent wire: a map with two string fields "entity" and "scope".
    #[derive(serde::Serialize)]
    struct WireShape<'a> {
        entity: &'a str,
        scope: &'a str,
    }

    let bad_entity = WireShape {
        entity: MALICIOUS_ENTITY_WITH_NUL,
        scope: "scope",
    };
    let bytes = rmp_serde::to_vec_named(&bad_entity).expect("encode malicious wire");

    let result = rmp_serde::from_slice::<Coordinate>(&bytes);
    assert!(
        result.is_err(),
        "PROPERTY: deserializing a Coordinate whose entity carries a NUL byte must fail because \
         rmp_serde routes through Coordinate::new; got Ok({:?})",
        result.ok()
    );

    // Also mirror for a path-traversal wire payload so both axes of the
    // hardened validator are exercised through the serde entry.
    let bad_scope = WireShape {
        entity: "entity",
        scope: "../escape",
    };
    let bytes = rmp_serde::to_vec_named(&bad_scope).expect("encode path-traversal wire");
    let result = rmp_serde::from_slice::<Coordinate>(&bytes);
    assert!(
        result.is_err(),
        "PROPERTY: deserializing a Coordinate with a path-traversal scope must fail through \
         Coordinate::new; got Ok({:?})",
        result.ok()
    );
}

#[test]
fn roundtrip_valid_coord_survives() {
    let coord = Coordinate::new("entity:valid", "scope:valid").expect("valid coord must construct");
    coord
        .validate()
        .expect("public Coordinate::validate must accept a known-good coordinate");
    let bytes = rmp_serde::to_vec_named(&coord).expect("serialize valid coord");
    let decoded: Coordinate =
        rmp_serde::from_slice(&bytes).expect("legitimate coord must decode cleanly");
    decoded
        .validate()
        .expect("decoded valid coordinate must also pass public validation");
    assert_eq!(
        decoded, coord,
        "PROPERTY: a valid Coordinate must survive a msgpack encode+decode round-trip unchanged"
    );
}
