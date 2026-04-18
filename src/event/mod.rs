/// Typed decode/route seam — shared dispatch primitive for both replay lanes.
pub mod decode;
/// Blake3 hash chain for per-entity integrity verification.
pub mod hash;
/// Metadata header attached to every event.
pub mod header;
/// Discriminant enum identifying the category of an event.
pub mod kind;
/// Binding between a Rust payload type and its wire EventKind.
pub mod payload;
/// Traits for event-sourced and reactive state reconstruction.
pub mod sourcing;

pub use decode::{DecodeSource, DecodeTyped, TypedDecodeError};
pub use hash::HashChain;
pub use header::EventHeader;
pub use kind::EventKind;
pub use payload::EventPayload;
pub use sourcing::{
    EventSourced, JsonValueInput, MultiDispatchError, MultiReactive, ProjectionEvent,
    ProjectionInput, ProjectionPayload, RawMsgpackInput, Reactive, ReplayLane, TypedReactive,
};

use crate::coordinate::Coordinate;
use serde::{Deserialize, Serialize};

/// `Event<P>`: header + payload + optional hash chain.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event<P> {
    /// Metadata describing the event's identity, timing, and position.
    pub header: EventHeader,
    /// Domain-specific data carried by this event.
    pub payload: P,
    /// Optional hash chain linking this event to its predecessor.
    pub hash_chain: Option<HashChain>,
}

/// `StoredEvent<P>`: what store.get() returns. Coordinate + Event.
/// store.get() returns StoredEvent<serde_json::Value> because segments are
/// schema-free MessagePack.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredEvent<P> {
    /// Stream coordinate (entity + segment) where this event is stored.
    pub coordinate: Coordinate,
    /// The full event, including header, payload, and optional hash chain.
    pub event: Event<P>,
}

impl<P> Event<P> {
    /// Creates a new event from a header and payload, with no hash chain.
    pub fn new(header: EventHeader, payload: P) -> Self {
        Self {
            header,
            payload,
            hash_chain: None,
        }
    }

    /// Attaches a hash chain to this event, enabling integrity verification.
    pub fn with_hash_chain(mut self, chain: HashChain) -> Self {
        self.hash_chain = Some(chain);
        self
    }

    /// Returns the unique event identifier.
    pub fn event_id(&self) -> u128 {
        self.header.event_id
    }

    /// Returns the kind of this event.
    pub fn event_kind(&self) -> EventKind {
        self.header.event_kind
    }

    /// Returns the DAG position of this event within its stream.
    pub fn position(&self) -> &crate::coordinate::DagPosition {
        &self.header.position
    }

    /// Returns whether this is the first event in its hash chain.
    ///
    /// Events without hash-chain metadata are treated as genesis events for
    /// callers that only care about the common "is this a root?" question.
    pub fn is_genesis(&self) -> bool {
        self.hash_chain
            .as_ref()
            .is_none_or(|c| c.prev_hash == [0u8; 32])
    }

    /// Transforms the payload with `f`, preserving the header and hash chain.
    pub fn map_payload<U, F: FnOnce(P) -> U>(self, f: F) -> Event<U> {
        Event {
            header: self.header,
            payload: f(self.payload),
            hash_chain: self.hash_chain,
        }
    }
}
