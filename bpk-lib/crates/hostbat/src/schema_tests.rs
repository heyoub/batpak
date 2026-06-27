//! Unit + immutability-gate tests for the D7 schema-identity model.
//!
//! These prove the load-bearing properties of [`SchemaDescriptor`]:
//! - identity is `(id, version, role)` + the canonical encoding;
//! - the canonical encoding is deterministic and content-addressed;
//! - changing a golden vector at a fixed `(id, version)` changes the encoding
//!   (the immutability gate fires — you must bump the version);
//! - [`DiagnosticRustType`] is non-load-bearing (it never touches identity);
//! - golden vectors round-trip (a committed encoding is reproducible).
//!
//! `panic!` is denied even in tests; failures are collected and asserted.

use super::*;

use crate::{RecordField, RecordShape, RefShape, ScalarKind, ScalarShape, SchemaShape};

fn id(s: &str) -> SchemaId {
    SchemaId::new(s).expect("valid schema id")
}

fn descriptor(
    id_str: &str,
    version: u32,
    role: SchemaRole,
    golden: Vec<GoldenVector>,
) -> SchemaDescriptor {
    SchemaDescriptor::new(id(id_str), SchemaVersion(version), role, golden).expect("descriptor")
}

fn canonical_fixture(value: &str) -> Vec<u8> {
    batpak::canonical::to_bytes(&value).expect("fixture encodes")
}

fn invalid_canonical_fixture() -> Vec<u8> {
    vec![0xc1]
}

// ---- schema id grammar --------------------------------------------------

