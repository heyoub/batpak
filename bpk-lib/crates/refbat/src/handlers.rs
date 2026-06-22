//! Runtime handlers for [`crate::bank`] operations.
//!
//! These handlers capture an `Arc<batpak::store::Store>` so they can be
//! registered with [`syncbat::CoreBuilder::register`] from the `refbat`
//! binary. They live in their own module (not in `bank.rs`) because the
//! library half of `refbat` deliberately does NOT depend on a runtime
//! store handle — the descriptors and payload types are pure data and
//! must be linkable from `xtask` without dragging the runtime in.

use std::collections::BTreeMap;
use std::sync::Arc;

use batpak::coordinate::{Coordinate, Region};
use batpak::event::EventKind;
use batpak::id::EntityIdType;
use batpak::id::EventId;
use batpak::store::index::IndexEntry;
use batpak::store::{
    AppendOptions, AppendReceipt, BatchAppendItem, CausationRef, ChainWalkFinding, EncodedBytes,
    ExtensionKey, ProjectionEvidenceRegistry, ProjectionRunReportError, ReadWalkDroppedCount,
    ReceiptVerification, ReceiptVerificationError, Store,
};
// Hex codec is the canonical netbat implementation; refbat does not
// re-roll its own. See netbat::transport::hex.
use netbat::{decode_hex_str, encode_hex_str};
use syncbat::{Ctx, Handler, HandlerError, HandlerResult};

use crate::bank::{
    BankCommitAck, BankCommitRequest, EventGetAck, EventGetRequest, EventQueryAck,
    EventQueryRequest, EventSummary, EVENT_QUERY_MAX_LIMIT,
};
use crate::evidence::{
    ChainWalkEvidenceAck, ChainWalkEvidenceRequest, ProjectionRunEvidenceAck,
    ProjectionRunEvidenceRequest, ReadWalkEvidenceAck, ReadWalkEvidenceRequest,
    StoreResourceEvidenceAck, StoreResourceEvidenceRequest,
};
use crate::receipt::{ReceiptVerifyAck, ReceiptVerifyRequest};
use crate::walk::{EventWalkAck, EventWalkRequest, EVENT_WALK_MAX_LIMIT};

// ─── bank.commit handler ────────────────────────────────────────────────────

/// Handler binding for [`crate::bank::BANK_COMMIT_DESCRIPTOR`]. Captures
/// the runtime store handle.
pub struct BankCommitHandler {
    /// Shared handle to the BatPAK store. Cloning the `Arc` is cheap.
    pub store: Arc<Store>,
}

impl Handler for BankCommitHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut Ctx<'_>) -> HandlerResult {
        handle_bank_commit(&self.store, input)
    }
}

fn handle_bank_commit(store: &Store, input: &[u8]) -> HandlerResult {
    let request: BankCommitRequest = batpak::encoding::from_bytes(input)
        .map_err(|error| HandlerError::invalid_input(format!("decode request: {error}")))?;

    let coord = Coordinate::new(&request.entity, &request.scope)
        .map_err(|error| HandlerError::invalid_input(format!("coordinate: {error}")))?;

    let kind = EventKind::try_custom(request.kind_category, request.kind_type_id)
        .map_err(|error| HandlerError::invalid_input(format!("event kind: {error:?}")))?;

    let payload_bytes = decode_hex_str(&request.payload_hex)
        .map_err(|error| HandlerError::invalid_input(format!("payload_hex: {error}")))?;

    let mut options = AppendOptions::new();
    if let Some(idempotency_key_hex) = request.idempotency_key_hex.as_deref() {
        let raw = decode_event_id_hex(idempotency_key_hex).map_err(|error| {
            HandlerError::invalid_input(format!("idempotency_key_hex: {error}"))
        })?;
        options = options.with_idempotency(batpak::id::IdempotencyKey::from(raw));
    }

    let item = BatchAppendItem::from_msgpack_bytes(
        coord,
        kind,
        payload_bytes,
        options,
        CausationRef::None,
    );

    let receipts = store
        .append_batch(vec![item])
        .map_err(|error| HandlerError::failed(format!("append: {error}")))?;

    let receipt = receipts
        .into_iter()
        .next()
        .ok_or_else(|| HandlerError::failed("append returned no receipt"))?;

    let ack = append_receipt_to_ack(&receipt);
    batpak::encoding::to_bytes(&ack)
        .map_err(|error| HandlerError::failed(format!("encode ack: {error}")))
}

