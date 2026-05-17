#![allow(clippy::panic)]

use syncbat::{Core, EffectClass, HandlerResult, Module, OperationDescriptor, Register};

const VALID: OperationDescriptor = OperationDescriptor::new(
    "repo.patch",
    EffectClass::Persist,
    "schema.repo.patch.input.v1",
    "schema.repo.patch.output.v1",
    "receipt.repo.patch.v1",
);

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
    );

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
        let err = match Register::from_operations([descriptor.clone()]) {
            Ok(_) => panic!("expected descriptor rejection for {:?}", descriptor.name()),
            Err(error) => error,
        };
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
        let err = match descriptor.validate() {
            Ok(()) => panic!("expected descriptor rejection"),
            Err(error) => error,
        };
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
        let err = match Module::from_operations(name, [VALID]) {
            Ok(_) => panic!("expected module rejection for {name:?}"),
            Err(error) => error,
        };

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
    let err = match builder.register_operation(invalid_descriptor) {
        Ok(_) => panic!("expected invalid operation"),
        Err(error) => error,
    };
    assert!(matches!(err, syncbat::BuildError::InvalidOperation { .. }));

    let mut builder = Core::builder();
    let err = match builder.register_handler(
        "bad/name",
        |_input: &[u8], _cx: &mut syncbat::Ctx<'_>| -> HandlerResult { Ok(Vec::new()) },
    ) {
        Ok(_) => panic!("expected invalid handler"),
        Err(error) => error,
    };
    assert!(matches!(err, syncbat::BuildError::InvalidHandler { .. }));
}
