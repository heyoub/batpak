//! Beginner-oriented imports for the canonical BatPAK store path.
//!
//! This prelude is intentionally small: open a store, append typed events,
//! page the commit spine, point-read payloads, verify append receipts, walk
//! bounded ancestry, and project derived state. Advanced batteries such as
//! pipelines, reactors, delivery cursors, cache backends, schema snapshots,
//! and evidence reports remain public under their owning modules.

pub use crate::coordinate::{
    ClockRange, Coordinate, CoordinateError, EventCategory, KindFilter, Region, RegionFilterError,
};
pub use crate::event::{
    revalidate_event_payload_registry, validate_event_payload_registry, verify_registry,
    DecodeTyped, Event, EventHeader, EventKind, EventKindError, EventPayload,
    EventPayloadKindCollision, EventPayloadRegistryError, EventPayloadValidation, EventSourced,
    HashChain, JsonValueInput, ProjectionEvent, ProjectionInput, ProjectionPayload,
    ProjectionStateContract, RawMsgpackInput, ReplayLane, StateExtent, StateExtentCost,
    StoredEvent, TypedDecodeError,
};
pub use crate::id::EventId;
pub use crate::store::{
    AppendOptions, AppendReceipt, Closed, Freshness, Open, ReadOnly, ReceiptVerification,
    ReceiptVerificationError, Store, StoreConfig, StoreError, SyncMode,
};
pub use batpak_macros::{EventPayload, EventSourced};
