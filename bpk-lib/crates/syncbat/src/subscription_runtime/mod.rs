//! Store-backed subscription runtime (Packet C).
//!
//! syncbat owns replay, live wake, cursor resume, ACK/backpressure, and
//! delivery envelopes. Wire framing lives in netbat.

mod config;
mod cursor;
mod envelope;
mod error;
mod event_stream;
mod projection_stream;
mod projector;
mod registry;
mod session;

pub use config::SubscriptionRuntimeConfig;
pub use cursor::{
    EventStreamCursorV1, PositionKind, ProjectionPositionKind, ProjectionStreamCursorV1,
    CURSOR_V1_LEN, PROJECTION_CURSOR_V1_LEN, SOURCE_KIND_EVENT_CATEGORY, SOURCE_KIND_PROJECTION,
};
pub use envelope::{EventStreamEnvelopeV1, ProjectionStreamEnvelopeV1};
pub use error::SubscriptionRuntimeError;
pub use event_stream::{EventStreamSession, EventSubscriptionRuntime};
pub use projection_stream::{
    CompositeSubscriptionRuntime, ProjectionStreamSession, TypedProjectionProjector,
};
pub use projector::{ProjectionProjector, ProjectionRouteBinding};
pub use registry::{SubscriptionId, SubscriptionRegistry, SubscriptionRoute};
pub use session::{
    cursor_invalid_error, cursor_mismatch_error, unknown_subscription_error, RuntimeCursor,
    SessionControl, SessionDelivery, SessionEnd, SessionError, SessionEventDelivery, SessionPoll,
    SessionWatermarkDelivery, SubscriptionSession, SubscriptionSessionFactory, SubscriptionStore,
};

#[cfg(test)]
#[path = "event_stream_tests.rs"]
mod event_stream_tests;

#[cfg(test)]
#[path = "projection_stream_tests.rs"]
mod projection_stream_tests;
