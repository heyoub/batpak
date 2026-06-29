//! Event payload binding registration, identity, and composition witnesses.

use batpak::event::EventKind;
use syncbat::{Ctx, HandlerResult, OperationDescriptor};

use crate::error::HostError;
use crate::module::{HostModule, HostModuleBuilder};
use crate::schema::{GoldenVector, SchemaDescriptor, SchemaId, SchemaRole, SchemaVersion};
use crate::{EventPayloadBinding, HostBuilder};

const KIND_A: EventKind = EventKind::custom(0xF, 1);

fn canonical_bytes(value: &str) -> Vec<u8> {
    batpak::canonical::to_bytes(&value).expect("canonical fixture encodes")
}

fn op(name: &'static str) -> OperationDescriptor {
    syncbat::OperationDescriptor::new(
        name,
        syncbat::EffectClass::Inspect,
        "schema.in.v1",
        "schema.out.v1",
        "receipt.v1",
    )
}

fn echo(input: &[u8], _cx: &mut Ctx<'_>) -> HandlerResult {
    Ok(input.to_vec())
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

fn event_payload_schema(id: &str, bytes: &[u8]) -> SchemaDescriptor {
    schema_with_role(id, SchemaRole::EventPayload, bytes)
}

fn module_builder_with_op(id: &'static str, op_name: &'static str) -> HostModuleBuilder {
    with_default_operation_schemas(
        HostModule::builder(id, 1)
            .operation(op(op_name), echo)
            .expect("op"),
    )
}

#[test]
fn binding_rejects_empty_schema_reference() -> Result<(), Box<dyn std::error::Error>> {
    let err = match EventPayloadBinding::new(KIND_A, "") {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: empty payload schema reference must be rejected",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(err, HostError::EventPayloadBindingInvalid { .. }));
    Ok(())
}

#[test]
fn module_rejects_duplicate_binding_within_module() -> Result<(), Box<dyn std::error::Error>> {
    let err = match module_builder_with_op("mod.a", "mod.a.echo")
        .bind_event_payload(KIND_A, "event.payload.v1")
        .expect("first binding")
        .bind_event_payload(KIND_A, "event.payload.v2")
    {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: duplicate event kind within one module must be rejected",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        err,
        HostError::EventPayloadBindingDuplicateWithinModule { .. }
    ));
    Ok(())
}

#[test]
fn module_digest_changes_when_event_payload_binding_is_added() {
    let base = module_builder_with_op("mod.a", "mod.a.echo")
        .build()
        .expect("base module");
    let bound = module_builder_with_op("mod.a", "mod.a.echo")
        .bind_event_payload(KIND_A, "event.payload.v1")
        .expect("binding")
        .build()
        .expect("bound module");
    assert_ne!(
        base.manifest().digest(),
        bound.manifest().digest(),
        "event payload bindings fold into H_module",
    );
    assert!(bound.manifest().verify_hash().expect("verify"));
}

#[test]
fn composition_rejects_duplicate_event_kind_across_modules(
) -> Result<(), Box<dyn std::error::Error>> {
    let left = module_builder_with_op("mod.a", "mod.a.echo")
        .bind_event_payload(KIND_A, "event.payload.v1")
        .expect("left binding")
        .build()
        .expect("left module");
    let right = module_builder_with_op("mod.b", "mod.b.echo")
        .bind_event_payload(KIND_A, "event.payload.v1")
        .expect("right binding")
        .build()
        .expect("right module");
    let err = match HostBuilder::new()
        .mount(left)
        .expect("mount left")
        .mount(right)
    {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: duplicate event kind across modules must be rejected",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        err,
        HostError::DuplicateEventPayloadBinding { .. }
    ));
    Ok(())
}

#[test]
fn composition_rejects_conflicting_bindings_for_same_kind() -> Result<(), Box<dyn std::error::Error>>
{
    let left = module_builder_with_op("mod.a", "mod.a.echo")
        .bind_event_payload(KIND_A, "event.payload.v1")
        .expect("left binding")
        .build()
        .expect("left module");
    let right = module_builder_with_op("mod.b", "mod.b.echo")
        .bind_event_payload(KIND_A, "event.payload.v2")
        .expect("right binding")
        .build()
        .expect("right module");
    let err = match HostBuilder::new()
        .mount(left)
        .expect("mount left")
        .mount(right)
    {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: conflicting payload schema refs for one kind must be rejected",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(err, HostError::EventPayloadBindingConflict { .. }));
    Ok(())
}

#[test]
fn host_build_rejects_binding_to_missing_schema() -> Result<(), Box<dyn std::error::Error>> {
    let module = module_builder_with_op("mod.a", "mod.a.echo")
        .bind_event_payload(KIND_A, "event.payload.missing")
        .expect("binding")
        .build()
        .expect("module");
    let err = match HostBuilder::new().mount(module).expect("mount").build() {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: binding to missing schema must fail host build",
            )
            .into())
        }
        Err(error) => error,
    };
    assert!(matches!(
        err,
        HostError::EventPayloadBindingSchemaMissing { .. }
    ));
    Ok(())
}

#[test]
fn interface_fingerprint_changes_when_event_payload_binding_is_added() {
    let base = HostBuilder::new()
        .mount(
            module_builder_with_op("mod.a", "mod.a.echo")
                .schema(event_payload_schema(
                    "event.payload.v1",
                    &canonical_bytes("event-a"),
                ))
                .expect("event schema")
                .build()
                .expect("module"),
        )
        .expect("mount")
        .build()
        .expect("build")
        .interface_fingerprint();
    let bound = HostBuilder::new()
        .mount(
            module_builder_with_op("mod.a", "mod.a.echo")
                .schema(event_payload_schema(
                    "event.payload.v1",
                    &canonical_bytes("event-a"),
                ))
                .expect("event schema")
                .bind_event_payload(KIND_A, "event.payload.v1")
                .expect("binding")
                .build()
                .expect("module"),
        )
        .expect("mount")
        .build()
        .expect("build")
        .interface_fingerprint();
    assert_ne!(
        base, bound,
        "event payload bindings fold into H_interface v4",
    );
}