#[test]
fn schema_id_rejects_bad_grammar() {
    let cases = [
        ("", "empty"),
        (".lead", "leading dot"),
        ("trail.", "trailing dot"),
        ("doubl..ed", "doubled dot"),
        ("Upper", "uppercase"),
        ("has space", "space"),
        ("bad/slash", "slash"),
    ];
    let mut failures = Vec::new();
    for (candidate, why) in cases {
        if SchemaId::new(candidate).is_ok() {
            failures.push(format!(
                "{candidate:?} ({why}) was accepted but should be rejected"
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn schema_id_accepts_namespaced_names() {
    let mut failures = Vec::new();
    for good in ["hostbat.op.echo.in", "hostbat.event.audit", "a", "a-b_c.0"] {
        if SchemaId::new(good).is_err() {
            failures.push(format!("{good:?} should be a valid schema id"));
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

// ---- encoding determinism + content addressing --------------------------

#[test]
fn encoding_is_deterministic_for_identical_shape() {
    let a = descriptor(
        "hostbat.op.echo.in",
        1,
        SchemaRole::OperationInput,
        vec![GoldenVector::new("empty", b"\x90".to_vec())],
    );
    let b = descriptor(
        "hostbat.op.echo.in",
        1,
        SchemaRole::OperationInput,
        vec![GoldenVector::new("empty", b"\x90".to_vec())],
    );
    assert_eq!(
        a.encoding(),
        b.encoding(),
        "identical declared shape ⇒ identical canonical encoding",
    );
    assert!(a.verify_encoding().expect("verify"));
}

#[test]
fn golden_vector_order_does_not_affect_encoding() {
    let forward = descriptor(
        "hostbat.op.x.in",
        1,
        SchemaRole::OperationInput,
        vec![
            GoldenVector::new("alpha", b"a".to_vec()),
            GoldenVector::new("beta", b"b".to_vec()),
        ],
    );
    let reverse = descriptor(
        "hostbat.op.x.in",
        1,
        SchemaRole::OperationInput,
        vec![
            GoldenVector::new("beta", b"b".to_vec()),
            GoldenVector::new("alpha", b"a".to_vec()),
        ],
    );
    assert_eq!(
        forward.encoding(),
        reverse.encoding(),
        "golden vectors are canonically sorted; declaration order is irrelevant",
    );
}

#[test]
fn encoding_distinguishes_each_identity_axis() {
    let base = descriptor(
        "hostbat.op.echo.in",
        1,
        SchemaRole::OperationInput,
        vec![GoldenVector::new("c", b"x".to_vec())],
    );
    let diff_id = descriptor(
        "hostbat.op.echo.out",
        1,
        SchemaRole::OperationInput,
        vec![GoldenVector::new("c", b"x".to_vec())],
    );
    let diff_version = descriptor(
        "hostbat.op.echo.in",
        2,
        SchemaRole::OperationInput,
        vec![GoldenVector::new("c", b"x".to_vec())],
    );
    let diff_role = descriptor(
        "hostbat.op.echo.in",
        1,
        SchemaRole::OperationOutput,
        vec![GoldenVector::new("c", b"x".to_vec())],
    );
    let mut failures = Vec::new();
    if base.encoding() == diff_id.encoding() {
        failures.push("id change did not change the encoding");
    }
    if base.encoding() == diff_version.encoding() {
        failures.push("version change did not change the encoding");
    }
    if base.encoding() == diff_role.encoding() {
        failures.push("role change did not change the encoding");
    }
    assert!(failures.is_empty(), "{failures:?}");
}

// ---- THE IMMUTABILITY GATE (the teeth) ----------------------------------

/// Changing a golden vector at a FIXED `(id, version)` changes the canonical
/// encoding. The bytes are not allowed to silently drift: a consumer pinning the
/// old encoding would see a mismatch and refuse. The fix is to bump the version.
#[test]
fn changing_bytes_at_a_fixed_version_changes_the_encoding() {
    let v1 = descriptor(
        "hostbat.op.echo.in",
        1,
        SchemaRole::OperationInput,
        vec![GoldenVector::new("case", b"old-bytes".to_vec())],
    );
    let v1_changed = descriptor(
        "hostbat.op.echo.in",
        1,
        SchemaRole::OperationInput,
        vec![GoldenVector::new("case", b"new-bytes".to_vec())],
    );
    assert_ne!(
        v1.encoding(),
        v1_changed.encoding(),
        "a byte change at the same (id, version) must change the encoding — the immutability gate",
    );
}

/// Bumping the version is the sanctioned way to change the bytes: the new
/// `(id, version)` is a distinct identity with its own encoding.
#[test]
fn bumping_the_version_is_a_distinct_identity() {
    let v1 = descriptor(
        "hostbat.op.echo.in",
        1,
        SchemaRole::OperationInput,
        vec![GoldenVector::new("case", b"old-bytes".to_vec())],
    );
    let v2 = descriptor(
        "hostbat.op.echo.in",
        2,
        SchemaRole::OperationInput,
        vec![GoldenVector::new("case", b"new-bytes".to_vec())],
    );
    assert_ne!(v1.version(), v2.version());
    assert_ne!(
        v1.encoding(),
        v2.encoding(),
        "the bumped version is a distinct schema identity",
    );
}

/// A committed encoding must reproduce from the committed golden vectors. If the
/// stored encoding is corrupted (a tampered seal), `verify_encoding` returns
/// false — the gate refuses it.
#[test]
fn golden_vectors_round_trip_against_committed_encoding() {
    let mut d = descriptor(
        "hostbat.receipt.audit",
        1,
        SchemaRole::ReceiptPayload,
        vec![GoldenVector::new("nominal", b"\x91\x01".to_vec())],
    );
    assert!(
        d.verify_encoding().expect("verify"),
        "committed golden vectors reproduce the sealed encoding",
    );
    d.corrupt_encoding_for_fixture();
    assert!(
        !d.verify_encoding().expect("verify"),
        "a corrupted seal no longer matches the golden vectors — the gate fires",
    );
}

// ---- DiagnosticRustType is non-load-bearing -----------------------------

/// Attaching, changing, or removing the diagnostic Rust type changes NO byte of
/// identity. This is the structural replacement for `refbat::*`-as-identity:
/// deleting the type cannot break the wire.
#[test]
fn diagnostic_rust_type_is_non_load_bearing() {
    let bare = descriptor(
        "hostbat.op.echo.in",
        1,
        SchemaRole::OperationInput,
        vec![GoldenVector::new("c", b"x".to_vec())],
    );
    let with_a = descriptor(
        "hostbat.op.echo.in",
        1,
        SchemaRole::OperationInput,
        vec![GoldenVector::new("c", b"x".to_vec())],
    )
    .with_diagnostic_rust_type(DiagnosticRustType::new("some_crate::SomeType"));
    let with_b = descriptor(
        "hostbat.op.echo.in",
        1,
        SchemaRole::OperationInput,
        vec![GoldenVector::new("c", b"x".to_vec())],
    )
    .with_diagnostic_rust_type(DiagnosticRustType::new("other_crate::RenamedType"));

    let mut failures = Vec::new();
    if bare.encoding() != with_a.encoding() {
        failures.push("attaching a diagnostic type changed the encoding");
    }
    if with_a.encoding() != with_b.encoding() {
        failures.push("renaming the diagnostic type changed the encoding");
    }
    assert_eq!(
        with_a
            .diagnostic_rust_type()
            .map(DiagnosticRustType::as_str),
        Some("some_crate::SomeType"),
        "the diagnostic type is still recorded (informational), just not identity",
    );
    assert!(failures.is_empty(), "{failures:?}");
}

// ---- runtime registry validation ---------------------------------------

#[test]
fn schema_registry_accepts_canonical_payload_for_resolved_descriptor() {
    let descriptor = descriptor(
        "hostbat.op.echo.in",
        1,
        SchemaRole::OperationInput,
        vec![GoldenVector::new("nominal", canonical_fixture("golden"))],
    );
    let registry = SchemaRegistry::from_descriptors([descriptor]);
    let payload = canonical_fixture("payload");

    registry
        .validate("hostbat.op.echo.in", SchemaRole::OperationInput, &payload)
        .expect("canonical payload validates");
}

#[test]
fn schema_registry_rejects_payload_that_is_not_canonical_bytes() {
    let descriptor = descriptor(
        "hostbat.op.echo.in",
        1,
        SchemaRole::OperationInput,
        vec![GoldenVector::new("nominal", canonical_fixture("golden"))],
    );
    let registry = SchemaRegistry::from_descriptors([descriptor]);
    let invalid = invalid_canonical_fixture();
    let outcome = registry.validate("hostbat.op.echo.in", SchemaRole::OperationInput, &invalid);

    assert!(
        matches!(outcome, Err(HostError::SchemaValidation { .. })),
        "non-canonical payload bytes must fail closed with SchemaValidation"
    );
}

#[test]
fn schema_registry_rejects_bad_golden_vector_on_validation() {
    let descriptor = descriptor(
        "hostbat.op.echo.in",
        1,
        SchemaRole::OperationInput,
        vec![GoldenVector::new("bad", invalid_canonical_fixture())],
    );
    let registry = SchemaRegistry::from_descriptors([descriptor]);
    let payload = canonical_fixture("payload");
    let outcome = registry.validate("hostbat.op.echo.in", SchemaRole::OperationInput, &payload);

    assert!(
        matches!(outcome, Err(HostError::SchemaValidation { .. })),
        "a descriptor with a non-canonical golden vector must fail validation"
    );
}

#[test]
fn schema_registry_rejects_missing_schema_ref() {
    let registry = SchemaRegistry::default();
    let payload = canonical_fixture("payload");
    let outcome = registry.validate("hostbat.missing", SchemaRole::OperationInput, &payload);

    assert!(
        matches!(outcome, Err(HostError::SchemaValidation { .. })),
        "missing schema descriptors must fail closed"
    );
}

// ---- descriptor coherence -----------------------------------------------

#[test]
fn duplicate_golden_case_is_rejected() {
    let outcome = SchemaDescriptor::new(
        id("hostbat.op.echo.in"),
        SchemaVersion(1),
        SchemaRole::OperationInput,
        vec![
            GoldenVector::new("dup", b"a".to_vec()),
            GoldenVector::new("dup", b"b".to_vec()),
        ],
    );
    assert!(matches!(outcome, Err(HostError::SchemaInvalid { .. })));
}

// ---- structural schema validation (D1) -----------------------------------

fn shaped_string_descriptor(
    id_str: &str,
    role: SchemaRole,
    golden_value: &str,
) -> SchemaDescriptor {
    descriptor(
        id_str,
        1,
        role,
        vec![GoldenVector::new("c", canonical_fixture(golden_value))],
    )
    .with_shape(SchemaShape::string())
    .expect("shape")
}

fn record_fixture(fields: &[(&str, &str)]) -> Vec<u8> {
    use std::collections::BTreeMap;

    let mut map = BTreeMap::new();
    for (key, value) in fields {
        map.insert(*key, *value);
    }
    batpak::canonical::to_bytes(&map).expect("record fixture encodes")
}

#[test]
fn structural_validation_accepts_matching_canonical_payload() {
    let descriptor =
        shaped_string_descriptor("hostbat.op.echo.in", SchemaRole::OperationInput, "golden");
    let registry = SchemaRegistry::from_descriptors([descriptor]);
    let payload = canonical_fixture("payload");

    registry
        .validate("hostbat.op.echo.in", SchemaRole::OperationInput, &payload)
        .expect("matching canonical payload validates");
}

#[test]
fn structural_validation_rejects_wrong_scalar_type() {
    let descriptor =
        shaped_string_descriptor("hostbat.op.echo.in", SchemaRole::OperationInput, "golden");
    let registry = SchemaRegistry::from_descriptors([descriptor]);
    let payload = batpak::canonical::to_bytes(&1_i64).expect("integer fixture encodes");
    let outcome = registry.validate("hostbat.op.echo.in", SchemaRole::OperationInput, &payload);

    assert!(
        matches!(outcome, Err(HostError::SchemaValidation { .. })),
        "wrong scalar type must fail structural validation",
    );
}

#[test]
fn structural_validation_rejects_missing_required_record_field() {
    let shape = SchemaShape::Record(
        RecordShape::new(
            "hostbat.op.record.in",
            vec![RecordField::required("required", SchemaShape::string())],
        )
        .expect("record shape"),
    );
    let descriptor = descriptor(
        "hostbat.op.record.in",
        1,
        SchemaRole::OperationInput,
        vec![GoldenVector::new(
            "c",
            record_fixture(&[("required", "ok")]),
        )],
    )
    .with_shape(shape)
    .expect("shape");
    let registry = SchemaRegistry::from_descriptors([descriptor]);
    let payload = record_fixture(&[]);
    let outcome = registry.validate("hostbat.op.record.in", SchemaRole::OperationInput, &payload);

    assert!(
        matches!(outcome, Err(HostError::SchemaValidation { .. })),
        "missing required field must fail structural validation",
    );
}

#[test]
fn structural_validation_rejects_unknown_record_field() {
    let shape = SchemaShape::Record(
        RecordShape::new(
            "hostbat.op.record.in",
            vec![RecordField::required("known", SchemaShape::string())],
        )
        .expect("record shape"),
    );
    let descriptor = descriptor(
        "hostbat.op.record.in",
        1,
        SchemaRole::OperationInput,
        vec![GoldenVector::new("c", record_fixture(&[("known", "ok")]))],
    )
    .with_shape(shape)
    .expect("shape");
    let registry = SchemaRegistry::from_descriptors([descriptor]);
    let payload = record_fixture(&[("known", "ok"), ("extra", "nope")]);
    let outcome = registry.validate("hostbat.op.record.in", SchemaRole::OperationInput, &payload);

    assert!(
        matches!(outcome, Err(HostError::SchemaValidation { .. })),
        "unknown record field must fail structural validation by default",
    );
}

#[test]
fn structural_validation_rejects_string_length_bounds() {
    let shape = SchemaShape::Scalar(ScalarShape {
        kind: ScalarKind::String,
        nullable: false,
        min_length: None,
        max_length: Some(3),
        min_i64: None,
        max_i64: None,
        min_u64: None,
        max_u64: None,
    });
    let descriptor = descriptor(
        "hostbat.op.bounded.in",
        1,
        SchemaRole::OperationInput,
        vec![GoldenVector::new("c", canonical_fixture("abc"))],
    )
    .with_shape(shape)
    .expect("shape");
    let registry = SchemaRegistry::from_descriptors([descriptor]);
    let payload = canonical_fixture("abcd");
    let outcome = registry.validate(
        "hostbat.op.bounded.in",
        SchemaRole::OperationInput,
        &payload,
    );

    assert!(
        matches!(outcome, Err(HostError::SchemaValidation { .. })),
        "bounds violations must fail structural validation",
    );
}

#[test]
fn nested_schema_ref_resolves_through_registry() {
    let inner = shaped_string_descriptor("hostbat.inner.v1", SchemaRole::OperationInput, "inner");
    let outer_shape = SchemaShape::Record(
        RecordShape::new(
            "hostbat.outer.in",
            vec![RecordField::required(
                "payload",
                SchemaShape::Ref(RefShape::new(
                    "hostbat.inner.v1",
                    SchemaRole::OperationInput,
                )),
            )],
        )
        .expect("record shape"),
    );
    let outer_bare = descriptor(
        "hostbat.outer.in",
        1,
        SchemaRole::OperationOutput,
        vec![GoldenVector::new(
            "c",
            record_fixture(&[("payload", "nested")]),
        )],
    );
    let outer = outer_bare
        .with_shape_peers(outer_shape, std::slice::from_ref(&inner))
        .expect("shape");
    let registry = SchemaRegistry::from_descriptors([inner, outer]);
    let payload = record_fixture(&[("payload", "nested")]);

    registry
        .validate("hostbat.outer.in", SchemaRole::OperationOutput, &payload)
        .expect("nested ref resolution validates");
}

#[test]
fn shape_change_changes_schema_encoding_at_fixed_version() {
    let golden = canonical_fixture("same-bytes");
    let bare = descriptor(
        "hostbat.op.echo.in",
        1,
        SchemaRole::OperationInput,
        vec![GoldenVector::new("c", golden.clone())],
    );
    let shaped = bare
        .clone()
        .with_shape(SchemaShape::string())
        .expect("shape");
    assert_ne!(
        bare.encoding(),
        shaped.encoding(),
        "attaching structural shape must change the v2 schema encoding",
    );
}

#[test]
fn duplicate_record_field_is_rejected_at_shape_construction() {
    let outcome = RecordShape::new(
        "hostbat.op.record.in",
        vec![
            RecordField::required("dup", SchemaShape::string()),
            RecordField::required("dup", SchemaShape::string()),
        ],
    );
    assert!(matches!(outcome, Err(HostError::SchemaInvalid { .. })));
}
