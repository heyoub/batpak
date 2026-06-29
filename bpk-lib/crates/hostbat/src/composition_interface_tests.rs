//! H_interface and schema-identity composition witnesses.

use syncbat::{Ctx, EffectClass, HandlerResult, OperationDescriptor};

use crate::descriptor::HookPhase;
use crate::module::{HostModule, HostModuleBuilder};
use crate::schema::{
    DiagnosticRustType, GoldenVector, SchemaDescriptor, SchemaId, SchemaRole, SchemaVersion,
};
use crate::HostBuilder;

fn op(name: &'static str) -> OperationDescriptor {
    OperationDescriptor::new(
        name,
        EffectClass::Inspect,
        "schema.in.v1",
        "schema.out.v1",
        "receipt.v1",
    )
}

fn echo(input: &[u8], _cx: &mut Ctx<'_>) -> HandlerResult {
    Ok(input.to_vec())
}

fn canonical_bytes(value: &str) -> Vec<u8> {
    batpak::canonical::to_bytes(&value).expect("canonical fixture encodes")
}

fn schema_with_role(id: &str, role: SchemaRole, bytes: &[u8]) -> SchemaDescriptor {
    SchemaDescriptor::new(
        SchemaId::new(id).expect("id"),
        SchemaVersion(1),
        role,
        vec![GoldenVector::new("c", bytes.to_vec())],
    )
    .expect("descriptor")
}

fn with_default_operation_schemas(builder: HostModuleBuilder) -> HostModuleBuilder {
    builder
        .schema(schema_with_role(
            "schema.in.v1",
            SchemaRole::OperationInput,
            &canonical_bytes("default-in"),
        ))
        .expect("input schema")
        .schema(schema_with_role(
            "schema.out.v1",
            SchemaRole::OperationOutput,
            &canonical_bytes("default-out"),
        ))
        .expect("output schema")
        .schema(schema_with_role(
            "receipt.v1",
            SchemaRole::ReceiptPayload,
            &canonical_bytes("default-receipt"),
        ))
        .expect("receipt schema")
}

fn module_builder_with_op(id: &'static str, op_name: &'static str) -> HostModuleBuilder {
    with_default_operation_schemas(
        HostModule::builder(id, 1)
            .operation(op(op_name), echo)
            .expect("op"),
    )
}

fn single_op_module(id: &'static str, op_name: &'static str) -> HostModule {
    module_builder_with_op(id, op_name)
        .build()
        .expect("module builds")
}

#[test]
fn interface_fingerprint_is_stable_for_internal_hook_changes() {
    let plain = HostBuilder::new()
        .mount(single_op_module("mod.a", "mod.a.echo"))
        .expect("mount")
        .build()
        .expect("build")
        .interface_fingerprint();
    let with_hook = HostBuilder::new()
        .mount(
            module_builder_with_op("mod.a", "mod.a.echo")
                .hook(HookPhase::Startup, "internal", 0, || Ok(()))
                .build()
                .expect("module"),
        )
        .expect("mount")
        .build()
        .expect("build")
        .interface_fingerprint();
    assert_eq!(
        plain, with_hook,
        "internal lifecycle hooks do not change the client-visible interface",
    );
}

#[test]
fn interface_fingerprint_changes_for_operation_or_schema_surface() {
    let base = HostBuilder::new()
        .mount(single_op_module("mod.a", "mod.a.echo"))
        .expect("mount")
        .build()
        .expect("build")
        .interface_fingerprint();
    let renamed = HostBuilder::new()
        .mount(single_op_module("mod.a", "mod.a.renamed"))
        .expect("mount")
        .build()
        .expect("build")
        .interface_fingerprint();
    let schema_changed = HostBuilder::new()
        .mount(
            HostModule::builder("mod.a", 1)
                .operation(op("mod.a.echo"), echo)
                .expect("op")
                .schema(schema_with_role(
                    "schema.in.v1",
                    SchemaRole::OperationInput,
                    &canonical_bytes("changed-in"),
                ))
                .expect("input schema")
                .schema(schema_with_role(
                    "schema.out.v1",
                    SchemaRole::OperationOutput,
                    &canonical_bytes("default-out"),
                ))
                .expect("output schema")
                .schema(schema_with_role(
                    "receipt.v1",
                    SchemaRole::ReceiptPayload,
                    &canonical_bytes("default-receipt"),
                ))
                .expect("receipt schema")
                .build()
                .expect("module"),
        )
        .expect("mount")
        .build()
        .expect("build")
        .interface_fingerprint();
    assert_ne!(base, renamed, "operation rename changes H_interface");
    assert_ne!(
        base, schema_changed,
        "operation payload schema identity changes H_interface",
    );
}

#[test]
fn interface_fingerprint_is_stable_for_diagnostic_rust_type() {
    let base = HostBuilder::new()
        .mount(single_op_module("mod.a", "mod.a.echo"))
        .expect("mount")
        .build()
        .expect("build")
        .interface_fingerprint();
    let with_diagnostic = HostBuilder::new()
        .mount(
            HostModule::builder("mod.a", 1)
                .operation(op("mod.a.echo"), echo)
                .expect("op")
                .schema(
                    schema_with_role(
                        "schema.in.v1",
                        SchemaRole::OperationInput,
                        &canonical_bytes("default-in"),
                    )
                    .with_diagnostic_rust_type(DiagnosticRustType::new("hostbat_tests::EchoInput")),
                )
                .expect("input schema")
                .schema(schema_with_role(
                    "schema.out.v1",
                    SchemaRole::OperationOutput,
                    &canonical_bytes("default-out"),
                ))
                .expect("output schema")
                .schema(schema_with_role(
                    "receipt.v1",
                    SchemaRole::ReceiptPayload,
                    &canonical_bytes("default-receipt"),
                ))
                .expect("receipt schema")
                .build()
                .expect("module"),
        )
        .expect("mount")
        .build()
        .expect("build")
        .interface_fingerprint();
    assert_eq!(
        base, with_diagnostic,
        "DiagnosticRustType remains excluded from H_interface",
    );
}
