//! `evidence.*` operation surface.
//!
//! Domain-neutral wire access to batpak's substrate evidence reports. Each
//! op is a thin adapter over an existing `Store` evidence method; no new
//! analysis happens here. Requests use domain-neutral substrate selectors
//! (entity/scope prefixes, optional kind filters, optional per-entity clock
//! range on read walks, projection ids, event-id hex on chain walks). Acks
//! carry the report **body** as a canonical-encoding blob (`report_hex`) plus
//! its `body_hash` (evidence-report identity per `RECEIPTS.md`) and a
//! `truncated` flag — never a decoded domain payload.
//!
//! ## Why a report blob instead of a typed mirror
//!
//! `body_hash` is computed over the canonical bytes of the report body. A
//! field-by-field hbat mirror would re-encode and produce *different* bytes,
//! so the hash on the wire would not match the body on the wire. Shipping the
//! exact canonical body bytes preserves byte-exact evidence identity: a
//! consumer re-hashes `report_hex` and checks it equals `body_hash_hex`.

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

/// Tighter `limit` bound for read walks that request proof refs. Each returned
/// entry then contributes a proof ref (event id + global sequence + 32-byte
/// hash) to the report body, which is hex-expanded into the ack; without proof
/// refs the body is `O(1)` in the result count, so the looser
/// [`EVIDENCE_MAX_LIMIT`] applies. This keeps the worst-case proof-ref body well
/// under [`netbat::DEFAULT_MAX_OUTPUT_BYTES`]; the handler also guards the
/// encoded size as a backstop.
pub const EVIDENCE_READ_WALK_PROOF_MAX_LIMIT: u64 = 64;

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
    /// A kind filter axis was invalid.
    InvalidKind {
        /// Field whose kind validation failed.
        field: &'static str,
        /// Human-readable validation error.
        message: String,
    },
    /// A clock-range axis was invalid.
    InvalidClockRange {
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
            Self::InvalidKind { field, message } => write!(f, "{field}: {message}"),
            Self::InvalidClockRange { message } => write!(f, "{message}"),
        }
    }
}

impl EvidenceRequestError {
    fn from_wire_region(error: crate::region_wire::WireRegionError) -> Self {
        match error {
            crate::region_wire::WireRegionError::InvalidCoordinate { field, message } => {
                Self::InvalidCoordinate { field, message }
            }
            crate::region_wire::WireRegionError::InvalidKind { field, message } => {
                Self::InvalidKind { field, message }
            }
            crate::region_wire::WireRegionError::InvalidClockRange { message } => {
                Self::InvalidClockRange { message }
            }
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
    let array: [u8; 16] =
        bytes
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
            body_hash_hex: "0000000000000000000000000000000000000000000000000000000000000000"
                .to_owned(),
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
            body_hash_hex: "0000000000000000000000000000000000000000000000000000000000000000"
                .to_owned(),
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
/// Selects a [`batpak::coordinate::Region`] by optional entity prefix, exact
/// scope, kind filters, and per-entity clock range. Maps onto
/// [`batpak::store::ReadWalkRequest`] with caller-declared freshness intent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0xA70)]
pub struct ReadWalkEvidenceRequest {
    /// Optional entity namespace prefix selector.
    pub entity: Option<String>,
    /// Optional exact scope selector.
    pub scope: Option<String>,
    /// Optional event-kind category filter.
    pub kind_category: Option<u8>,
    /// Optional event-kind type id. Requires `kind_category` when present.
    pub kind_type_id: Option<u16>,
    /// Optional inclusive per-entity clock range start.
    pub start_clock: Option<u32>,
    /// Optional inclusive per-entity clock range end.
    pub end_clock: Option<u32>,
    /// Optional output limit. Bounded to [`EVIDENCE_MAX_LIMIT`] (or the tighter
    /// [`EVIDENCE_READ_WALK_PROOF_MAX_LIMIT`] when `include_proof_refs` is set);
    /// omitting it applies that same bound rather than scanning the whole region.
    pub limit: Option<u64>,
    /// Include deterministic proof refs for returned entries.
    pub include_proof_refs: bool,
    /// Optional stale-allowance bound in milliseconds; `None` is consistent.
    pub max_stale_ms: Option<u64>,
}

impl ReadWalkEvidenceRequest {
    /// Convert to the substrate [`ReadWalkRequest`], validating region axes and
    /// bounding `limit` to [`EVIDENCE_MAX_LIMIT`].
    ///
    /// # Errors
    /// Returns [`EvidenceRequestError`] when region axes or `limit` are invalid.
    pub fn to_core(&self) -> Result<ReadWalkRequest, EvidenceRequestError> {
        if self.limit == Some(0) {
            return Err(EvidenceRequestError::ZeroLimit);
        }

        let region = crate::region_wire::wire_axes_to_region(
            self.entity.as_deref(),
            self.scope.as_deref(),
            self.kind_category,
            self.kind_type_id,
            self.start_clock,
            self.end_clock,
        )
        .map_err(EvidenceRequestError::from_wire_region)?;

        // Always bound the read, even when `limit` is omitted: an unbounded
        // request makes the store materialize every matching entry (work
        // proportional to the whole store) before producing an otherwise small
        // report. Proof refs additionally add a per-entry hash to the body, so
        // they get a tighter bound to keep the hex-expanded ack within frame.
        let max_limit = if self.include_proof_refs {
            EVIDENCE_READ_WALK_PROOF_MAX_LIMIT
        } else {
            EVIDENCE_MAX_LIMIT
        };
        let bounded_limit = self.limit.map_or(max_limit, |value| value.min(max_limit));
        let limit = Some(usize::try_from(bounded_limit).unwrap_or(usize::MAX));

        let freshness_intent = match self.max_stale_ms {
            Some(max_stale_ms) => Freshness::MaybeStale { max_stale_ms },
            None => Freshness::Consistent,
        };

        Ok(ReadWalkRequest {
            region,
            limit,
            include_proof_refs: self.include_proof_refs,
            freshness_intent,
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
    /// True only when the `limit` dropped matching entries (more results exist).
    /// Degraded reports (e.g. an entry missing its backing index) are not
    /// truncation and leave this `false`.
    pub truncated: bool,
}

impl EventPayloadFixture for ReadWalkEvidenceRequest {
    fn fixture_value() -> Self {
        Self {
            entity: Some("fixture:bank".to_owned()),
            scope: None,
            kind_category: Some(0xF),
            kind_type_id: None,
            start_clock: None,
            end_clock: None,
            limit: Some(64),
            include_proof_refs: false,
            max_stale_ms: None,
        }
    }
}

impl EventPayloadFixture for ReadWalkEvidenceAck {
    fn fixture_value() -> Self {
        Self {
            report_hex: "a1b2c3d4".to_owned(),
            body_hash_hex: "0000000000000000000000000000000000000000000000000000000000000000"
                .to_owned(),
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
        ("kind_category", "option<u8>"),
        ("kind_type_id", "option<u16>"),
        ("start_clock", "option<u32>"),
        ("end_clock", "option<u32>"),
        ("limit", "option<u64-safe-positive>"),
        ("include_proof_refs", "bool"),
        ("max_stale_ms", "option<u64-safe>"),
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
            body_hash_hex: "0000000000000000000000000000000000000000000000000000000000000000"
                .to_owned(),
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
