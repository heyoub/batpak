//! Deterministic generated-client manifest projection.

use serde::Serialize;

use crate::host::Host;
use crate::schema::{GoldenVector, SchemaDescriptor};
use crate::subscription::{SubscriptionDescriptor, SubscriptionSource, SUBSCRIPTION_WIRE_REQUIRES};

const CLIENT_MANIFEST_VERSION: u16 = 2;
const NETBAT_VERSION: &str = "NETBAT/1";
const CANONICAL_ENCODING_KIND: &str = "named-field-msgpack";
/// Stable wire-encoding contract version — NOT the `rmp-serde` crate patch, so a
/// transitive encoder bump that changes no wire bytes does not churn the manifest.
const WIRE_ENCODING_CONTRACT_VERSION: &str = "v1";

/// Generated-client contract projected from a live [`Host`].
///
/// This is not a hand-authored SDK manifest. It is a deterministic view of the
/// host's mounted operation descriptors, client-visible interface fingerprint,
/// and composition schema manifest. Schema golden vectors are exported as hex so
/// code generators and parity harnesses can treat committed Rust bytes as the
/// source of truth.
///
/// NOTE: this is the Rust-side projection of the interface contract. Generated
/// non-Rust client surfaces are deferred post-1.0; external consumers should
/// treat this manifest and its golden vectors as the wire contract source of
/// truth.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientManifest {
    /// Manifest schema version for this hostbat client projection.
    pub manifest_version: u16,
    /// Wire protocol version understood by generated clients for callable operations.
    pub netbat_version: &'static str,
    /// Streaming transport required to serve declared subscriptions (not yet implemented).
    pub subscription_wire_requires: &'static str,
    /// Hostbat crate version that emitted this manifest.
    pub hostbat_version: &'static str,
    /// Canonical byte encoding used by golden vectors.
    pub canonical_encoding: ClientManifestEncoding,
    /// Client-visible interface fingerprint.
    pub interface_fingerprint_hex: String,
    /// Exported operations in canonical operation-name order.
    pub operations: Vec<ClientManifestOperation>,
    /// Exported subscriptions in canonical `(module-id, subscription-id)` order.
    pub subscriptions: Vec<ClientManifestSubscription>,
    /// Exported schemas in canonical `(id, version, role)` order.
    pub schemas: Vec<ClientManifestSchema>,
}

impl ClientManifest {
    /// Project a generated-client manifest from a live host contract.
    #[must_use]
    pub fn from_host(host: &Host) -> Self {
        let operations = host
            .operations()
            .map(|descriptor| ClientManifestOperation {
                name: descriptor.name().to_owned(),
                effect: descriptor.effect.as_str().to_owned(),
                input_schema_ref: descriptor.input_schema_ref().to_owned(),
                output_schema_ref: descriptor.output_schema_ref().to_owned(),
                receipt_kind: descriptor.receipt_kind().to_owned(),
            })
            .collect();
        let subscriptions = host
            .subscriptions()
            .map(|(module_id, descriptor)| {
                ClientManifestSubscription::from_descriptor(module_id, descriptor)
            })
            .collect();
        let schemas = host
            .composition_schemas()
            .schemas()
            .map(|entry| ClientManifestSchema::from_schema(entry.descriptor()))
            .collect();
        Self {
            manifest_version: CLIENT_MANIFEST_VERSION,
            netbat_version: NETBAT_VERSION,
            subscription_wire_requires: SUBSCRIPTION_WIRE_REQUIRES,
            hostbat_version: env!("CARGO_PKG_VERSION"),
            canonical_encoding: ClientManifestEncoding {
                kind: CANONICAL_ENCODING_KIND,
                contract_version: WIRE_ENCODING_CONTRACT_VERSION,
            },
            interface_fingerprint_hex: host.interface_fingerprint().to_hex(),
            operations,
            subscriptions,
            schemas,
        }
    }
}

/// Canonical encoding descriptor for manifest golden bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientManifestEncoding {
    /// Stable encoding family.
    pub kind: &'static str,
    /// Stable wire-encoding contract version (not the rmp-serde crate patch).
    pub contract_version: &'static str,
}

/// Operation exported to generated clients.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientManifestOperation {
    /// Stable operation name.
    pub name: String,
    /// Coarse effect class spelling.
    pub effect: String,
    /// Referenced operation-input schema id.
    pub input_schema_ref: String,
    /// Referenced operation-output schema id.
    pub output_schema_ref: String,
    /// Receipt kind emitted by this operation.
    pub receipt_kind: String,
}

/// Subscription exported to generated clients.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientManifestSubscription {
    /// Owning module id.
    pub module_id: String,
    /// Globally unique subscription id.
    pub id: String,
    /// Source axis for the subscription stream.
    pub source: SubscriptionSource,
    /// Referenced payload schema id.
    pub payload_schema_ref: String,
    /// Required payload schema role spelling.
    pub payload_schema_role: String,
    /// Declared delivery semantics spelling.
    pub delivery: String,
    /// Declared backpressure policy kind.
    pub backpressure_kind: String,
    /// Bounded-queue capacity when applicable.
    pub backpressure_capacity: Option<u32>,
}

impl ClientManifestSubscription {
    fn from_descriptor(module_id: &str, descriptor: &SubscriptionDescriptor) -> Self {
        let backpressure = descriptor.backpressure();
        let (backpressure_kind, backpressure_capacity) = match backpressure {
            crate::subscription::BackpressurePolicy::BoundedQueue { capacity } => {
                (backpressure.kind().to_owned(), Some(capacity))
            }
        };
        Self {
            module_id: module_id.to_owned(),
            id: descriptor.id().as_str().to_owned(),
            source: descriptor.source().clone(),
            payload_schema_ref: descriptor.payload_schema_ref().to_owned(),
            payload_schema_role: descriptor.required_payload_role().as_str().to_owned(),
            delivery: descriptor.delivery().as_str().to_owned(),
            backpressure_kind,
            backpressure_capacity,
        }
    }
}

/// Schema exported to generated clients.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientManifestSchema {
    /// Stable schema id.
    pub id: String,
    /// Monotonic schema version.
    pub version: u32,
    /// Schema role spelling.
    pub role: String,
    /// Content hash pinning the schema declaration and golden vectors.
    pub encoding_hex: String,
    /// Informational Rust type path, if the schema recorded one.
    pub diagnostic_rust_type: Option<String>,
    /// Committed canonical bytes for this schema.
    pub golden: Vec<ClientManifestGoldenVector>,
}

impl ClientManifestSchema {
    fn from_schema(schema: &SchemaDescriptor) -> Self {
        Self {
            id: schema.id().as_str().to_owned(),
            version: schema.version().get(),
            role: schema.role().as_str().to_owned(),
            encoding_hex: schema.encoding().to_hex(),
            diagnostic_rust_type: schema
                .diagnostic_rust_type()
                .map(|rust_type| rust_type.as_str().to_owned()),
            golden: schema
                .golden()
                .map(ClientManifestGoldenVector::from)
                .collect(),
        }
    }
}

/// One committed schema golden vector.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientManifestGoldenVector {
    /// Stable golden-vector case name.
    pub case: String,
    /// Canonical bytes encoded as lowercase hex.
    pub bytes_hex: String,
}

impl From<&GoldenVector> for ClientManifestGoldenVector {
    fn from(golden: &GoldenVector) -> Self {
        Self {
            case: golden.case.clone(),
            bytes_hex: hex(&golden.bytes),
        }
    }
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
