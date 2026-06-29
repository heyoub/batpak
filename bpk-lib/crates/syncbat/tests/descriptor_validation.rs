//! PROVES: INV-SYNCBAT-REGISTER-CATALOG-DETERMINISTIC
//! CATCHES: invalid operation names and schema/receipt references before catalog or runtime insertion.
//! SEEDED: fixed descriptor-token tables.

use syncbat::{
    Core, EffectClass, HandlerResult, Module, OperationDescriptor, OperationEffectRow, Register,
};

fn valid_descriptor() -> OperationDescriptor {
    OperationDescriptor::new(
        "repo.patch",
        EffectClass::Persist,
        "schema.repo.patch.input.v1",
        "schema.repo.patch.output.v1",
        "receipt.repo.patch.v1",
    )
    .with_effect_row(OperationEffectRow::new().appends_event("event.repo.patch.v1"))
}

fn descriptor_with_name(name: &'static str) -> OperationDescriptor {
    OperationDescriptor::new(
        name,
        EffectClass::Compute,
        "schema.valid.input.v1",
        "schema.valid.output.v1",
        "receipt.valid.v1",
    )
}

fn descriptor_with_refs(
    input_schema_ref: &'static str,
    output_schema_ref: &'static str,
    receipt_kind: &'static str,
) -> OperationDescriptor {
    OperationDescriptor::new(
        "valid.operation",
        EffectClass::Compute,
        input_schema_ref,
        output_schema_ref,
        receipt_kind,
    )
}

#[test]
fn descriptor_validation_accepts_stable_ascii_tokens() {
    let descriptor = OperationDescriptor::new(
        "repo.patch-v1_test",
        EffectClass::Persist,
        "schema.repo.patch-v1_test.input",
        "schema.repo.patch-v1_test.output",
        "receipt.repo.patch-v1_test",
    )
    .with_effect_row(OperationEffectRow::new().appends_event("event.repo.patch-v1_test"));

    descriptor.validate().expect("descriptor is valid");
    Register::from_operations([descriptor]).expect("register accepts descriptor");
}

#[test]
fn descriptor_validation_rejects_empty_overlong_and_path_like_names() {
    let overlong = "a".repeat(syncbat::MAX_OPERATION_NAME_BYTES + 1);
    let cases = [
        descriptor_with_name(""),
        descriptor_with_name("repo patch"),
        descriptor_with_name("repo/patch"),
        descriptor_with_name("repo?patch"),
        descriptor_with_name("repo#patch"),
        descriptor_with_name(".repo"),
        descriptor_with_name("repo."),
        descriptor_with_name("repo..patch"),
        descriptor_with_name(Box::leak(overlong.into_boxed_str())),
    ];

    for descriptor in cases {
        let err = Register::from_operations([descriptor.clone()])
            .map(|_| ())
            .expect_err(&format!(
                "expected descriptor rejection for {:?}",
                descriptor.name()
            ));
        assert!(
            matches!(
                err,
                syncbat::RegisterValidationError::InvalidDescriptor { .. }
            ),
            "unexpected error: {err:?}"
        );
    }
}

#[test]
fn descriptor_validation_rejects_bad_schema_and_receipt_refs() {
    let cases = [
        descriptor_with_refs("", "schema.valid.output.v1", "receipt.valid.v1"),
        descriptor_with_refs("schema.valid.input.v1", "", "receipt.valid.v1"),
        descriptor_with_refs("schema.valid.input.v1", "schema.valid.output.v1", ""),
        descriptor_with_refs("schema/input", "schema.valid.output.v1", "receipt.valid.v1"),
        descriptor_with_refs("schema.valid.input.v1", "schema output", "receipt.valid.v1"),
        descriptor_with_refs(
            "schema.valid.input.v1",
            "schema.valid.output.v1",
            "receipt..valid",
        ),
    ];

    for descriptor in cases {
        let err = descriptor
            .validate()
            .expect_err("expected descriptor rejection");
        assert!(!err.field.is_empty());
        assert!(!err.message.is_empty());
    }
}

#[test]
fn module_validation_rejects_invalid_module_names() {
    for name in [
        "",
        "bad/module",
        "bad module",
        ".bad",
        "bad.",
        "bad..module",
    ] {
        let err = Module::from_operations(name, [valid_descriptor()])
            .map(|_| ())
            .expect_err(&format!("expected module rejection for {name:?}"));

        assert!(
            matches!(
                err,
                syncbat::RegisterValidationError::InvalidModuleName { .. }
            ),
            "unexpected error: {err:?}"
        );
    }
}

#[test]
fn builder_rejects_invalid_descriptor_and_handler_names() {
    let invalid_descriptor = descriptor_with_name("bad/name");
    let mut builder = Core::builder();
    let err = builder
        .register_operation(invalid_descriptor)
        .map(|_| ())
        .expect_err("expected invalid operation");
    assert!(matches!(err, syncbat::BuildError::InvalidOperation { .. }));

    let mut builder = Core::builder();
    let err = builder
        .register_handler(
            "bad/name",
            |_input: &[u8], _cx: &mut syncbat::Ctx<'_>| -> HandlerResult { Ok(Vec::new()) },
        )
        .map(|_| ())
        .expect_err("expected invalid handler");
    assert!(matches!(err, syncbat::BuildError::InvalidHandler { .. }));
}
