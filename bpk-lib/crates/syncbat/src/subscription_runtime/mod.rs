//! Store-backed subscription runtime (Packet C).
//!
//! syncbat owns replay, live wake, cursor resume, ACK/backpressure, and
//! delivery envelopes. Wire framing lives in netbat.

mod config;
mod cursor;
mod envelope;
mod error;
mod event_stream;
mod registry;

pub use config::SubscriptionRuntimeConfig;
pub use cursor::{EventStreamCursorV1, PositionKind, CURSOR_V1_LEN, SOURCE_KIND_EVENT_CATEGORY};
pub use envelope::EventStreamEnvelopeV1;
pub use error::SubscriptionRuntimeError;
pub use event_stream::{
    EventStreamSession, EventSubscriptionRuntime, SessionControl, SessionDelivery, SessionEnd,
    SessionError, SessionEventDelivery, SessionPoll, SessionWatermarkDelivery, SubscriptionSession,
    SubscriptionSessionFactory, SubscriptionStore,
};
pub use registry::{SubscriptionId, SubscriptionRegistry, SubscriptionRoute};

#[cfg(test)]
#[path = "event_stream_tests.rs"]
mod event_stream_tests;
