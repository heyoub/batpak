use std::error::Error;
use std::fmt;
use std::io;

/// Error returned by netbat transport framing or syncbat dispatch.
///
/// `#[non_exhaustive]` so post-1.0 we can add wire-format variants
/// (or new runtime-error mappings) without breaking downstream
/// exhaustive `match` arms.
#[derive(Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum NetbatError {
    /// Underlying IO failed.
    Io {
        /// Stable IO error kind.
        kind: io::ErrorKind,
    },
    /// End-of-file occurred before any request bytes were read.
    EmptyStream,
    /// Request line exceeded the configured byte limit.
    LineTooLong {
        /// Configured byte limit.
        max: usize,
    },
    /// Request frame was malformed.
    MalformedRequest {
        /// Stable malformed-request reason.
        reason: &'static str,
    },
    /// Request frame declared an unsupported protocol version.
    UnsupportedProtocolVersion {
        /// Unsupported version token from the request line.
        version: String,
    },
    /// Operation name exceeded the configured byte limit.
    OperationNameTooLong {
        /// Configured byte limit.
        max: usize,
    },
    /// Decoded input exceeded the configured byte limit.
    InputTooLarge {
        /// Configured byte limit.
        max: usize,
    },
    /// Runtime produced output too large for the configured response limit.
    OutputTooLarge {
        /// Configured byte limit.
        max: usize,
    },
    /// syncbat rejected the checkout.
    Runtime(syncbat::RuntimeError),
    /// NETBAT/2 stream frame was malformed.
    MalformedStreamFrame {
        /// Stable malformed-stream reason.
        reason: &'static str,
    },
    /// Subscription id exceeded the configured byte limit.
    SubscriptionIdTooLong {
        /// Configured byte limit.
        max: usize,
    },
    /// Decoded cursor bytes exceeded the configured limit.
    CursorTooLarge {
        /// Configured byte limit.
        max: usize,
    },
    /// Decoded stream payload exceeded the configured limit.
    StreamPayloadTooLarge {
        /// Configured byte limit.
        max: usize,
    },
    /// Decoded SUB_ERR message exceeded the configured limit.
    StreamMessageTooLarge {
        /// Configured byte limit.
        max: usize,
    },
}

impl fmt::Display for NetbatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { kind } => write!(f, "io error: {kind:?}"),
            Self::EmptyStream => f.write_str("empty stream"),
            Self::LineTooLong { max } => {
                write!(f, "request line exceeded {max} bytes")
            }
            Self::MalformedRequest { reason } => write!(f, "malformed request: {reason}"),
            Self::UnsupportedProtocolVersion { version } => {
                write!(f, "unsupported protocol version: {version}")
            }
            Self::OperationNameTooLong { max } => {
                write!(f, "operation name exceeded {max} bytes")
            }
            Self::InputTooLarge { max } => write!(f, "input exceeded {max} bytes"),
            Self::OutputTooLarge { max } => write!(f, "output exceeded {max} bytes"),
            Self::Runtime(error) => write!(f, "runtime error: {error}"),
            Self::MalformedStreamFrame { reason } => write!(f, "malformed stream frame: {reason}"),
            Self::SubscriptionIdTooLong { max } => {
                write!(f, "subscription id exceeded {max} bytes")
            }
            Self::CursorTooLarge { max } => write!(f, "cursor exceeded {max} bytes"),
            Self::StreamPayloadTooLarge { max } => {
                write!(f, "stream payload exceeded {max} bytes")
            }
            Self::StreamMessageTooLarge { max } => {
                write!(f, "stream error message exceeded {max} bytes")
            }
        }
    }
}

impl Error for NetbatError {}

impl From<io::Error> for NetbatError {
    fn from(error: io::Error) -> Self {
        Self::Io { kind: error.kind() }
    }
}

impl From<syncbat::RuntimeError> for NetbatError {
    fn from(error: syncbat::RuntimeError) -> Self {
        Self::Runtime(error)
    }
}

impl NetbatError {
    /// Return the stable ASCII token used on the wire for this error.
    ///
    /// The same token is emitted by [`crate::encode_response`] in the `ERR <code> ...`
    /// frame and is therefore already part of the public wire contract; this
    /// accessor exposes the mapping to callers that need to reproduce or
    /// compare against the token without going through a full frame
    /// round-trip (golden-fixture generators, structured logging, etc.).
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } => "io",
            Self::EmptyStream => "empty_stream",
            Self::LineTooLong { .. } => "line_too_long",
            Self::MalformedRequest { .. } => "malformed_request",
            Self::UnsupportedProtocolVersion { .. } => "unsupported_protocol_version",
            Self::OperationNameTooLong { .. } => "operation_name_too_long",
            Self::InputTooLarge { .. } => "input_too_large",
            Self::OutputTooLarge { .. } => "output_too_large",
            Self::MalformedStreamFrame { .. } => "malformed_stream_frame",
            Self::SubscriptionIdTooLong { .. } => "subscription_id_too_long",
            Self::CursorTooLarge { .. } => "cursor_too_large",
            Self::StreamPayloadTooLarge { .. } => "stream_payload_too_large",
            Self::StreamMessageTooLarge { .. } => "stream_message_too_large",
            Self::Runtime(syncbat::RuntimeError::UnknownOperation { .. }) => "unknown_operation",
            Self::Runtime(syncbat::RuntimeError::MissingHandler { .. }) => "missing_handler",
            Self::Runtime(syncbat::RuntimeError::Handler { .. }) => "handler",
            Self::Runtime(syncbat::RuntimeError::ReceiptSink { .. }) => "receipt_sink",
            // `syncbat::RuntimeError` is `#[non_exhaustive]`; any variant
            // added post-0.8.0 surfaces under the generic `runtime` code
            // until netbat learns a more specific token for it.
            Self::Runtime(_) => "runtime",
        }
    }
}
