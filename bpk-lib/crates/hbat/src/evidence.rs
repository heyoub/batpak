//! `evidence.*` operation surface.
//!
//! Domain-neutral wire access to batpak's substrate evidence reports. Each
//! op is a thin adapter over an existing `Store` evidence method; no new
//! analysis happens here. Requests are keyed only on substrate coordinates
//! (event ids, regions, projection ids, commit-order). Acks carry the
//! report **body** as a canonical-encoding blob (`report_hex`) plus its
//! `body_hash` (evidence-report identity per `RECEIPTS.md`) and a
//! `truncated` flag — never a decoded domain payload.
//!
//! ## Why a report blob instead of a typed mirror
//!
//! `body_hash` is computed over the canonical bytes of the report body. A
//! field-by-field hbat mirror would re-encode and produce *different* bytes,
//! so the hash on the wire would not match the body on the wire. Shipping the
//! exact canonical body bytes preserves byte-exact evidence identity: a
//! consumer re-hashes `report_hex` and checks it equals `body_hash_hex`.

use batpak::coordinate::{Coordinate, Region};
use batpak::store::{ChainWalkRequest, ChainWalkStartRef, Freshness, ReadWalkRequest};
use batpak::EventPayload;
use netbat::decode_hex_str;
use serde::{Deserialize, Serialize};
use syncbat::{EffectClass, OperationDescriptor};

use crate::EventPayloadFixture;

/// Upper bound on walk-shaped evidence `limit` fields, matching
/// `event.walk`/`event.query`. Bounds `findings[]`/`proof_refs` growth so a
/// report body stays within the [`netbat::DEFAULT_MAX_OUTPUT_BYTES`] frame cap.
pub const EVIDENCE_MAX_LIMIT: u64 = 1024;

/// Reason an evidence wire request could not be mapped onto its substrate
/// request. Domain-neutral: only substrate-coordinate decode failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvidenceRequestError {
    /// A hex-encoded substrate coordinate did not decode.
    InvalidHex {
        /// Field whose hex failed to decode.
        field: &'static str,
        /// Human-readable decode error.
        message: String,
    },
    /// A bounded `limit` was zero.
    ZeroLimit,
    /// An `entity`/`scope` selector was not a valid substrate coordinate.
    InvalidCoordinate {
        /// Field whose coordinate validation failed.
        field: &'static str,
        /// Human-readable validation error.
        message: String,
    },
}

impl std::fmt::Display for EvidenceRequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidHex { field, message } => write!(f, "{field}: {message}"),
            Self::ZeroLimit => write!(f, "limit must be greater than 0"),
            Self::InvalidCoordinate { field, message } => write!(f, "{field}: {message}"),
        }
    }
}

impl std::error::Error for EvidenceRequestError {}

/// Decode a 32-char lowercase-hex `u128` substrate id.
fn decode_u128_hex(field: &'static str, hex: &str) -> Result<u128, EvidenceRequestError> {
    let bytes = decode_hex_str(hex).map_err(|error| EvidenceRequestError::InvalidHex {
        field,
        message: error.to_string(),
    })?;
    let array: [u8; 16] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| EvidenceRequestError::InvalidHex {
            field,
            message: format!("expected 16 bytes, got {}", bytes.len()),
        })?;
    Ok(u128::from_be_bytes(array))
}

/// Decode a 64-char lowercase-hex 32-byte content hash.
fn decode_hash_hex(field: &'static str, hex: &str) -> Result<[u8; 32], EvidenceRequestError> {
    let bytes = decode_hex_str(hex).map_err(|error| EvidenceRequestError::InvalidHex {
        field,
        message: error.to_string(),
    })?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| EvidenceRequestError::InvalidHex {
            field,
            message: format!("expected 32 bytes, got {}", bytes.len()),
        })
}

// ─── evidence.chain_walk ──────────────────────────────────────────────────────

/// Stable operation name for chain-walk evidence.
pub const EVIDENCE_CHAIN_WALK_OPERATION_NAME: &str = "evidence.chain_walk";
/// Schema reference for the request payload.
pub const EVIDENCE_CHAIN_WALK_INPUT_SCHEMA_REF: &str = "evidence.chain_walk.request";
/// Schema reference for the ack payload.
pub const EVIDENCE_CHAIN_WALK_OUTPUT_SCHEMA_REF: &str = "evidence.chain_walk.ack";
/// Receipt kind emitted for `evidence.chain_walk` calls.
pub const EVIDENCE_CHAIN_WALK_RECEIPT_KIND: &str = "receipt.evidence.chain_walk.v1";