fn append_receipt_to_ack(receipt: &AppendReceipt) -> BankCommitAck {
    let extensions = receipt
        .extensions
        .iter()
        .map(|(key, value)| (key.as_str().to_owned(), encode_hex_str(value)))
        .collect();
    BankCommitAck {
        event_id_hex: format!("{:032x}", u128::from(receipt.event_id)),
        sequence: receipt.global_sequence,
        content_hash_hex: encode_hex_str(&receipt.content_hash),
        key_id_hex: encode_hex_str(&receipt.key_id),
        signature_hex: receipt.signature.map(|s| encode_hex_str(&s)),
        extensions,
    }
}

// ─── event.get handler ──────────────────────────────────────────────────────

/// Handler binding for [`crate::bank::EVENT_GET_DESCRIPTOR`].
pub struct EventGetHandler {
    /// Shared handle to the BatPAK store. Cloning the `Arc` is cheap.
    pub store: Arc<Store>,
}

impl Handler for EventGetHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut Ctx<'_>) -> HandlerResult {
        handle_event_get(&self.store, input)
    }
}

fn handle_event_get(store: &Store, input: &[u8]) -> HandlerResult {
    let request: EventGetRequest = batpak::encoding::from_bytes(input)
        .map_err(|error| HandlerError::invalid_input(format!("decode request: {error}")))?;

    let event_id = decode_event_id_hex(&request.event_id_hex)?;
    let typed_event_id = EventId::from(event_id);

    let stored = store
        .read_raw(typed_event_id)
        .map_err(|error| HandlerError::failed(format!("read event: {error}")))?;

    // Look up the IndexEntry to pull the real global_sequence. The
    // EventHeader does not carry sequence (sequence is assigned at
    // commit-time into the index), so we query the entity's region
    // and find the entry by event_id. O(N) over the entity but
    // correct — and EventGetAck.sequence is part of the declared
    // wire contract that consumers use for monotonic replay,
    // checkpointing, and dedup, so it must be truthful.
    let region = batpak::coordinate::Region::entity(stored.coordinate.entity());
    let sequence = store
        .query(&region)
        .into_iter()
        .find(|entry| entry.event_id() == typed_event_id)
        .map(|entry| entry.global_sequence())
        .ok_or_else(|| {
            HandlerError::failed(format!(
                "event_id {event_id:032x} was read_raw-able but missing from the index query"
            ))
        })?;

    let ack = EventGetAck {
        event_id_hex: format!("{:032x}", u128::from(stored.event.header.event_id)),
        sequence,
        timestamp_us: stored.event.header.timestamp_us,
        correlation_id_hex: format!("{:032x}", u128::from(stored.event.header.correlation_id)),
        causation_id_hex: stored
            .event
            .header
            .causation_id
            .map(|c| format!("{:032x}", u128::from(c))),
        kind_category: stored.event.header.event_kind.category(),
        kind_type_id: stored.event.header.event_kind.type_id(),
        entity: stored.coordinate.entity().to_owned(),
        scope: stored.coordinate.scope().to_owned(),
        payload_hex: encode_hex_str(&stored.event.payload),
        content_hash_hex: encode_hex_str(&stored.event.header.content_hash),
    };

    batpak::encoding::to_bytes(&ack)
        .map_err(|error| HandlerError::failed(format!("encode ack: {error}")))
}

// ─── event.query handler ────────────────────────────────────────────────────

