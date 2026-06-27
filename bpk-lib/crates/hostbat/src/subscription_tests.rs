//! Subscription descriptor validation + host/interface projection tests.
//!
//! Red fixtures prove each refusal path returns its specific typed error variant.
//! Green fixtures prove subscriptions fold into `H_module`, `H_interface`, and
//! [`crate::client_manifest::ClientManifest`] without claiming NETBAT/2 runtime.

use batpak::coordinate::Coordinate;
use syncbat::{Ctx, HandlerResult, OperationDescriptor};

use crate::builder::HostBuilder;
use crate::client_manifest::ClientManifest;
use crate::error::HostError;
use crate::module::{HostModule, HostModuleBuilder};
use crate::schema::{GoldenVector, SchemaDescriptor, SchemaId, SchemaRole, SchemaVersion};
use crate::subscription::{
    BackpressurePolicy, EventCategory, OperationStatusSelector, ProjectionId, ReceiptFilter,
    SubscriptionDelivery, SubscriptionDescriptor, SubscriptionId, SubscriptionSource,
    SUBSCRIPTION_WIRE_REQUIRES,
};
use crate::SchemaShape;

use super::{validate_subscription_id, EventCategory as SubEventCategory, SubscriptionId as SubId};

fn op(name: &'static str) -> OperationDescriptor {
    OperationDescriptor::new(
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

fn default_backpressure() -> BackpressurePolicy {
    BackpressurePolicy::BoundedQueue { capacity: 128 }
}

fn category_subscription(
    id: &str,
    category: u8,
    payload_schema: &str,
) -> Result<SubscriptionDescriptor, HostError> {
    Ok(SubscriptionDescriptor::new(
        SubscriptionId::new(id)?,
        SubscriptionSource::EventCategory(EventCategory::new(category)?),
        payload_schema,
        SubscriptionDelivery::CursorAtLeastOnce,
        default_backpressure(),
    ))
}

fn module_with_category_subscription(
    module_id: &'static str,
    op_name: &'static str,
    subscription_id: &str,
    category: u8,
) -> Result<HostModule, HostError> {
    module_builder_with_op(module_id, op_name)
        .schema(schema_with_role(
            "hostbat.event.orders.v1",
            SchemaRole::EventPayload,
            &canonical_bytes("orders-payload"),
        ))
        .expect("event payload schema")
        .subscription(category_subscription(
            subscription_id,
            category,
            "hostbat.event.orders.v1",
        )?)?
        .build()
}

// ---- unit: subscription id grammar ---------------------------------------

#[test]
fn subscription_id_rejects_bad_grammar() {
    let cases = [
        ("", "empty"),
        (".orders.v1", "leading dot"),
        ("orders.v1.", "trailing dot"),
        ("orders..open.v1", "doubled dot"),
        ("Orders.open.v1", "uppercase"),
        ("orders.open.v0", "version zero"),
        ("orders.open.v01", "leading zero in version"),
        ("orders.open", "missing .v suffix"),
        ("v1", "missing name prefix"),
    ];
    let mut failures = Vec::new();
    for (candidate, why) in cases {
        if SubId::new(candidate).is_ok() {
            failures.push(format!(
                "{candidate:?} ({why}) was accepted but should be rejected"
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn subscription_id_accepts_valid_names() {
    for good in ["orders.open.v1", "a.v1", "orders.v12", "x_y.z-1.v9"] {
        if SubId::new(good).is_err() {
            assert!(
                std::hint::black_box(false),
                "PROPERTY: {good:?} should be valid"
            );
        }
    }
}

#[test]
fn validate_subscription_id_rejects_overlong_ids() {
    let long_name = format!("{}.v1", "a".repeat(130));
    assert_eq!(
        validate_subscription_id(&long_name),
        Err("subscription id longer than 128 bytes")
    );
}

// ---- unit: reserved categories -------------------------------------------

#[test]
fn reserved_event_categories_are_rejected() {
    for category in [0u8, 0xD, 16] {
        let outcome = SubEventCategory::new(category);
        assert!(
            matches!(outcome, Err(HostError::SubscriptionReservedCategory { .. })),
            "category 0x{category:02x} must be rejected"
        );
    }
}

#[test]
fn exported_event_category_accepts_non_reserved_values() {
    assert!(SubEventCategory::new(0xA).is_ok());
}

// ---- red: duplicate ids --------------------------------------------------

#[test]
fn red_duplicate_subscription_id_within_module_fails_exact_variant() {
    let first = category_subscription("orders.open.v1", 0xA, "hostbat.event.orders.v1")
        .expect("descriptor");
    let duplicate = category_subscription("orders.open.v1", 0xB, "hostbat.event.orders.v1")
        .expect("descriptor");
    let outcome = module_builder_with_op("mod.a", "mod.a.echo")
        .subscription(first)
        .expect("first")
        .subscription(duplicate);
    assert!(matches!(
        outcome,
        Err(HostError::SubscriptionDuplicateWithinModule { .. })
    ));
}

#[test]
fn red_duplicate_subscription_id_across_modules_fails_exact_variant() {
    let module_a = module_with_category_subscription("mod.a", "mod.a.echo", "orders.open.v1", 0xA)
        .expect("module a");
    let module_b = module_with_category_subscription("mod.b", "mod.b.echo", "orders.open.v1", 0xA)
        .expect("module b");
    let outcome = HostBuilder::new()
        .mount(module_a)
        .expect("mount a")
        .mount(module_b);
    assert!(matches!(
        outcome,
        Err(HostError::DuplicateSubscriptionId { .. })
    ));
}

// ---- red: missing payload schema -----------------------------------------

#[test]
fn red_missing_payload_schema_fails_exact_variant() -> Result<(), HostError> {
    let module = module_builder_with_op("mod.a", "mod.a.echo")
        .subscription(category_subscription(
            "orders.open.v1",
            0xA,
            "hostbat.event.missing.v1",
        )?)?
        .build()?;
    let outcome = HostBuilder::new().mount(module).expect("mount").build();
    assert!(matches!(
        outcome,
        Err(HostError::SubscriptionPayloadSchemaMissing { .. })
    ));
    Ok(())
}

// ---- green: H_interface + ClientManifest -----------------------------------

#[test]
fn adding_subscription_flips_h_interface() -> Result<(), HostError> {
    let base = HostBuilder::new()
        .mount(
            module_builder_with_op("mod.a", "mod.a.echo")
                .build()
                .expect("module"),
        )
        .expect("mount")
        .build()
        .expect("build")
        .interface_fingerprint();
    let with_subscription = HostBuilder::new()
        .mount(module_with_category_subscription(
            "mod.a",
            "mod.a.echo",
            "orders.open.v1",
            0xA,
        )?)
        .expect("mount")
        .build()
        .expect("build")
        .interface_fingerprint();
    assert_ne!(
        base, with_subscription,
        "declaring a subscription changes H_interface"
    );
    Ok(())
}

#[test]
fn changing_subscription_source_flips_h_interface() -> Result<(), HostError> {
    let category_a = HostBuilder::new()
        .mount(module_with_category_subscription(
            "mod.a",
            "mod.a.echo",
            "orders.open.v1",
            0xA,
        )?)
        .expect("mount")
        .build()
        .expect("build")
        .interface_fingerprint();
    let projection = HostBuilder::new()
        .mount({
            let descriptor = SubscriptionDescriptor::new(
                SubscriptionId::new("orders.open.v1")?,
                SubscriptionSource::Projection(ProjectionId::new("orders.projection.v1")?),
                "hostbat.event.orders.v1",
                SubscriptionDelivery::CursorAtLeastOnce,
                default_backpressure(),
            );
            module_builder_with_op("mod.a", "mod.a.echo")
                .schema(schema_with_role(
                    "hostbat.event.orders.v1",
                    SchemaRole::SubscriptionPayload,
                    &canonical_bytes("orders-status"),
                ))
                .expect("subscription payload schema")
                .subscription(descriptor)?
                .build()?
        })
        .expect("mount")
        .build()
        .expect("build")
        .interface_fingerprint();
    assert_ne!(
        category_a, projection,
        "changing subscription source changes H_interface"
    );
    Ok(())
}

#[test]
fn changing_subscription_delivery_flips_h_interface() -> Result<(), HostError> {
    let make = |capacity: u32| -> Result<_, HostError> {
        let descriptor = SubscriptionDescriptor::new(
            SubscriptionId::new("orders.open.v1")?,
            SubscriptionSource::EventCategory(EventCategory::new(0xA)?),
            "hostbat.event.orders.v1",
            SubscriptionDelivery::CursorAtLeastOnce,
            BackpressurePolicy::BoundedQueue { capacity },
        );
        Ok(HostBuilder::new()
            .mount(
                module_builder_with_op("mod.a", "mod.a.echo")
                    .schema(schema_with_role(
                        "hostbat.event.orders.v1",
                        SchemaRole::EventPayload,
                        &canonical_bytes("orders-payload"),
                    ))
                    .expect("payload schema")
                    .subscription(descriptor)?
                    .build()?,
            )
            .expect("mount")
            .build()?
            .interface_fingerprint())
    };
    assert_ne!(
        make(128)?,
        make(256)?,
        "changing backpressure capacity changes H_interface"
    );
    Ok(())
}

#[test]
fn hook_change_does_not_flip_h_interface() {
    let plain = HostBuilder::new()
        .mount(
            module_with_category_subscription("mod.a", "mod.a.echo", "orders.open.v1", 0xA)
                .expect("module"),
        )
        .expect("mount")
        .build()
        .expect("build")
        .interface_fingerprint();
    let with_hook = HostBuilder::new()
        .mount(
            module_builder_with_op("mod.a", "mod.a.echo")
                .schema(schema_with_role(
                    "hostbat.event.orders.v1",
                    SchemaRole::EventPayload,
                    &canonical_bytes("orders-payload"),
                ))
                .expect("payload schema")
                .subscription(
                    category_subscription("orders.open.v1", 0xA, "hostbat.event.orders.v1")
                        .expect("descriptor"),
                )
                .expect("subscription")
                .hook(crate::descriptor::HookPhase::Startup, "internal", 0, || {
                    Ok(())
                })
                .build()
                .expect("module"),
        )
        .expect("mount")
        .build()
        .expect("build")
        .interface_fingerprint();
    assert_eq!(
        plain, with_hook,
        "internal lifecycle hooks do not change the client-visible interface"
    );
}

#[test]
fn client_manifest_includes_subscriptions_and_matches_h_interface() -> Result<(), HostError> {
    let host = HostBuilder::new()
        .mount(module_with_category_subscription(
            "mod.a",
            "mod.a.echo",
            "orders.open.v1",
            0xA,
        )?)
        .expect("mount")
        .build()?;
    let manifest = ClientManifest::from_host(&host);
    assert_eq!(manifest.manifest_version, 3);
    assert_eq!(manifest.netbat_version, "NETBAT/1");
    assert_eq!(
        manifest.subscription_wire_requires,
        SUBSCRIPTION_WIRE_REQUIRES
    );
    assert_eq!(
        manifest.interface_fingerprint_hex,
        host.interface_fingerprint().to_hex()
    );
    assert_eq!(manifest.subscriptions.len(), 1);
    assert_eq!(manifest.subscriptions[0].id, "orders.open.v1");
    assert_eq!(manifest.subscriptions[0].module_id, "mod.a");
    assert_eq!(
        manifest.subscriptions[0].payload_schema_role,
        "event-payload"
    );
    Ok(())
}

#[test]
fn runtime_does_not_claim_netbat2_serving() -> Result<(), HostError> {
    let host = HostBuilder::new()
        .mount(module_with_category_subscription(
            "mod.a",
            "mod.a.echo",
            "orders.open.v1",
            0xA,
        )?)
        .expect("mount")
        .build()?;
    let manifest = ClientManifest::from_host(&host);
    assert_eq!(manifest.netbat_version, "NETBAT/1");
    assert_eq!(manifest.subscription_wire_requires, "NETBAT/2-streaming");
    assert!(
        !manifest.subscriptions.is_empty(),
        "subscriptions are declared in the client manifest"
    );
    Ok(())
}

#[test]
fn entity_stream_subscription_uses_event_payload_role() -> Result<(), Box<dyn std::error::Error>> {
    let coord = Coordinate::new("entity:orders", "scope:open")?;
    let descriptor = SubscriptionDescriptor::new(
        SubscriptionId::new("orders.entity.v1")?,
        SubscriptionSource::EntityStream(coord),
        "hostbat.event.orders.v1",
        SubscriptionDelivery::CursorAtLeastOnce,
        default_backpressure(),
    );
    assert_eq!(descriptor.required_payload_role(), SchemaRole::EventPayload);
    Ok(())
}

#[test]
fn receipt_stream_subscription_uses_receipt_payload_role() -> Result<(), HostError> {
    let descriptor = SubscriptionDescriptor::new(
        SubscriptionId::new("receipts.appended.v1")?,
        SubscriptionSource::ReceiptStream(ReceiptFilter::new("receipt.v1")),
        "receipt.v1",
        SubscriptionDelivery::CursorAtLeastOnce,
        default_backpressure(),
    );
    assert_eq!(
        descriptor.required_payload_role(),
        SchemaRole::ReceiptPayload
    );
    Ok(())
}

#[test]
fn operation_status_subscription_uses_subscription_payload_role() -> Result<(), HostError> {
    let descriptor = SubscriptionDescriptor::new(
        SubscriptionId::new("orders.status.v1")?,
        SubscriptionSource::OperationStatus(OperationStatusSelector::new("mod.a.echo")),
        "hostbat.sub.status.v1",
        SubscriptionDelivery::CursorAtLeastOnce,
        default_backpressure(),
    );
    assert_eq!(
        descriptor.required_payload_role(),
        SchemaRole::SubscriptionPayload
    );
    Ok(())
}
