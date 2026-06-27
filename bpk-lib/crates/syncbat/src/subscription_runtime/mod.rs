//! Store-backed subscription runtime (Packet C).
//!
//! syncbat owns replay, live wake, cursor resume, ACK/backpressure, and
//! delivery envelopes. Wire framing lives in netbat.

mod config;
mod cursor;
mod envelope;
mod error;
mod event_stream;
mod operation_status_stream;
mod projection_stream;
mod projector;
mod receipt_stream;
mod registry;
mod session;

pub use config::SubscriptionRuntimeConfig;
pub use cursor::{
    EventStreamCursorV1, OperationStatusPositionKind, OperationStatusStreamCursorV1, PositionKind,
    ProjectionPositionKind, ProjectionStreamCursorV1, ReceiptStreamCursorV1, CURSOR_V1_LEN,
    OPERATION_STATUS_CURSOR_V1_LEN, PROJECTION_CURSOR_V1_LEN, RECEIPT_STREAM_CURSOR_V1_LEN,
    SOURCE_KIND_EVENT_CATEGORY, SOURCE_KIND_OPERATION_STATUS, SOURCE_KIND_PROJECTION,
    SOURCE_KIND_RECEIPT_STREAM,
};
pub use envelope::{
    EventStreamEnvelopeV1, OperationStatusStreamEnvelopeV1, ProjectionStreamEnvelopeV1,
    ReceiptStreamEnvelopeV1,
};
pub use error::SubscriptionRuntimeError;
pub use event_stream::{EventStreamSession, EventSubscriptionRuntime};
pub use operation_status_stream::OperationStatusStreamSession;
pub use projection_stream::{
    CompositeSubscriptionRuntime, ProjectionStreamSession, TypedProjectionProjector,
};
pub use projector::{ProjectionProjector, ProjectionRouteBinding};
pub use receipt_stream::ReceiptStreamSession;
pub use registry::{
    OperationStatusRouteBinding, ReceiptStreamRouteBinding, SubscriptionId, SubscriptionRegistry,
    SubscriptionRoute,
};
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

#[cfg(test)]
#[path = "operation_status_stream_tests.rs"]
mod operation_status_stream_tests;

#[cfg(test)]
#[path = "receipt_stream_tests.rs"]
mod receipt_stream_tests;
