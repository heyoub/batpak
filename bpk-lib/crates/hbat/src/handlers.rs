//! Runtime handlers for [`crate::bank`] operations.
//!
//! These handlers capture an `Arc<batpak::store::Store>` so they can be
//! registered with [`syncbat::CoreBuilder::register`] from the `hbat`
//! binary. They live in their own module (not in `bank.rs`) because the
//! library half of `hbat` deliberately does NOT depend on a runtime
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
    AppendOptions, AppendReceipt, BatchAppendItem, CausationRef, EncodedBytes, ExtensionKey,
    ReceiptVerification, ReceiptVerificationError, Store,
};
// Hex codec is the canonical netbat implementation; hbat does not
// re-roll its own. See netbat::transport::hex.
use netbat::{decode_hex_str, encode_hex_str};
use syncbat::{Ctx, Handler, HandlerError, HandlerResult};

use crate::bank::{
    BankCommitAck, BankCommitRequest, EventGetAck, EventGetRequest, EventQueryAck,
    EventQueryRequest, EventSummary, EVENT_QUERY_MAX_LIMIT,
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

    let item = BatchAppendItem::from_msgpack_bytes(
        coord,
        kind,
        payload_bytes,
        AppendOptions::new(),
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
        sequence: receipt.sequence,
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
        .find(|entry| entry.event_id() == event_id)
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
    if let Some(entity) = request.entity.as_deref() {
        Coordinate::new(entity, "hbat-query")
            .map_err(|error| HandlerError::invalid_input(format!("entity: {error}")))?;
    }
    if let Some(scope) = request.scope.as_deref() {
        Coordinate::new("hbat:query", scope)
            .map_err(|error| HandlerError::invalid_input(format!("scope: {error}")))?;
    }

    let mut region = request
        .entity
        .as_deref()
        .map_or_else(Region::all, Region::entity);

    if let Some(scope) = request.scope.as_deref() {
        region = region.with_scope(scope);
    }

    match (request.kind_category, request.kind_type_id) {
        (Some(category), Some(type_id)) => {
            let kind = EventKind::try_custom(category, type_id)
                .map_err(|error| HandlerError::invalid_input(format!("event kind: {error:?}")))?;
            Ok(region.with_fact(batpak::coordinate::KindFilter::Exact(kind)))
        }
        (Some(category), None) if category <= 0xF => Ok(region.with_fact_category(category)),
        (Some(category), None) => Err(HandlerError::invalid_input(format!(
            "kind_category must fit in 4 bits, got {category}"
        ))),
        (None, Some(_)) => Err(HandlerError::invalid_input(
            "kind_type_id requires kind_category",
        )),
        (None, None) => Ok(region),
    }
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
        event_id_hex: format!("{:032x}", entry.event_id()),
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
        .find(|entry| entry.event_id() == event_id.as_u128())
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