/// Handler binding for [`crate::bank::EVENT_QUERY_DESCRIPTOR`].
pub struct EventQueryHandler {
    /// Shared handle to the BatPAK store. Cloning the `Arc` is cheap.
    pub store: Arc<Store>,
}

impl Handler for EventQueryHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut Ctx<'_>) -> HandlerResult {
        handle_event_query(&self.store, input)
    }
}

fn handle_event_query(store: &Store, input: &[u8]) -> HandlerResult {
    let request: EventQueryRequest = batpak::encoding::from_bytes(input)
        .map_err(|error| HandlerError::invalid_input(format!("decode request: {error}")))?;

    if request.limit == 0 {
        return Err(HandlerError::invalid_input("limit must be greater than 0"));
    }
    let bounded_limit = request.limit.min(EVENT_QUERY_MAX_LIMIT);
    let limit = usize::try_from(bounded_limit)
        .map_err(|error| HandlerError::invalid_input(format!("limit: {error}")))?;

    let region = event_query_region(&request)?;
    let entries = query_event_summaries(
        store,
        &region,
        request.after_global_sequence,
        limit.saturating_add(1),
    );
    let truncated = entries.len() > limit;
    let entries: Vec<EventSummary> = entries.into_iter().take(limit).collect();
    let next_after_global_sequence = entries.last().map(|summary| summary.global_sequence);

    let ack = EventQueryAck {
        entries,
        next_after_global_sequence,
        truncated,
    };

    batpak::encoding::to_bytes(&ack)
        .map_err(|error| HandlerError::failed(format!("encode ack: {error}")))
}

fn event_query_region(request: &EventQueryRequest) -> Result<Region, HandlerError> {
    crate::region_wire::wire_axes_to_region(
        request.entity.as_deref(),
        request.scope.as_deref(),
        request.kind_category,
        request.kind_type_id,
        None,
        None,
    )
    .map_err(|error| HandlerError::invalid_input(error.to_string()))
}

fn query_event_summaries(
    store: &Store,
    region: &Region,
    after_global_sequence: Option<u64>,
    limit: usize,
) -> Vec<EventSummary> {
    store
        .query_entries_after(region, after_global_sequence, limit)
        .into_iter()
        .map(|entry| index_entry_to_query_summary(&entry))
        .collect()
}

fn index_entry_to_query_summary(entry: &IndexEntry) -> EventSummary {
    EventSummary {
        event_id_hex: format!("{:032x}", entry.event_id().as_u128()),
        global_sequence: entry.global_sequence(),
        wall_ms: entry.wall_ms(),
        clock: entry.clock(),
        correlation_id_hex: format!("{:032x}", entry.correlation_id()),
        causation_id_hex: entry
            .causation_id()
            .map(|causation_id| format!("{causation_id:032x}")),
        kind_category: entry.event_kind().category(),
        kind_type_id: entry.event_kind().type_id(),
        entity: entry.coord().entity().to_owned(),
        scope: entry.coord().scope().to_owned(),
        content_hash_hex: encode_hex_str(&entry.hash_chain().event_hash),
    }
}

fn decode_event_id_hex(event_id_hex: &str) -> Result<u128, HandlerError> {
    let event_id_bytes = decode_hex_str(event_id_hex)
        .map_err(|error| HandlerError::invalid_input(format!("event_id_hex: {error}")))?;
    if event_id_bytes.len() != 16 {
        return Err(HandlerError::invalid_input(format!(
            "event_id_hex must decode to 16 bytes, got {}",
            event_id_bytes.len()
        )));
    }
    let mut be = [0_u8; 16];
    be.copy_from_slice(&event_id_bytes);
    Ok(u128::from_be_bytes(be))
}