/// Operation descriptor for `evidence.chain_walk`.
pub const EVIDENCE_CHAIN_WALK_DESCRIPTOR: OperationDescriptor = OperationDescriptor::new(
    EVIDENCE_CHAIN_WALK_OPERATION_NAME,
    EffectClass::Inspect,
    EVIDENCE_CHAIN_WALK_INPUT_SCHEMA_REF,
    EVIDENCE_CHAIN_WALK_OUTPUT_SCHEMA_REF,
    EVIDENCE_CHAIN_WALK_RECEIPT_KIND,
);

static EVIDENCE_CHAIN_WALK_DESCRIPTOR_STORAGE: OperationDescriptor = EVIDENCE_CHAIN_WALK_DESCRIPTOR;

fn evidence_chain_walk_descriptor() -> &'static OperationDescriptor {
    &EVIDENCE_CHAIN_WALK_DESCRIPTOR_STORAGE
}

inventory::submit! {
    crate::manifest::OperationDescriptorRegistration {
        descriptor: evidence_chain_walk_descriptor,
    }
}

/// Wire input for [`EVIDENCE_CHAIN_WALK_DESCRIPTOR`].
///
/// Maps onto [`batpak::store::ChainWalkRequest`] in `Linear` mode. A
/// `start_expected_hash_hex` of `Some` selects the receipt-anchored start ref;
/// `None` selects the bare event-id start ref.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0xA50)]
pub struct ChainWalkEvidenceRequest {
    /// Start `event_id` as 32-char lowercase hex.
    pub start_event_id_hex: String,
    /// Optional expected start chain hash (receipt-anchored start), 64-char hex.
    pub start_expected_hash_hex: Option<String>,
    /// Optional stop `event_id` as 32-char lowercase hex.
    pub end_event_id_hex: Option<String>,
    /// Maximum number of checked entries (bounded to [`EVIDENCE_MAX_LIMIT`]).
    pub limit: u64,
}

impl ChainWalkEvidenceRequest {
    /// Convert to the substrate [`ChainWalkRequest`], decoding hex coordinates
    /// and bounding `limit` to [`EVIDENCE_MAX_LIMIT`].
    ///
    /// # Errors
    /// Returns [`EvidenceRequestError`] when a hex coordinate fails to decode or
    /// `limit` is zero.
    pub fn to_core(&self) -> Result<ChainWalkRequest, EvidenceRequestError> {
        if self.limit == 0 {
            return Err(EvidenceRequestError::ZeroLimit);
        }
        let start_event_id = decode_u128_hex("start_event_id_hex", &self.start_event_id_hex)?;
        let start = match &self.start_expected_hash_hex {
            Some(hash_hex) => ChainWalkStartRef::Receipt {
                event_id: start_event_id,
                content_hash: decode_hash_hex("start_expected_hash_hex", hash_hex)?,
            },
            None => ChainWalkStartRef::EventId(start_event_id),
        };
        let end_event_id = match &self.end_event_id_hex {
            Some(hex) => Some(decode_u128_hex("end_event_id_hex", hex)?),
            None => None,
        };
        let bounded = self.limit.min(EVIDENCE_MAX_LIMIT);
        // bounded <= EVIDENCE_MAX_LIMIT (1024) so the usize cast cannot truncate.
        let limit = usize::try_from(bounded).unwrap_or(usize::MAX);
        Ok(ChainWalkRequest {
            start,
            end_event_id,
            limit,
            mode: batpak::store::ChainWalkMode::Linear,
        })
    }
}

/// Wire output for [`EVIDENCE_CHAIN_WALK_DESCRIPTOR`].
///
/// Carries the canonical report-body bytes plus their identity hash. See the
/// module docs for why the body is shipped as a blob rather than a typed mirror.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0xA51)]
pub struct ChainWalkEvidenceAck {
    /// Lowercase hex of the canonical-encoded report body.
    pub report_hex: String,
    /// Lowercase hex of the 32-byte report-body identity hash.
    pub body_hash_hex: String,
    /// True when the source walk reached its `limit` (more ancestry may exist).
    pub truncated: bool,
}

