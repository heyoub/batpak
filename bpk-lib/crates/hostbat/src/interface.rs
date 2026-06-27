//! Client-visible interface identity for a composed host.

use serde::Serialize;
use syncbat::OperationDescriptor;

use crate::composition::HostCompositionManifest;
use crate::error::HostError;
use crate::event_payload_binding::EventPayloadBinding;
use crate::identity::{canonical_digest, Digest, InterfaceFingerprint};
use crate::module::HostModuleParts;
use crate::schema::{SchemaDescriptor, SchemaRole};
use crate::subscription::{BackpressurePolicy, SubscriptionDescriptor, SUBSCRIPTION_WIRE_REQUIRES};
use crate::SchemaShape;

/// Domain separator for the client-visible interface fingerprint.
const INTERFACE_DIGEST_DOMAIN: &str = "hostbat.interface.v4";
/// Version of the canonical view folded into [`InterfaceFingerprint`].
const INTERFACE_VIEW_SCHEMA_VERSION: u16 = 4;
/// Wire protocol version exposed to generated clients for callable operations.
const WIRE_PROTOCOL_VERSION: &str = "NETBAT/1";
/// Batpak named-field MessagePack encoding contract exposed to generated
/// clients. This pins the stable wire *contract*, not the encoder crate's patch
/// version — a transitive `rmp-serde` bump that changes no wire bytes must not
/// flip the client-visible interface fingerprint.
const WIRE_ENCODING_VERSION: &str = "named-field-msgpack:v1";

#[derive(Serialize)]
struct InterfaceView<'a> {
    domain: &'a str,
    view_schema_version: u16,
    wire_protocol_version: &'a str,
    wire_encoding_version: &'a str,
    subscription_wire_requires: &'a str,
    operations: Vec<InterfaceOperationView<'a>>,
    subscriptions: Vec<InterfaceSubscriptionView<'a>>,
    event_payload_bindings: Vec<InterfaceEventPayloadBindingView<'a>>,
    exported_event_payloads: Vec<SchemaIdentityView<'a>>,
}

#[derive(Serialize)]
struct InterfaceOperationView<'a> {
    module_id: &'a str,
    name: &'a str,
    input_schema: SchemaIdentityView<'a>,
    output_schema: SchemaIdentityView<'a>,
    receipt_kind: &'a str,
    receipt_schema: SchemaIdentityView<'a>,
}

#[derive(Serialize)]
struct InterfaceSubscriptionView<'a> {
    module_id: &'a str,
    id: &'a str,
    source: &'a crate::subscription::SubscriptionSource,
    payload_schema: SchemaIdentityView<'a>,
    delivery: &'a str,
    backpressure: BackpressureView,
}

#[derive(Serialize)]
struct InterfaceEventPayloadBindingView<'a> {
    module_id: &'a str,
    kind: u16,
    payload_schema: SchemaIdentityView<'a>,
}

#[derive(Serialize)]
struct BackpressureView {
    kind: &'static str,
    capacity: Option<u32>,
}

#[derive(Clone, Copy, Serialize)]
struct SchemaIdentityView<'a> {
    id: &'a str,
    version: u32,
    role: &'a str,
    encoding: Digest,
    shape: Option<&'a SchemaShape>,
}