fn decode_fixed_hex<const N: usize>(field: &str, hex: &str) -> Result<[u8; N], HandlerError> {
    let bytes = decode_hex_str(hex)
        .map_err(|error| HandlerError::invalid_input(format!("{field}: {error}")))?;
    if bytes.len() != N {
        return Err(HandlerError::invalid_input(format!(
            "{field} must decode to {N} bytes, got {}",
            bytes.len()
        )));
    }
    let mut out = [0_u8; N];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn decode_wire_extensions(
    wire: &BTreeMap<String, String>,
) -> Result<BTreeMap<ExtensionKey, EncodedBytes>, HandlerError> {
    let mut decoded = BTreeMap::new();
    for (key_str, value_hex) in wire {
        let key = ExtensionKey::new(key_str)
            .map_err(|error| HandlerError::invalid_input(format!("extension key: {error}")))?;
        let value = decode_hex_str(value_hex).map_err(|error| {
            HandlerError::invalid_input(format!("extension value hex: {error}"))
        })?;
        decoded.insert(key, value);
    }
    Ok(decoded)
}

fn summary_for_event_id(store: &Store, event_id: EventId) -> Result<EventSummary, HandlerError> {
    let stored = store
        .read_raw(event_id)
        .map_err(|error| HandlerError::failed(format!("read event: {error}")))?;
    let region = Region::entity(stored.coordinate.entity());
    let entry = store
        .query(&region)
        .into_iter()
        .find(|entry| entry.event_id() == event_id)
        .ok_or_else(|| {
            HandlerError::failed(format!(
                "event_id {:032x} was read_raw-able but missing from the index query",
                event_id.as_u128()
            ))
        })?;
    Ok(index_entry_to_query_summary(&entry))
}

fn receipt_verification_reason_code(error: &ReceiptVerificationError) -> &'static str {
    match error {
        ReceiptVerificationError::MissingCommittedEvent => "missing_committed_event",
        ReceiptVerificationError::EventIdMismatch => "event_id_mismatch",
        ReceiptVerificationError::SequenceMismatch => "sequence_mismatch",
        ReceiptVerificationError::DiskPositionMismatch => "disk_position_mismatch",
        ReceiptVerificationError::ContentHashMismatch => "content_hash_mismatch",
        ReceiptVerificationError::ExtensionsMismatch => "extensions_mismatch",
        ReceiptVerificationError::DenialKindMismatch => "denial_kind_mismatch",
        ReceiptVerificationError::UnsignedReceiptRejected => "unsigned_receipt_rejected",
        ReceiptVerificationError::MissingSignature => "missing_signature",
        ReceiptVerificationError::ZeroKeyWithSignature => "zero_key_with_signature",
        ReceiptVerificationError::UnknownSigningKey => "unknown_signing_key",
        ReceiptVerificationError::InvalidSignature => "invalid_signature",
        ReceiptVerificationError::CoverBuildFailed { .. } => "cover_build_failed",
    }
}

fn receipt_verification_to_ack(verification: ReceiptVerification) -> ReceiptVerifyAck {
    match verification {
        ReceiptVerification::Signed => ReceiptVerifyAck {
            valid: true,
            outcome: "signed".to_owned(),
            reason_code: None,
        },
        ReceiptVerification::UnsignedAccepted => ReceiptVerifyAck {
            valid: true,
            outcome: "unsigned_accepted".to_owned(),
            reason_code: None,
        },
        ReceiptVerification::Invalid(error) => ReceiptVerifyAck {
            valid: false,
            outcome: "invalid".to_owned(),
            reason_code: Some(receipt_verification_reason_code(&error).to_owned()),
        },
    }
}

// ─── receipt.verify handler ───────────────────────────────────────────────────

/// Handler binding for [`crate::receipt::RECEIPT_VERIFY_DESCRIPTOR`].
pub struct ReceiptVerifyHandler {
    /// Shared handle to the BatPAK store. Cloning the `Arc` is cheap.
    pub store: Arc<Store>,
}

impl Handler for ReceiptVerifyHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut Ctx<'_>) -> HandlerResult {
        handle_receipt_verify(&self.store, input)
    }
}

