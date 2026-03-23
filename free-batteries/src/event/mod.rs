pub mod hash;
pub mod header;
pub mod kind;
pub mod sourcing;

pub use hash::HashChain;
pub use header::EventHeader;
pub use kind::EventKind;
pub use sourcing::{EventSourced, Reactive};

use crate::coordinate::Coordinate;
use serde::{Deserialize, Serialize};

/// `Event<P>`: header + payload + optional hash chain.
/// [SPEC:src/event/mod.rs]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event<P> {
    pub header: EventHeader,
    pub payload: P,
    pub hash_chain: Option<HashChain>,
}

/// `StoredEvent<P>`: what store.get() returns. Coordinate + Event.
/// store.get() returns StoredEvent<serde_json::Value> because segments are
/// schema-free MessagePack. [SPEC:src/event/mod.rs]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredEvent<P> {
    pub coordinate: Coordinate,
    pub event: Event<P>,
}

impl<P> Event<P> {
    pub fn new(header: EventHeader, payload: P) -> Self {
        Self {
            header,
            payload,
            hash_chain: None,
        }
    }

    pub fn with_hash_chain(mut self, chain: HashChain) -> Self {
        self.hash_chain = Some(chain);
        self
    }

    pub fn event_id(&self) -> u128 {
        self.header.event_id
    }

    pub fn event_kind(&self) -> EventKind {
        self.header.event_kind
    }

    pub fn position(&self) -> &crate::coordinate::DagPosition {
        &self.header.position
    }

    pub fn is_genesis(&self) -> bool {
        self.hash_chain
            .as_ref()
            .map(|c| c.prev_hash == [0u8; 32])
            .unwrap_or(true)
    }

    pub fn map_payload<U, F: FnOnce(P) -> U>(self, f: F) -> Event<U> {
        Event {
            header: self.header,
            payload: f(self.payload),
            hash_chain: self.hash_chain,
        }
    }
}
