use std::sync::Arc;
use std::time::Duration;

use batpak::store::{Open, Store};
use flume::Receiver;

use super::config::SubscriptionRuntimeConfig;
use super::error::{stream_code, SubscriptionRuntimeError};

/// Opaque runtime cursor bytes passed between syncbat sessions and netbat transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeCursor(Vec<u8>);

impl RuntimeCursor {
    /// Wrap encoded cursor bytes.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Borrow the encoded cursor bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// One server-side delivery frame produced by the runtime engine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionDelivery {
    /// Deliver one committed event or projection update.
    Event(SessionEventDelivery),
    /// Coalesced source-frontier watermark.
    Watermark(SessionWatermarkDelivery),
    /// Terminal stream error.
    Error(SessionError),
    /// Terminal stream end.
    End(SessionEnd),
}

/// One delivered event or projection update with cursor and envelope bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionEventDelivery {
    /// Globally unique subscription id.
    pub subscription_id: String,
    /// Monotonic per-session delivery index.
    pub delivery_index: u64,
    /// Cursor before this delivery.
    pub cursor_before: RuntimeCursor,
    /// Cursor after this delivery.
    pub cursor_after: RuntimeCursor,
    /// Route-declared wire payload schema ref.
    pub wire_payload_schema_ref: String,
    /// Canonical envelope bytes for `payload_hex`.
    pub envelope_bytes: Vec<u8>,
}

/// Coalesced watermark delivery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionWatermarkDelivery {
    /// Globally unique subscription id.
    pub subscription_id: String,
    /// Monotonic per-session delivery index.
    pub delivery_index: u64,
    /// Frontier cursor after the watermark point.
    pub cursor_after: RuntimeCursor,
}

/// Terminal stream error delivery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionError {
    /// Globally unique subscription id when known.
    pub subscription_id: Option<String>,
    /// Stable error code token.
    pub code: &'static str,
    /// Optional last delivered cursor.
    pub last_delivered_cursor: Option<RuntimeCursor>,
    /// Optional last acknowledged cursor.
    pub last_acked_cursor: Option<RuntimeCursor>,
    /// UTF-8 message bytes.
    pub message: Vec<u8>,
}

/// Terminal stream end delivery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionEnd {
    /// Globally unique subscription id.
    pub subscription_id: String,
    /// Stable end reason code.
    pub reason_code: &'static str,
    /// Final cursor after stream end, if any.
    pub cursor_after: Option<RuntimeCursor>,
}

/// Result of one runtime poll step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionPoll {
    /// Produced one delivery frame.
    Delivery(SessionDelivery),
    /// No work available within the timeout.
    Blocked,
    /// Session has ended.
    Ended,
}

/// Runtime session polled by transport adapters.
pub trait SubscriptionSession: Send {
    /// Poll for the next delivery frame.
    ///
    /// # Errors
    /// Runtime failures while producing the next delivery.
    fn poll(&mut self, timeout: Duration) -> Result<SessionPoll, SubscriptionRuntimeError>;
}

/// Factory that opens subscription sessions from wire `SUBSCRIBE` inputs.
pub trait SubscriptionSessionFactory {
    /// Open one session for a validated subscription id.
    ///
    /// # Errors
    /// Unknown subscription, invalid cursor, invalid runtime config, or store failures.
    fn open_session(
        &self,
        subscription_id: &str,
        resume_cursor: Option<&[u8]>,
        client_window: u32,
        control_rx: Receiver<SessionControl>,
    ) -> Result<Box<dyn SubscriptionSession>, SubscriptionRuntimeError>;
}

/// Client control input accepted after subscribe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionControl {
    /// Cumulative delivery acknowledgement.
    Ack {
        /// Highest delivered index acknowledged by the client.
        delivery_index: u64,
        /// Authoritative resume cursor after the acknowledged point.
        cursor: RuntimeCursor,
    },
    /// Client-initiated cancellation.
    Cancel,
    /// Peer disconnected without a semantic cancel frame.
    Disconnected,
    /// Malformed post-subscribe control frame.
    Malformed,
}

/// Cloneable syncbat-owned store handle for subscription runtime sessions.
#[derive(Clone)]
pub struct SubscriptionStore {
    pub(crate) inner: Arc<Store<Open>>,
}

impl SubscriptionStore {
    /// Wrap an open BatPak store for syncbat subscription delivery.
    #[must_use]
    pub fn new(store: Arc<Store<Open>>) -> Self {
        Self { inner: store }
    }
}

/// Unknown subscription terminal error before session open.
#[must_use]
pub fn unknown_subscription_error(subscription_id: &str) -> SessionDelivery {
    SessionDelivery::Error(SessionError {
        subscription_id: Some(subscription_id.to_owned()),
        code: stream_code::UNKNOWN_SUBSCRIPTION,
        last_delivered_cursor: None,
        last_acked_cursor: None,
        message: subscription_id.as_bytes().to_vec(),
    })
}