fn handle_receipt_verify(store: &Store, input: &[u8]) -> HandlerResult {
    let request: ReceiptVerifyRequest = batpak::encoding::from_bytes(input)
        .map_err(|error| HandlerError::invalid_input(format!("decode request: {error}")))?;

    let event_id_raw = decode_event_id_hex(&request.event_id_hex)?;
    let event_id = EventId::from(event_id_raw);
    let content_hash = decode_fixed_hex("content_hash_hex", &request.content_hash_hex)?;
    let key_id = decode_fixed_hex("key_id_hex", &request.key_id_hex)?;
    let signature = match request.signature_hex.as_deref() {
        None => None,
        Some(hex) => Some(decode_fixed_hex("signature_hex", hex)?),
    };
    let extensions = decode_wire_extensions(&request.extensions)?;

    let verification = store.verify_append_receipt_wire_detailed(
        event_id,
        request.sequence,
        content_hash,
        key_id,
        signature,
        extensions,
    );
    let ack = receipt_verification_to_ack(verification);
    batpak::encoding::to_bytes(&ack)
        .map_err(|error| HandlerError::failed(format!("encode ack: {error}")))
}

// ─── event.walk handler ───────────────────────────────────────────────────────

/// Handler binding for [`crate::walk::EVENT_WALK_DESCRIPTOR`].
pub struct EventWalkHandler {
    /// Shared handle to the BatPAK store. Cloning the `Arc` is cheap.
    pub store: Arc<Store>,
}

impl Handler for EventWalkHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut Ctx<'_>) -> HandlerResult {
        handle_event_walk(&self.store, input)
    }
}

fn handle_event_walk(store: &Store, input: &[u8]) -> HandlerResult {
    let request: EventWalkRequest = batpak::encoding::from_bytes(input)
        .map_err(|error| HandlerError::invalid_input(format!("decode request: {error}")))?;

    if request.limit == 0 {
        return Err(HandlerError::invalid_input("limit must be greater than 0"));
    }
    let bounded_limit = request.limit.min(EVENT_WALK_MAX_LIMIT);
    let limit = usize::try_from(bounded_limit)
        .map_err(|error| HandlerError::invalid_input(format!("limit: {error}")))?;

    let event_id_raw = decode_event_id_hex(&request.event_id_hex)?;
    let event_id = EventId::from(event_id_raw);
    let ancestors = store.walk_ancestors(event_id, limit);
    let entries: Vec<EventSummary> = ancestors
        .iter()
        .map(|stored| summary_for_event_id(store, stored.event.header.event_id))
        .collect::<Result<_, _>>()?;

    let ack = EventWalkAck { entries };
    batpak::encoding::to_bytes(&ack)
        .map_err(|error| HandlerError::failed(format!("encode ack: {error}")))
}

// ─── evidence.chain_walk handler ──────────────────────────────────────────────

/// Handler binding for [`crate::evidence::EVIDENCE_CHAIN_WALK_DESCRIPTOR`].
pub struct ChainWalkEvidenceHandler {
    /// Shared handle to the BatPAK store. Cloning the `Arc` is cheap.
    pub store: Arc<Store>,
}

impl Handler for ChainWalkEvidenceHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut Ctx<'_>) -> HandlerResult {
        handle_evidence_chain_walk(&self.store, input)
    }
}

fn handle_evidence_chain_walk(store: &Store, input: &[u8]) -> HandlerResult {
    let request: ChainWalkEvidenceRequest = batpak::encoding::from_bytes(input)
        .map_err(|error| HandlerError::invalid_input(format!("decode request: {error}")))?;

    let core_request = request
        .to_core()
        .map_err(|error| HandlerError::invalid_input(error.to_string()))?;

    let report = store
        .chain_walk_evidence(&core_request)
        .map_err(|error| HandlerError::failed(format!("chain walk evidence: {error}")))?;

    // Trust core's explicit truncation finding rather than inferring from the
    // checked count: a walk that ends exactly at the limit is NOT truncated.
    // Core emits `TruncatedByLimit` only when it stopped on the limit with a
    // parent edge still to follow.
    let truncated = report
        .body
        .findings
        .iter()
        .any(|finding| matches!(finding, ChainWalkFinding::TruncatedByLimit { .. }));
    let (report_hex, body_hash_hex) = encode_report_body(&report.body, &report.body_hash)?;
    let ack = ChainWalkEvidenceAck {
        report_hex,
        body_hash_hex,
        truncated,
    };
    finish_evidence_ack(&ack)
}