impl EventPayloadFixture for ChainWalkEvidenceRequest {
    fn fixture_value() -> Self {
        Self {
            start_event_id_hex: "0123456789abcdef0123456789abcdef".to_owned(),
            start_expected_hash_hex: None,
            end_event_id_hex: None,
            limit: 16,
        }
    }
}

impl EventPayloadFixture for ChainWalkEvidenceAck {
    fn fixture_value() -> Self {
        Self {
            report_hex: "a1b2c3d4".to_owned(),
            body_hash_hex:
                "0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
            truncated: false,
        }
    }
}

crate::hbat_event_descriptor! {
    type = ChainWalkEvidenceRequest,
    schema_ref = EVIDENCE_CHAIN_WALK_INPUT_SCHEMA_REF,
    ts_name = "ChainWalkEvidenceRequest",
    fields = [
        ("start_event_id_hex", "u128-hex"),
        ("start_expected_hash_hex", "option<blake3-32-hex>"),
        ("end_event_id_hex", "option<u128-hex>"),
        ("limit", "u64-safe-positive"),
    ],
}

crate::hbat_event_descriptor! {
    type = ChainWalkEvidenceAck,
    schema_ref = EVIDENCE_CHAIN_WALK_OUTPUT_SCHEMA_REF,
    ts_name = "ChainWalkEvidenceAck",
    fields = [
        ("report_hex", "hex-blob"),
        ("body_hash_hex", "blake3-32-hex"),
        ("truncated", "bool"),
    ],
}

// ─── evidence.store_resource ──────────────────────────────────────────────────

/// Stable operation name for the point-in-time store-resource snapshot.
pub const EVIDENCE_STORE_RESOURCE_OPERATION_NAME: &str = "evidence.store_resource";
/// Schema reference for the request payload.
pub const EVIDENCE_STORE_RESOURCE_INPUT_SCHEMA_REF: &str = "evidence.store_resource.request";
/// Schema reference for the ack payload.
pub const EVIDENCE_STORE_RESOURCE_OUTPUT_SCHEMA_REF: &str = "evidence.store_resource.ack";
/// Receipt kind emitted for `evidence.store_resource` calls.
pub const EVIDENCE_STORE_RESOURCE_RECEIPT_KIND: &str = "receipt.evidence.store_resource.v1";

/// Operation descriptor for `evidence.store_resource`.
pub const EVIDENCE_STORE_RESOURCE_DESCRIPTOR: OperationDescriptor = OperationDescriptor::new(
    EVIDENCE_STORE_RESOURCE_OPERATION_NAME,
    EffectClass::Inspect,
    EVIDENCE_STORE_RESOURCE_INPUT_SCHEMA_REF,
    EVIDENCE_STORE_RESOURCE_OUTPUT_SCHEMA_REF,
    EVIDENCE_STORE_RESOURCE_RECEIPT_KIND,
);

static EVIDENCE_STORE_RESOURCE_DESCRIPTOR_STORAGE: OperationDescriptor =
    EVIDENCE_STORE_RESOURCE_DESCRIPTOR;

fn evidence_store_resource_descriptor() -> &'static OperationDescriptor {
    &EVIDENCE_STORE_RESOURCE_DESCRIPTOR_STORAGE
}

inventory::submit! {
    crate::manifest::OperationDescriptorRegistration {
        descriptor: evidence_store_resource_descriptor,
    }
}

/// Wire input for [`EVIDENCE_STORE_RESOURCE_DESCRIPTOR`].
///
/// The store-resource snapshot takes no substrate coordinates; the request is
/// intentionally empty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0xA60)]
pub struct StoreResourceEvidenceRequest {}

/// Wire output for [`EVIDENCE_STORE_RESOURCE_DESCRIPTOR`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0xA61)]
pub struct StoreResourceEvidenceAck {
    /// Lowercase hex of the canonical-encoded report body.
    pub report_hex: String,
    /// Lowercase hex of the 32-byte report-body identity hash.
    pub body_hash_hex: String,
    /// Always `false`; the snapshot body is bounded by construction. Present so
    /// every `evidence.*` ack shares one wire shape.
    pub truncated: bool,
}

impl EventPayloadFixture for StoreResourceEvidenceRequest {
    fn fixture_value() -> Self {
        Self {}
    }
}

impl EventPayloadFixture for StoreResourceEvidenceAck {
    fn fixture_value() -> Self {
        Self {
            report_hex: "a1b2c3d4".to_owned(),
            body_hash_hex:
                "0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
            truncated: false,
        }
    }
}