/// Cursor invalid terminal error before session open.
#[must_use]
pub fn cursor_invalid_error(reason: &'static str) -> SessionDelivery {
    SessionDelivery::Error(SessionError {
        subscription_id: None,
        code: stream_code::CURSOR_INVALID,
        last_delivered_cursor: None,
        last_acked_cursor: None,
        message: reason.as_bytes().to_vec(),
    })
}

/// Cursor mismatch terminal error before session open.
#[must_use]
pub fn cursor_mismatch_error(reason: &'static str) -> SessionDelivery {
    SessionDelivery::Error(SessionError {
        subscription_id: None,
        code: stream_code::CURSOR_MISMATCH,
        last_delivered_cursor: None,
        last_acked_cursor: None,
        message: reason.as_bytes().to_vec(),
    })
}

/// Build a slow-consumer terminal error for an active session.
#[must_use]
pub fn slow_consumer_error(
    subscription_id: &str,
    last_delivered_cursor: Option<RuntimeCursor>,
    last_acked_cursor: Option<RuntimeCursor>,
) -> SessionError {
    SessionError {
        subscription_id: Some(subscription_id.to_owned()),
        code: stream_code::SLOW_CONSUMER,
        last_delivered_cursor,
        last_acked_cursor,
        message: b"delivery window full".to_vec(),
    }
}

/// Build a client-cancel terminal end frame.
#[must_use]
pub fn client_cancel_end(
    subscription_id: &str,
    cursor_after: Option<RuntimeCursor>,
) -> SessionDelivery {
    SessionDelivery::End(SessionEnd {
        subscription_id: subscription_id.to_owned(),
        reason_code: stream_code::CLIENT_CANCELLED,
        cursor_after,
    })
}

/// Build a malformed-control terminal error frame.
#[must_use]
pub fn malformed_control_error(
    subscription_id: &str,
    last_delivered_cursor: Option<RuntimeCursor>,
    last_acked_cursor: Option<RuntimeCursor>,
) -> SessionDelivery {
    SessionDelivery::Error(SessionError {
        subscription_id: Some(subscription_id.to_owned()),
        code: stream_code::MALFORMED_STREAM_FRAME,
        last_delivered_cursor,
        last_acked_cursor,
        message: b"malformed stream control frame".to_vec(),
    })
}

/// Build an invalid-ACK terminal error frame.
#[must_use]
pub fn ack_invalid_error(
    subscription_id: &str,
    reason: &'static str,
    last_delivered_cursor: Option<RuntimeCursor>,
    last_acked_cursor: Option<RuntimeCursor>,
) -> SessionDelivery {
    SessionDelivery::Error(SessionError {
        subscription_id: Some(subscription_id.to_owned()),
        code: match reason {
            "ack cursor does not match sent cursor" | "ack delivery index out of range" => {
                stream_code::MALFORMED_STREAM_FRAME
            }
            _ => stream_code::CURSOR_INVALID,
        },
        last_delivered_cursor,
        last_acked_cursor,
        message: reason.as_bytes().to_vec(),
    })
}

/// Build a cursor-mismatch terminal error frame for an active session.
#[must_use]
pub fn cursor_mismatch_terminal(
    subscription_id: &str,
    reason: &'static str,
    last_delivered_cursor: Option<RuntimeCursor>,
    last_acked_cursor: Option<RuntimeCursor>,
) -> SessionDelivery {
    SessionDelivery::Error(SessionError {
        subscription_id: Some(subscription_id.to_owned()),
        code: stream_code::CURSOR_MISMATCH,
        last_delivered_cursor,
        last_acked_cursor,
        message: reason.as_bytes().to_vec(),
    })
}

/// Compute bounded delivery queue capacity from client and route limits.
#[must_use]
pub fn queue_capacity(
    client_window: u32,
    server_max_window: usize,
    route_cap: Option<usize>,
) -> u64 {
    let client = u64::from(client_window);
    let server = u64::try_from(server_max_window).unwrap_or(u64::MAX);
    let route = route_cap
        .and_then(|cap| u64::try_from(cap).ok())
        .unwrap_or(server);
    client.min(server).min(route)
}

/// Validate runtime configuration before opening a session.
///
/// # Errors
/// [`SubscriptionRuntimeError::InvalidConfig`].
pub fn validate_open_limits(
    config: SubscriptionRuntimeConfig,
    client_window: u32,
    queue_cap: u64,
) -> Result<(), SubscriptionRuntimeError> {
    config.validate()?;
    if client_window == 0 {
        return Err(SubscriptionRuntimeError::InvalidConfig {
            reason: "client window is zero",
        });
    }
    if queue_cap == 0 {
        return Err(SubscriptionRuntimeError::InvalidConfig {
            reason: "delivery queue capacity is zero",
        });
    }
    Ok(())
}