/// Encode an evidence report body to its canonical `(report_hex, body_hash_hex)`
/// wire pair.
///
/// `report_hex` is the exact byte material `body_hash` is computed over, so a
/// consumer can re-hash `report_hex` and confirm it equals `body_hash_hex`
/// (evidence-report identity per `RECEIPTS.md`).
fn encode_report_body<B: serde::Serialize>(
    body: &B,
    body_hash: &[u8; 32],
) -> Result<(String, String), HandlerError> {
    let body_bytes = batpak::encoding::to_bytes(body)
        .map_err(|error| HandlerError::failed(format!("encode report body: {error}")))?;
    Ok((encode_hex_str(&body_bytes), encode_hex_str(body_hash)))
}

/// Encode an evidence ack and reject it if it would overrun the NETBAT output
/// frame cap, with a deterministic, domain-neutral error instead of an opaque
/// transport `OutputTooLarge`. A backstop: bounded `limit`s keep report bodies
/// small in the normal case, but a pathological report (e.g. many proof refs or
/// findings) must fail loudly rather than overrun the wire.
fn finish_evidence_ack<A: serde::Serialize>(ack: &A) -> HandlerResult {
    let bytes = batpak::encoding::to_bytes(ack)
        .map_err(|error| HandlerError::failed(format!("encode ack: {error}")))?;
    if bytes.len() > netbat::DEFAULT_MAX_OUTPUT_BYTES {
        return Err(HandlerError::invalid_input(format!(
            "evidence response is {} bytes, over the {}-byte transport limit; \
             lower `limit` or disable proof refs",
            bytes.len(),
            netbat::DEFAULT_MAX_OUTPUT_BYTES
        )));
    }
    Ok(bytes)
}

// ─── evidence.store_resource handler ──────────────────────────────────────────

/// Handler binding for [`crate::evidence::EVIDENCE_STORE_RESOURCE_DESCRIPTOR`].
pub struct StoreResourceEvidenceHandler {
    /// Shared handle to the BatPAK store. Cloning the `Arc` is cheap.
    pub store: Arc<Store>,
}

impl Handler for StoreResourceEvidenceHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut Ctx<'_>) -> HandlerResult {
        handle_evidence_store_resource(&self.store, input)
    }
}

fn handle_evidence_store_resource(store: &Store, input: &[u8]) -> HandlerResult {
    // The request is empty, but decode it so a malformed frame is rejected
    // rather than silently ignored.
    let _request: StoreResourceEvidenceRequest = batpak::encoding::from_bytes(input)
        .map_err(|error| HandlerError::invalid_input(format!("decode request: {error}")))?;

    let report = store
        .store_resource_evidence_report()
        .map_err(|error| HandlerError::failed(format!("store resource evidence: {error}")))?;

    let (report_hex, body_hash_hex) = encode_report_body(&report.body, &report.body_hash)?;
    let ack = StoreResourceEvidenceAck {
        report_hex,
        body_hash_hex,
        truncated: false,
    };
    finish_evidence_ack(&ack)
}

// ─── evidence.read_walk handler ───────────────────────────────────────────────

/// Handler binding for [`crate::evidence::EVIDENCE_READ_WALK_DESCRIPTOR`].
pub struct ReadWalkEvidenceHandler {
    /// Shared handle to the BatPAK store. Cloning the `Arc` is cheap.
    pub store: Arc<Store>,
}