/// Compute the client-visible interface fingerprint for a mounted host.
///
/// # Errors
/// Returns [`HostError`] when an operation or subscription references a schema id
/// that is missing or ambiguous in the composition, or when canonical encoding fails.
pub(crate) fn compute_interface_fingerprint(
    modules: &[HostModuleParts],
    composition_schemas: &HostCompositionManifest,
) -> Result<InterfaceFingerprint, HostError> {
    let mut operations = Vec::new();
    let mut subscriptions = Vec::new();
    let mut event_payload_bindings = Vec::new();
    for parts in modules {
        let module_id = parts.manifest.id();
        for descriptor in parts.manifest.operations() {
            operations.push(operation_view(module_id, descriptor, composition_schemas)?);
        }
        for descriptor in parts.manifest.subscriptions() {
            subscriptions.push(subscription_view(
                module_id,
                descriptor,
                composition_schemas,
            )?);
        }
        for binding in parts.manifest.event_payload_bindings() {
            event_payload_bindings.push(event_payload_binding_view(
                module_id,
                binding,
                composition_schemas,
            )?);
        }
    }
    operations.sort_by(|a, b| {
        a.module_id
            .cmp(b.module_id)
            .then_with(|| a.name.cmp(b.name))
    });
    subscriptions.sort_by(|a, b| a.module_id.cmp(b.module_id).then_with(|| a.id.cmp(b.id)));
    event_payload_bindings.sort_by(|a, b| {
        a.module_id
            .cmp(b.module_id)
            .then_with(|| a.kind.cmp(&b.kind))
    });

    let mut exported_event_payloads = composition_schemas
        .schemas()
        .filter_map(|entry| {
            let descriptor = entry.descriptor();
            if descriptor.role() == SchemaRole::EventPayload {
                Some(schema_identity_view(descriptor))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    exported_event_payloads.sort_by(|a, b| a.id.cmp(b.id).then_with(|| a.version.cmp(&b.version)));

    let view = InterfaceView {
        domain: INTERFACE_DIGEST_DOMAIN,
        view_schema_version: INTERFACE_VIEW_SCHEMA_VERSION,
        wire_protocol_version: WIRE_PROTOCOL_VERSION,
        wire_encoding_version: WIRE_ENCODING_VERSION,
        subscription_wire_requires: SUBSCRIPTION_WIRE_REQUIRES,
        operations,
        subscriptions,
        event_payload_bindings,
        exported_event_payloads,
    };
    canonical_digest(&view).map(InterfaceFingerprint)
}

fn event_payload_binding_view<'a>(
    module_id: &'a str,
    binding: &'a EventPayloadBinding,
    composition_schemas: &'a HostCompositionManifest,
) -> Result<InterfaceEventPayloadBindingView<'a>, HostError> {
    let payload_schema = resolve_schema_ref(
        module_id,
        None,
        binding.payload_schema_ref(),
        SchemaRole::EventPayload,
        composition_schemas,
    )?;
    Ok(InterfaceEventPayloadBindingView {
        module_id,
        kind: binding.kind_raw(),
        payload_schema,
    })
}

fn operation_view<'a>(
    module_id: &'a str,
    descriptor: &'a OperationDescriptor,
    composition_schemas: &'a HostCompositionManifest,
) -> Result<InterfaceOperationView<'a>, HostError> {
    let input_schema = resolve_schema_ref(
        module_id,
        Some(descriptor.name()),
        descriptor.input_schema_ref(),
        SchemaRole::OperationInput,
        composition_schemas,
    )?;
    let output_schema = resolve_schema_ref(
        module_id,
        Some(descriptor.name()),
        descriptor.output_schema_ref(),
        SchemaRole::OperationOutput,
        composition_schemas,
    )?;
    let receipt_schema = resolve_schema_ref(
        module_id,
        Some(descriptor.name()),
        descriptor.receipt_kind(),
        SchemaRole::ReceiptPayload,
        composition_schemas,
    )?;
    Ok(InterfaceOperationView {
        module_id,
        name: descriptor.name(),
        input_schema,
        output_schema,
        receipt_kind: descriptor.receipt_kind(),
        receipt_schema,
    })
}

fn subscription_view<'a>(
    module_id: &'a str,
    descriptor: &'a SubscriptionDescriptor,
    composition_schemas: &'a HostCompositionManifest,
) -> Result<InterfaceSubscriptionView<'a>, HostError> {
    let payload_schema = resolve_schema_ref(
        module_id,
        None,
        descriptor.payload_schema_ref(),
        descriptor.required_payload_role(),
        composition_schemas,
    )?;
    Ok(InterfaceSubscriptionView {
        module_id,
        id: descriptor.id().as_str(),
        source: descriptor.source(),
        payload_schema,
        delivery: descriptor.delivery().as_str(),
        backpressure: backpressure_view(descriptor.backpressure()),
    })
}

fn backpressure_view(policy: BackpressurePolicy) -> BackpressureView {
    match policy {
        BackpressurePolicy::BoundedQueue { capacity } => BackpressureView {
            kind: policy.kind(),
            capacity: Some(capacity),
        },
    }
}

fn resolve_schema_ref<'a>(
    module: &str,
    operation: Option<&str>,
    reference: &str,
    role: SchemaRole,
    composition_schemas: &'a HostCompositionManifest,
) -> Result<SchemaIdentityView<'a>, HostError> {
    let matches = composition_schemas
        .schemas()
        .filter_map(|entry| {
            let descriptor = entry.descriptor();
            if descriptor.id().as_str() == reference && descriptor.role() == role {
                Some(descriptor)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [descriptor] => Ok(schema_identity_view(descriptor)),
        [] => Err(HostError::SchemaReferenceMissing {
            module: module.to_owned(),
            operation: operation.map(str::to_owned),
            reference: reference.to_owned(),
            role: role.to_string(),
        }),
        many => Err(HostError::SchemaReferenceAmbiguous {
            module: module.to_owned(),
            operation: operation.map(str::to_owned),
            reference: reference.to_owned(),
            role: role.to_string(),
            versions: many.iter().map(|schema| schema.version().get()).collect(),
        }),
    }
}

fn schema_identity_view(descriptor: &SchemaDescriptor) -> SchemaIdentityView<'_> {
    SchemaIdentityView {
        id: descriptor.id().as_str(),
        version: descriptor.version().get(),
        role: descriptor.role().as_str(),
        encoding: *descriptor.encoding().bytes(),
        shape: descriptor.shape(),
    }
}