crate::hbat_event_descriptor! {
    type = StoreResourceEvidenceRequest,
    schema_ref = EVIDENCE_STORE_RESOURCE_INPUT_SCHEMA_REF,
    ts_name = "StoreResourceEvidenceRequest",
    fields = [],
}

crate::hbat_event_descriptor! {
    type = StoreResourceEvidenceAck,
    schema_ref = EVIDENCE_STORE_RESOURCE_OUTPUT_SCHEMA_REF,
    ts_name = "StoreResourceEvidenceAck",
    fields = [
        ("report_hex", "hex-blob"),
        ("body_hash_hex", "blake3-32-hex"),
        ("truncated", "bool"),
    ],
}

// ─── evidence.read_walk ───────────────────────────────────────────────────────

/// Stable operation name for read-walk evidence.
pub const EVIDENCE_READ_WALK_OPERATION_NAME: &str = "evidence.read_walk";
/// Schema reference for the request payload.
pub const EVIDENCE_READ_WALK_INPUT_SCHEMA_REF: &str = "evidence.read_walk.request";
/// Schema reference for the ack payload.
pub const EVIDENCE_READ_WALK_OUTPUT_SCHEMA_REF: &str = "evidence.read_walk.ack";
/// Receipt kind emitted for `evidence.read_walk` calls.
pub const EVIDENCE_READ_WALK_RECEIPT_KIND: &str = "receipt.evidence.read_walk.v1";

/// Operation descriptor for `evidence.read_walk`.
pub const EVIDENCE_READ_WALK_DESCRIPTOR: OperationDescriptor = OperationDescriptor::new(
    EVIDENCE_READ_WALK_OPERATION_NAME,
    EffectClass::Inspect,
    EVIDENCE_READ_WALK_INPUT_SCHEMA_REF,
    EVIDENCE_READ_WALK_OUTPUT_SCHEMA_REF,
    EVIDENCE_READ_WALK_RECEIPT_KIND,
);

static EVIDENCE_READ_WALK_DESCRIPTOR_STORAGE: OperationDescriptor = EVIDENCE_READ_WALK_DESCRIPTOR;

fn evidence_read_walk_descriptor() -> &'static OperationDescriptor {
    &EVIDENCE_READ_WALK_DESCRIPTOR_STORAGE
}

inventory::submit! {
    crate::manifest::OperationDescriptorRegistration {
        descriptor: evidence_read_walk_descriptor,
    }
}

/// Wire input for [`EVIDENCE_READ_WALK_DESCRIPTOR`].
///
/// Selects a region by optional `entity` prefix and/or exact `scope`. Maps onto
/// [`batpak::store::ReadWalkRequest`] with `Freshness::Consistent` intent (v1
/// read walks always sample current visible state).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0xA70)]
pub struct ReadWalkEvidenceRequest {
    /// Optional entity namespace prefix selector.
    pub entity: Option<String>,
    /// Optional exact scope selector.
    pub scope: Option<String>,
    /// Optional output limit (bounded to [`EVIDENCE_MAX_LIMIT`]).
    pub limit: Option<u64>,
    /// Include deterministic proof refs for returned entries.
    pub include_proof_refs: bool,
}

impl ReadWalkEvidenceRequest {
    /// Convert to the substrate [`ReadWalkRequest`], validating coordinates and
    /// bounding `limit` to [`EVIDENCE_MAX_LIMIT`].
    ///
    /// # Errors
    /// Returns [`EvidenceRequestError`] when an `entity`/`scope` selector is not
    /// a valid coordinate.
    pub fn to_core(&self) -> Result<ReadWalkRequest, EvidenceRequestError> {
        if let Some(entity) = self.entity.as_deref() {
            Coordinate::new(entity, "hbat:evidence-read-walk").map_err(|error| {
                EvidenceRequestError::InvalidCoordinate {
                    field: "entity",
                    message: error.to_string(),
                }
            })?;
        }
        if let Some(scope) = self.scope.as_deref() {
            Coordinate::new("hbat:evidence-read-walk", scope).map_err(|error| {
                EvidenceRequestError::InvalidCoordinate {
                    field: "scope",
                    message: error.to_string(),
                }
            })?;
        }

        let mut region = self.entity.as_deref().map_or_else(Region::all, Region::entity);
        if let Some(scope) = self.scope.as_deref() {
            region = region.with_scope(scope);
        }

        let limit = self.limit.map(|value| {
            let bounded = value.min(EVIDENCE_MAX_LIMIT);
            usize::try_from(bounded).unwrap_or(usize::MAX)
        });

        Ok(ReadWalkRequest {
            region,
            limit,
            include_proof_refs: self.include_proof_refs,
            freshness_intent: Freshness::Consistent,
        })
    }
}