impl Handler for ReadWalkEvidenceHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut Ctx<'_>) -> HandlerResult {
        handle_evidence_read_walk(&self.store, input)
    }
}

fn handle_evidence_read_walk(store: &Store, input: &[u8]) -> HandlerResult {
    let request: ReadWalkEvidenceRequest = batpak::encoding::from_bytes(input)
        .map_err(|error| HandlerError::invalid_input(format!("decode request: {error}")))?;

    let core_request = request
        .to_core()
        .map_err(|error| HandlerError::invalid_input(error.to_string()))?;

    let (_entries, report) = store
        .query_with_read_walk_evidence(&core_request)
        .map_err(|error| HandlerError::failed(format!("read walk evidence: {error}")))?;

    // Truncation means the limit dropped matches — use core's explicit
    // dropped-by-limit count, not `matched > returned`. The latter also fires
    // when a hit cannot be upgraded to a backing entry (MissingBackingEntry),
    // a degraded-but-complete report that must not look pageable.
    let truncated = matches!(
        report.body.dropped_limited_count,
        ReadWalkDroppedCount::Known(dropped) if dropped > 0
    );
    let (report_hex, body_hash_hex) = encode_report_body(&report.body, &report.body_hash)?;
    let ack = ReadWalkEvidenceAck {
        report_hex,
        body_hash_hex,
        truncated,
    };
    finish_evidence_ack(&ack)
}

// ─── evidence.projection_run handler ──────────────────────────────────────────

/// Handler binding for [`crate::evidence::EVIDENCE_PROJECTION_RUN_DESCRIPTOR`].
///
/// Dispatches the request's domain-neutral `projection` id through an
/// embedder-populated [`ProjectionEvidenceRegistry`]. The reference `refbat`
/// binary registers an empty registry, so every projection id resolves to an
/// `unknown projection` error; an embedder that registers its projections makes
/// them reachable without changing the wire contract.
pub struct ProjectionRunEvidenceHandler {
    /// Shared handle to the BatPAK store. Cloning the `Arc` is cheap.
    pub store: Arc<Store>,
    /// Embedder-populated projection dispatch table.
    pub registry: Arc<ProjectionEvidenceRegistry>,
}

impl Handler for ProjectionRunEvidenceHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut Ctx<'_>) -> HandlerResult {
        handle_evidence_projection_run(&self.store, &self.registry, input)
    }
}

fn handle_evidence_projection_run(
    store: &Store,
    registry: &ProjectionEvidenceRegistry,
    input: &[u8],
) -> HandlerResult {
    let request: ProjectionRunEvidenceRequest = batpak::encoding::from_bytes(input)
        .map_err(|error| HandlerError::invalid_input(format!("decode request: {error}")))?;

    // Validate the entity as a substrate coordinate at the boundary, matching
    // bank.commit/event.query, so a malformed entity is a deterministic
    // invalid_input rather than reaching the projection/report path.
    Coordinate::new(&request.entity, "refbat:evidence-projection-run")
        .map_err(|error| HandlerError::invalid_input(format!("entity: {error}")))?;

    let freshness = request.freshness();
    let report = match registry.run(&request.projection, store, &request.entity, &freshness) {
        // A failed projection still yields a deterministic evidence report
        // (findings record the failure); surface it rather than erroring.
        Some(Ok(report)) => report,
        Some(Err(ProjectionRunReportError::ProjectionFailed { report, .. })) => *report,
        Some(Err(other)) => {
            return Err(HandlerError::failed(format!(
                "projection run evidence: {other}"
            )))
        }
        None => {
            return Err(HandlerError::invalid_input(format!(
                "unknown projection: {}",
                request.projection
            )))
        }
    };

    let (report_hex, body_hash_hex) = encode_report_body(&report.body, &report.body_hash)?;
    let ack = ProjectionRunEvidenceAck {
        report_hex,
        body_hash_hex,
        truncated: false,
    };
    finish_evidence_ack(&ack)
}
