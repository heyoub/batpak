//! H_interface, ClientManifest, and structural-schema composition witnesses.

use syncbat::{Ctx, EffectClass, HandlerResult, OperationDescriptor};

use crate::descriptor::HookPhase;
use crate::error::HostError;
use crate::module::{HostModule, HostModuleBuilder};
use crate::schema::{
    DiagnosticRustType, GoldenVector, SchemaDescriptor, SchemaId, SchemaRole, SchemaVersion,
};
use crate::{ClientManifest, HostBuilder, RecordField, RecordShape, SchemaShape};

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

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

fn schema_with_role(id: &str, role: SchemaRole, bytes: &[u8]) -> SchemaDescriptor {
    SchemaDescriptor::new(
        SchemaId::new(id).expect("id"),
        SchemaVersion(1),
        role,
        vec![GoldenVector::new("c", bytes.to_vec())],
    )
    .expect("descriptor")
    .with_shape(SchemaShape::string())
    .expect("shape")
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
fn interface_fingerprint_changes_when_structural_shape_changes() {
    use std::collections::BTreeMap;

    let string_host = HostBuilder::new()
        .mount(single_op_module("mod.a", "mod.a.echo"))
        .expect("mount")
        .build()
        .expect("build")
        .interface_fingerprint();
    let record_golden = {
        let mut fields = BTreeMap::new();
        fields.insert("msg", "default-in");
        batpak::canonical::to_bytes(&fields).expect("record fixture encodes")
    };
    let record_shape = SchemaShape::Record(
        RecordShape::new(
            "schema.in.v1",
            vec![RecordField::required("msg", SchemaShape::string())],
        )
        .expect("record shape"),
    );
    let record_host = HostBuilder::new()
        .mount(
            HostModule::builder("mod.a", 1)
                .operation(op("mod.a.echo"), echo)
                .expect("op")
                .schema(
                    SchemaDescriptor::new(
                        SchemaId::new("schema.in.v1").expect("id"),
                        SchemaVersion(1),
                        SchemaRole::OperationInput,
                        vec![GoldenVector::new("c", record_golden)],
                    )
                    .expect("descriptor")
                    .with_shape(record_shape)
                    .expect("shape"),
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
    assert_ne!(
        string_host, record_host,
        "structural shape change changes H_interface",
    );
}

#[test]
fn interface_fingerprint_is_stable_for_diagnostic_rust_type_on_shaped_schema() {
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

#[test]
fn red_client_visible_schema_without_shape_fails_host_build() {
    let module = HostModule::builder("mod.a", 1)
        .operation(op("mod.a.echo"), echo)
        .expect("op")
        .schema(
            SchemaDescriptor::new(
                SchemaId::new("schema.in.v1").expect("id"),
                SchemaVersion(1),
                SchemaRole::OperationInput,
                vec![GoldenVector::new("c", canonical_bytes("default-in"))],
            )
            .expect("descriptor"),
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
        .expect("module");
    let outcome = HostBuilder::new().mount(module).expect("mount").build();
    assert!(
        matches!(outcome, Err(HostError::SchemaShapeMissing { .. })),
        "client-visible schema refs without structural shape fail closed at host build",
    );
}

#[test]
fn client_manifest_projects_live_host_contract() {
    let host = HostBuilder::new()
        .mount(single_op_module("mod.a", "mod.a.echo"))
        .expect("mount")
        .build()
        .expect("build");

    let manifest = ClientManifest::from_host(&host);

    assert_eq!(manifest.manifest_version, 4);
    assert_eq!(manifest.netbat_version, "NETBAT/1");
    assert_eq!(
        manifest.subscription_wire_requires,
        crate::subscription::SUBSCRIPTION_WIRE_REQUIRES
    );
    assert_eq!(
        manifest.interface_fingerprint_hex,
        host.interface_fingerprint().to_hex()
    );
    assert_eq!(manifest.operations.len(), 1);
    assert_eq!(manifest.subscriptions.len(), 0);
    assert_eq!(manifest.operations[0].name, "mod.a.echo");
    assert_eq!(manifest.operations[0].input_schema_ref, "schema.in.v1");
    assert_eq!(manifest.schemas.len(), 3);
    let input_schema = manifest
        .schemas
        .iter()
        .find(|schema| schema.id == "schema.in.v1")
        .expect("input schema exported");
    assert_eq!(input_schema.role, "operation-input");
    assert_eq!(
        input_schema.golden[0].bytes_hex,
        hex(&canonical_bytes("default-in"))
    );
}

#[test]
fn client_manifest_changes_when_schema_golden_changes() {
    let make = |bytes: &[u8]| {
        let module = HostModule::builder("mod.a", 1)
            .operation(op("mod.a.echo"), echo)
            .expect("operation")
            .schema(schema_with_role(
                "schema.in.v1",
                SchemaRole::OperationInput,
                bytes,
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
            .expect("module");
        HostBuilder::new()
            .mount(module)
            .expect("mount")
            .build()
            .expect("build")
    };
    let left = ClientManifest::from_host(&make(&canonical_bytes("left")));
    let right = ClientManifest::from_host(&make(&canonical_bytes("right")));

    assert_ne!(
        left.interface_fingerprint_hex,
        right.interface_fingerprint_hex
    );
    assert_ne!(left.schemas, right.schemas);
}