/// Wire output for [`EVIDENCE_READ_WALK_DESCRIPTOR`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0xA71)]
pub struct ReadWalkEvidenceAck {
    /// Lowercase hex of the canonical-encoded report body.
    pub report_hex: String,
    /// Lowercase hex of the 32-byte report-body identity hash.
    pub body_hash_hex: String,
    /// True when matched entries exceeded the returned set (limit dropped some).
    pub truncated: bool,
}

impl EventPayloadFixture for ReadWalkEvidenceRequest {
    fn fixture_value() -> Self {
        Self {
            entity: Some("fixture:bank".to_owned()),
            scope: None,
            limit: Some(64),
            include_proof_refs: false,
        }
    }
}

impl EventPayloadFixture for ReadWalkEvidenceAck {
    fn fixture_value() -> Self {
        Self {
            report_hex: "a1b2c3d4".to_owned(),
            body_hash_hex:
                "0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
            truncated: false,
        }
    }
}

crate::hbat_event_descriptor! {
    type = ReadWalkEvidenceRequest,
    schema_ref = EVIDENCE_READ_WALK_INPUT_SCHEMA_REF,
    ts_name = "ReadWalkEvidenceRequest",
    fields = [
        ("entity", "option<string>"),
        ("scope", "option<string>"),
        ("limit", "option<u64-safe>"),
        ("include_proof_refs", "bool"),
    ],
}

crate::hbat_event_descriptor! {
    type = ReadWalkEvidenceAck,
    schema_ref = EVIDENCE_READ_WALK_OUTPUT_SCHEMA_REF,
    ts_name = "ReadWalkEvidenceAck",
    fields = [
        ("report_hex", "hex-blob"),
        ("body_hash_hex", "blake3-32-hex"),
        ("truncated", "bool"),
    ],
}

// ─── evidence.projection_run ──────────────────────────────────────────────────

/// Stable operation name for projection-run evidence.
pub const EVIDENCE_PROJECTION_RUN_OPERATION_NAME: &str = "evidence.projection_run";
/// Schema reference for the request payload.
pub const EVIDENCE_PROJECTION_RUN_INPUT_SCHEMA_REF: &str = "evidence.projection_run.request";
/// Schema reference for the ack payload.
pub const EVIDENCE_PROJECTION_RUN_OUTPUT_SCHEMA_REF: &str = "evidence.projection_run.ack";
/// Receipt kind emitted for `evidence.projection_run` calls.
pub const EVIDENCE_PROJECTION_RUN_RECEIPT_KIND: &str = "receipt.evidence.projection_run.v1";

/// Operation descriptor for `evidence.projection_run`.
pub const EVIDENCE_PROJECTION_RUN_DESCRIPTOR: OperationDescriptor = OperationDescriptor::new(
    EVIDENCE_PROJECTION_RUN_OPERATION_NAME,
    EffectClass::Inspect,
    EVIDENCE_PROJECTION_RUN_INPUT_SCHEMA_REF,
    EVIDENCE_PROJECTION_RUN_OUTPUT_SCHEMA_REF,
    EVIDENCE_PROJECTION_RUN_RECEIPT_KIND,
);

static EVIDENCE_PROJECTION_RUN_DESCRIPTOR_STORAGE: OperationDescriptor =
    EVIDENCE_PROJECTION_RUN_DESCRIPTOR;

fn evidence_projection_run_descriptor() -> &'static OperationDescriptor {
    &EVIDENCE_PROJECTION_RUN_DESCRIPTOR_STORAGE
}

inventory::submit! {
    crate::manifest::OperationDescriptorRegistration {
        descriptor: evidence_projection_run_descriptor,
    }
}

