//! Deterministic generated-client manifest projection.

use serde::Serialize;

use crate::host::Host;
use crate::schema::{GoldenVector, SchemaDescriptor};

const CLIENT_MANIFEST_VERSION: u16 = 1;
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
/// NOTE: this is the Rust-side projection of the interface contract. Wiring it
/// into the `bpk-ts` code generator + byte-parity harness (re-pointing them off
/// the legacy event-field manifest) is a deferred follow-up; until then the
/// TypeScript client is not regenerated from this shape.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientManifest {
    /// Manifest schema version for this hostbat client projection.
    pub manifest_version: u16,
    /// Wire protocol version understood by generated clients.
    pub netbat_version: &'static str,
    /// Hostbat crate version that emitted this manifest.
    pub hostbat_version: &'static str,
    /// Canonical byte encoding used by golden vectors.
    pub canonical_encoding: ClientManifestEncoding,
    /// Client-visible interface fingerprint.
    pub interface_fingerprint_hex: String,
    /// Exported operations in canonical operation-name order.
    pub operations: Vec<ClientManifestOperation>,
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
        let schemas = host
            .composition_schemas()
            .schemas()
            .map(|entry| ClientManifestSchema::from_schema(entry.descriptor()))
            .collect();
        Self {
            manifest_version: CLIENT_MANIFEST_VERSION,
            netbat_version: NETBAT_VERSION,
            hostbat_version: env!("CARGO_PKG_VERSION"),
            canonical_encoding: ClientManifestEncoding {
                kind: CANONICAL_ENCODING_KIND,
                contract_version: WIRE_ENCODING_CONTRACT_VERSION,
            },
            interface_fingerprint_hex: host.interface_fingerprint().to_hex(),
            operations,
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
