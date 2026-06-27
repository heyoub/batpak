use batpak::event::HashChain;
use batpak::id::EntityIdType;
use batpak::store::IndexEntry;
use serde::{Deserialize, Serialize};

use super::error::SubscriptionRuntimeError;

/// Canonical event-stream payload envelope encoded with `batpak::canonical::to_bytes`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EventStreamEnvelopeV1 {
    /// Envelope schema version.
    pub schema_version: u32,
    /// Globally unique subscription id.
    pub subscription_id: String,
    /// Committed event id.
    pub event_id: u128,
    /// Correlation id from the event header.
    pub correlation_id: u128,
    /// Optional causation id from the event header.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<u128>,
    /// Entity coordinate string.
    pub entity: String,
    /// Scope coordinate string.
    pub scope: String,
    /// Raw event kind u16.
    pub event_kind_raw: u16,
    /// Exported 4-bit event category.
    pub event_category: u8,
    /// Payload schema version stamped on the event header.
    pub payload_version: u16,
    /// Event header timestamp in microseconds.
    pub timestamp_us: i64,
    /// HLC wall milliseconds from the index entry.
    pub hlc_wall_ms: u64,
    /// Commit-order sequence for the event.
    pub global_sequence: u64,
    /// Payload content hash from the event header.
    pub content_hash: [u8; 32],
    /// Previous hash from the entity hash chain, if present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev_hash: Option<[u8; 32]>,
    /// Event hash from the entity hash chain, if present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_hash: Option<[u8; 32]>,
    /// Optional inner payload schema ref declared by the route.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inner_event_payload_schema_ref: Option<String>,
    /// Raw committed payload bytes.
    pub payload: Vec<u8>,
}

impl EventStreamEnvelopeV1 {
    /// Build and canonically encode an envelope for one committed event.
    ///
    /// # Errors
    /// [`SubscriptionRuntimeError::Store`] or [`SubscriptionRuntimeError::EnvelopeEncoding`].
    pub fn encode_for_entry(
        store: &batpak::store::Store<batpak::store::Open>,
        subscription_id: &str,
        entry: &IndexEntry,
        inner_event_payload_schema_ref: Option<&str>,
    ) -> Result<Vec<u8>, SubscriptionRuntimeError> {
        let stored = store.read_raw(entry.event_id())?;
        let envelope = Self::from_stored(
            subscription_id,
            entry,
            &stored,
            inner_event_payload_schema_ref,
        );
        batpak::canonical::to_bytes(&envelope)
            .map_err(|error| SubscriptionRuntimeError::EnvelopeEncoding(error.to_string()))
    }

    fn from_stored(
        subscription_id: &str,
        entry: &IndexEntry,
        stored: &batpak::event::StoredEvent<Vec<u8>>,
        inner_event_payload_schema_ref: Option<&str>,
    ) -> Self {
        let (prev_hash, event_hash) = hash_chain_parts(stored.event.hash_chain.as_ref());
        Self {
            schema_version: 1,
            subscription_id: subscription_id.to_owned(),
            event_id: entry.event_id().as_u128(),
            correlation_id: entry.correlation_id(),
            causation_id: entry.causation_id(),
            entity: stored.coordinate.entity().to_owned(),
            scope: stored.coordinate.scope().to_owned(),
            event_kind_raw: entry.event_kind().as_raw_u16(),
            event_category: entry.event_kind().category(),
            payload_version: stored.event.header.payload_version,
            timestamp_us: stored.event.header.timestamp_us,
            hlc_wall_ms: entry.wall_ms(),
            global_sequence: entry.global_sequence(),
            content_hash: stored.event.header.content_hash,
            prev_hash,
            event_hash,
            inner_event_payload_schema_ref: inner_event_payload_schema_ref.map(str::to_owned),
            payload: stored.event.payload.clone(),
        }
    }
}

fn hash_chain_parts(chain: Option<&HashChain>) -> (Option<[u8; 32]>, Option<[u8; 32]>) {
    chain
        .map(|chain| (Some(chain.prev_hash), Some(chain.event_hash)))
        .unwrap_or((None, None))
}

/// Canonical projection-stream payload envelope encoded with `batpak::canonical::to_bytes`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectionStreamEnvelopeV1 {
    /// Envelope schema version.
    pub schema_version: u32,
    /// Globally unique subscription id.
    pub subscription_id: String,
    /// Route-declared projection id.
    pub projection_id: String,
    /// Entity coordinate watched by the projection.
    pub entity: String,
    /// Entity generation at which the state was materialized.
    pub entity_generation: u64,
    /// Freshness mode used to materialize the projection.
    pub freshness: String,
    /// Optional inner projection schema ref declared by the route.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inner_projection_schema_ref: Option<String>,
    /// Blake3 digest of the canonical projection state bytes.
    pub state_hash: [u8; 32],
    /// Canonical bytes of the typed projection state.
    pub state: Vec<u8>,
}

impl ProjectionStreamEnvelopeV1 {
    /// Build and canonically encode an envelope for one projection update.
    ///
    /// # Errors
    /// [`SubscriptionRuntimeError::EnvelopeEncoding`].
    pub fn encode<T>(
        subscription_id: &str,
        projection_id: &str,
        entity: &str,
        entity_generation: u64,
        freshness: &batpak::store::Freshness,
        inner_projection_schema_ref: Option<&str>,
        state: &T,
    ) -> Result<Vec<u8>, SubscriptionRuntimeError>
    where
        T: serde::Serialize,
    {
        let state_bytes = batpak::canonical::to_bytes(state)
            .map_err(|error| SubscriptionRuntimeError::EnvelopeEncoding(error.to_string()))?;
        let state_hash = blake3_state_hash(&state_bytes);
        let envelope = Self {
            schema_version: 1,
            subscription_id: subscription_id.to_owned(),
            projection_id: projection_id.to_owned(),
            entity: entity.to_owned(),
            entity_generation,
            freshness: freshness_label(freshness)?,
            inner_projection_schema_ref: inner_projection_schema_ref.map(str::to_owned),
            state_hash,
            state: state_bytes,
        };
        batpak::canonical::to_bytes(&envelope)
            .map_err(|error| SubscriptionRuntimeError::EnvelopeEncoding(error.to_string()))
    }
}

fn blake3_state_hash(state_bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(state_bytes).as_bytes()
}

fn freshness_label(
    freshness: &batpak::store::Freshness,
) -> Result<String, SubscriptionRuntimeError> {
    match freshness {
        batpak::store::Freshness::Consistent => Ok("consistent".to_owned()),
        batpak::store::Freshness::MaybeStale { .. } => Ok("maybe_stale".to_owned()),
        _ => Err(SubscriptionRuntimeError::EnvelopeEncoding(
            "unsupported projection freshness mode".to_owned(),
        )),
    }
}