/// Wire input for [`EVIDENCE_PROJECTION_RUN_DESCRIPTOR`].
///
/// Keyed on a domain-neutral `projection` id (resolved by the host's
/// [`batpak::store::ProjectionEvidenceRegistry`]) plus the substrate `entity`
/// coordinate. `max_stale_ms` of `None` requests consistent replay; `Some`
/// permits stale-allowed output within that age bound.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0xA80)]
pub struct ProjectionRunEvidenceRequest {
    /// Stable projection id registered with the host.
    pub projection: String,
    /// Entity coordinate to fold the projection over.
    pub entity: String,
    /// Optional stale-allowance bound in milliseconds; `None` is consistent.
    pub max_stale_ms: Option<u64>,
}

impl ProjectionRunEvidenceRequest {
    /// Map `max_stale_ms` onto a substrate [`Freshness`] policy.
    #[must_use]
    pub fn freshness(&self) -> Freshness {
        match self.max_stale_ms {
            Some(max_stale_ms) => Freshness::MaybeStale { max_stale_ms },
            None => Freshness::Consistent,
        }
    }
}

/// Wire output for [`EVIDENCE_PROJECTION_RUN_DESCRIPTOR`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0xA81)]
pub struct ProjectionRunEvidenceAck {
    /// Lowercase hex of the canonical-encoded report body.
    pub report_hex: String,
    /// Lowercase hex of the 32-byte report-body identity hash.
    pub body_hash_hex: String,
    /// Always `false`; the projection-run report body is bounded by construction.
    pub truncated: bool,
}

impl EventPayloadFixture for ProjectionRunEvidenceRequest {
    fn fixture_value() -> Self {
        Self {
            projection: "fixture.projection".to_owned(),
            entity: "fixture:bank".to_owned(),
            max_stale_ms: None,
        }
    }
}

impl EventPayloadFixture for ProjectionRunEvidenceAck {
    fn fixture_value() -> Self {
        Self {
            report_hex: "a1b2c3d4".to_owned(),
            body_hash_hex:
                "0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
            truncated: false,
        }
    }
}

crate::hbat_event_descriptor! {
    type = ProjectionRunEvidenceRequest,
    schema_ref = EVIDENCE_PROJECTION_RUN_INPUT_SCHEMA_REF,
    ts_name = "ProjectionRunEvidenceRequest",
    fields = [
        ("projection", "string"),
        ("entity", "string"),
        ("max_stale_ms", "option<u64-safe>"),
    ],
}

crate::hbat_event_descriptor! {
    type = ProjectionRunEvidenceAck,
    schema_ref = EVIDENCE_PROJECTION_RUN_OUTPUT_SCHEMA_REF,
    ts_name = "ProjectionRunEvidenceAck",
    fields = [
        ("report_hex", "hex-blob"),
        ("body_hash_hex", "blake3-32-hex"),
        ("truncated", "bool"),
    ],
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    #[test]
    fn chain_walk_request_fixture_roundtrips() -> Result<()> {
        let value = ChainWalkEvidenceRequest::fixture_value();
        let bytes = batpak::encoding::to_bytes(&value)?;
        let decoded: ChainWalkEvidenceRequest = batpak::encoding::from_bytes(&bytes)?;
        assert_eq!(decoded, value);
        Ok(())
    }

    #[test]
    fn chain_walk_ack_fixture_roundtrips() -> Result<()> {
        let value = ChainWalkEvidenceAck::fixture_value();
        let bytes = batpak::encoding::to_bytes(&value)?;
        let decoded: ChainWalkEvidenceAck = batpak::encoding::from_bytes(&bytes)?;
        assert_eq!(decoded, value);
        Ok(())
    }

    #[test]
    fn chain_walk_request_maps_event_id_start() -> Result<()> {
        let core = ChainWalkEvidenceRequest::fixture_value().to_core()?;
        assert!(matches!(core.start, ChainWalkStartRef::EventId(_)));
        assert_eq!(core.limit, 16);
        assert_eq!(core.mode, batpak::store::ChainWalkMode::Linear);
        Ok(())
    }

    #[test]
    fn chain_walk_request_maps_receipt_start_when_hash_present() -> Result<()> {
        let request = ChainWalkEvidenceRequest {
            start_expected_hash_hex: Some(
                "0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
            ),
            ..ChainWalkEvidenceRequest::fixture_value()
        };
        let core = request.to_core()?;
        assert!(matches!(core.start, ChainWalkStartRef::Receipt { .. }));
        Ok(())
    }

    #[test]
    fn chain_walk_request_bounds_limit() -> Result<()> {
        let request = ChainWalkEvidenceRequest {
            limit: EVIDENCE_MAX_LIMIT * 4,
            ..ChainWalkEvidenceRequest::fixture_value()
        };
        let core = request.to_core()?;
        assert_eq!(core.limit, EVIDENCE_MAX_LIMIT as usize);
        Ok(())
    }

    #[test]
    fn chain_walk_request_rejects_zero_limit() {
        let request = ChainWalkEvidenceRequest {
            limit: 0,
            ..ChainWalkEvidenceRequest::fixture_value()
        };
        assert_eq!(request.to_core().unwrap_err(), EvidenceRequestError::ZeroLimit);
    }

    #[test]
    fn store_resource_request_fixture_roundtrips() -> Result<()> {
        let value = StoreResourceEvidenceRequest::fixture_value();
        let bytes = batpak::encoding::to_bytes(&value)?;
        let decoded: StoreResourceEvidenceRequest = batpak::encoding::from_bytes(&bytes)?;
        assert_eq!(decoded, value);
        Ok(())
    }

    #[test]
    fn store_resource_ack_fixture_roundtrips() -> Result<()> {
        let value = StoreResourceEvidenceAck::fixture_value();
        let bytes = batpak::encoding::to_bytes(&value)?;
        let decoded: StoreResourceEvidenceAck = batpak::encoding::from_bytes(&bytes)?;
        assert_eq!(decoded, value);
        Ok(())
    }

    #[test]
    fn read_walk_request_fixture_roundtrips() -> Result<()> {
        let value = ReadWalkEvidenceRequest::fixture_value();
        let bytes = batpak::encoding::to_bytes(&value)?;
        let decoded: ReadWalkEvidenceRequest = batpak::encoding::from_bytes(&bytes)?;
        assert_eq!(decoded, value);
        Ok(())
    }

    #[test]
    fn read_walk_ack_fixture_roundtrips() -> Result<()> {
        let value = ReadWalkEvidenceAck::fixture_value();
        let bytes = batpak::encoding::to_bytes(&value)?;
        let decoded: ReadWalkEvidenceAck = batpak::encoding::from_bytes(&bytes)?;
        assert_eq!(decoded, value);
        Ok(())
    }

    #[test]
    fn read_walk_request_maps_region_and_bounds_limit() -> Result<()> {
        let request = ReadWalkEvidenceRequest {
            entity: Some("fixture:bank".to_owned()),
            scope: Some("fixture-scope".to_owned()),
            limit: Some(EVIDENCE_MAX_LIMIT * 4),
            include_proof_refs: true,
        };
        let core = request.to_core()?;
        assert_eq!(core.limit, Some(EVIDENCE_MAX_LIMIT as usize));
        assert!(core.include_proof_refs);
        Ok(())
    }

    #[test]
    fn read_walk_request_allows_unbounded_region() -> Result<()> {
        let request = ReadWalkEvidenceRequest {
            entity: None,
            scope: None,
            limit: None,
            include_proof_refs: false,
        };
        let core = request.to_core()?;
        assert_eq!(core.limit, None);
        Ok(())
    }

    #[test]
    fn projection_run_request_fixture_roundtrips() -> Result<()> {
        let value = ProjectionRunEvidenceRequest::fixture_value();
        let bytes = batpak::encoding::to_bytes(&value)?;
        let decoded: ProjectionRunEvidenceRequest = batpak::encoding::from_bytes(&bytes)?;
        assert_eq!(decoded, value);
        Ok(())
    }

    #[test]
    fn projection_run_ack_fixture_roundtrips() -> Result<()> {
        let value = ProjectionRunEvidenceAck::fixture_value();
        let bytes = batpak::encoding::to_bytes(&value)?;
        let decoded: ProjectionRunEvidenceAck = batpak::encoding::from_bytes(&bytes)?;
        assert_eq!(decoded, value);
        Ok(())
    }

    #[test]
    fn projection_run_request_maps_freshness() {
        let consistent = ProjectionRunEvidenceRequest::fixture_value();
        assert!(matches!(consistent.freshness(), Freshness::Consistent));
        let stale = ProjectionRunEvidenceRequest {
            max_stale_ms: Some(250),
            ..ProjectionRunEvidenceRequest::fixture_value()
        };
        assert!(matches!(
            stale.freshness(),
            Freshness::MaybeStale { max_stale_ms: 250 }
        ));
    }
}
