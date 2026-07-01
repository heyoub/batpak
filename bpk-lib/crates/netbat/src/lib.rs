#![deny(missing_docs)]
//! Lean, sync-first server/network boundary exposure layer (blocking transport, TLS opt-in).
//!
//! `netbat` is a lean, blocking transport boundary: nb exposes, sb dispatches,
//! bp records. It stays narrow in remit — synchronous and blocking per
//! connection, with a small default dependency graph (TLS is opt-in). This
//! crate can describe server-facing modules, endpoints, and route tables around
//! [`syncbat`] modules or cores. It can also handle bounded sync transport
//! frames, but it does not own dispatch decisions, run handlers directly, or
//! write batpak records.
//!
//! The crate is designed to be imported as:
//!
//! ```rust
//! use netbat as nb;
//! ```
//!
//! # Frame round-trip
//!
//! Encode a CALL request, decode it back, and inspect the parts. The
//! encoder enforces the operation-name grammar via the substrate
//! [`OperationName`] newtype; downstream code never re-parses.
//!
//! ```rust
//! use netbat as nb;
//!
//! let frame = nb::encode_request("system.heartbeat", &[0xde, 0xad]);
//! assert_eq!(frame, b"NETBAT/1 CALL system.heartbeat dead\n");
//!
//! let parsed = nb::decode_line(&frame, &nb::Limits::default()).expect("decode");
//! assert_eq!(parsed.operation(), "system.heartbeat");
//! assert_eq!(parsed.input(), &[0xde, 0xad]);
//! ```
//!
//! # Response framing
//!
//! `encode_response` emits either `OK <hex>\n` or `ERR <code> <hex>\n`.
//! The ERR-frame `code` is a stable token from
//! [`NetbatError::code`](crate::NetbatError::code) — never a runtime
//! string. The message half is hex of UTF-8 text, **not** MessagePack.
//!
//! ```rust
//! use netbat as nb;
//!
//! // Success: OK <hex>\n
//! let ok = nb::encode_response(Ok(b"hi"));
//! assert_eq!(ok, b"OK 6869\n");
//!
//! // Error: ERR <code> <hex>\n
//! let err = nb::NetbatError::MalformedRequest { reason: "bad" };
//! let err_frame = nb::encode_response(Err(&err));
//! assert!(err_frame.starts_with(b"ERR malformed_request "));
//! assert!(err_frame.ends_with(b"\n"));
//! ```
//!
//! # Connection limits and dispatch
//!
//! A blocking listener caps the connections it serves through
//! [`ConnectionLimit`] (the `connection_limit` field on [`TcpServerConfig`] and
//! [`TcpSubscriptionServerConfig`]). The default,
//! [`ConnectionLimit::Concurrent`], is an *in-flight* permit pool sized at
//! [`DEFAULT_MAX_CONNECTIONS`]: at most `n` connections are served at once and a
//! freed slot is immediately reusable. It REPLACES the pre-0.9 `max_connections`
//! *lifetime* accept budget, which is now the explicit opt-in
//! [`ConnectionLimit::Lifetime`]; [`ConnectionLimit::Unlimited`] removes the gate
//! entirely.
//!
//! The subscription listener additionally chooses how each accepted session is
//! served via [`SubscriptionDispatch`]. The default,
//! [`SubscriptionDispatch::Concurrent`], spawns a contained worker per session
//! so subscribers stream concurrently (gated by the same permit pool);
//! [`SubscriptionDispatch::Sequential`] keeps the pre-0.9 inline behavior where
//! one long-lived subscriber blocks the accept loop until its session ends.
//!
//! ```
//! use netbat as nb;
//!
//! // Both listeners default to a concurrent in-flight permit pool, and
//! // subscriptions are served concurrently.
//! assert!(matches!(
//!     nb::ConnectionLimit::default(),
//!     nb::ConnectionLimit::Concurrent(_),
//! ));
//! assert_eq!(nb::SubscriptionDispatch::default(), nb::SubscriptionDispatch::Concurrent);
//!
//! // Opt into the pre-0.9 lifetime accept budget — or remove the gate entirely.
//! let config = nb::TcpServerConfig::default()
//!     .with_connection_limit(nb::ConnectionLimit::Unlimited);
//! assert_eq!(config.connection_limit, nb::ConnectionLimit::Unlimited);
//! ```
//!
//! # Security / transport trust model
//!
//! netbat has **no authentication and no authorization, by design**. Identity
//! and access control are downstream-domain concerns: authenticate and
//! authorize at a fronting proxy or in the application layer that owns the
//! [`syncbat`] runtime, never inside netbat. netbat only frames bytes, maps
//! stable error codes, and moves bounded request/response and subscription
//! frames over a blocking transport.
//!
//! Without the `tls` feature (the default) netbat speaks **plaintext** and so
//! assumes a **trusted transport**: bind it to loopback, a private network
//! segment, or behind a TLS-terminating reverse proxy. There is no in-process
//! confidentiality on the plaintext path.
//!
//! Enabling the opt-in `tls` feature adds **server-only** TLS (rustls): it
//! provides confidentiality and *server* identity only — it does **not**
//! authenticate the client (auth still lives above netbat). Build a
//! [`TlsServerConfig`] from PEM and pass [`TransportSecurity::Tls`] to
//! [`serve_tcp_listener_secured`] (or
//! [`serve_tcp_subscription_listener_secured`]). The rustls handshake runs on
//! the per-connection worker *after* the concurrency permit is acquired, so a
//! slow or hostile handshake occupies at most one worker+permit slot and never
//! blocks the accept loop; a failed handshake (for example, a cleartext peer) is
//! counted in [`TcpServeStats::tls_handshake_failures`] and the connection is
//! dropped — never listener-fatal. See [`TlsServerConfig`] for a PEM example.

mod route;
mod transport;

pub use route::{
    inspect_core_operations, introspect_modules, CoreHealth, Endpoint, Introspection, Route,
    RouteValidationError, Server, ServerModule, LAYER_RULE, MAX_ROUTE_PATH_BYTES,
};
// Re-export the substrate-wide operation-name newtype so callers writing
// `use netbat as nb;` can reach for `nb::OperationName` instead of pulling
// syncbat directly.
pub use syncbat::{OperationName, OperationNameError};
#[cfg(feature = "tls")]
pub use transport::TlsServerConfig;
pub use transport::{
    decode_hex, decode_hex_str, decode_line, decode_stream_line, dispatch_frame, encode_hex,
    encode_hex_into, encode_hex_str, encode_request, encode_response, encode_stream_frame,
    serve_stream, serve_subscription_stream, serve_tcp_listener, serve_tcp_listener_secured,
    serve_tcp_subscription_listener, serve_tcp_subscription_listener_secured, ClientWindow,
    ConnectionLimit, CursorBytes, DeliveryIndex, IoTimeouts, Limits, MaybeCursor, NetbatError,
    PayloadSchemaRef, RequestFrame, ResponseFrame, ShutdownHandle, StreamFrame, StreamReasonCode,
    SubAckFrame, SubCancelFrame, SubEndFrame, SubErrFrame, SubEventFrame, SubWatermarkFrame,
    SubscribeFrame, SubscriptionDispatch, SubscriptionToken, TcpServeStats, TcpServerConfig,
    TcpSubscriptionServeStats, TcpSubscriptionServerConfig, TransportSecurity, CALL_VERB,
    DEFAULT_MAX_CONNECTIONS, DEFAULT_MAX_CURSOR_BYTES, DEFAULT_MAX_INPUT_BYTES,
    DEFAULT_MAX_LINE_BYTES, DEFAULT_MAX_OPERATION_NAME_BYTES, DEFAULT_MAX_OUTPUT_BYTES,
    DEFAULT_MAX_REQUESTS_PER_CONNECTION, DEFAULT_MAX_STREAM_ERROR_MESSAGE_BYTES,
    DEFAULT_MAX_STREAM_PAYLOAD_BYTES, DEFAULT_MAX_SUBSCRIPTION_ID_BYTES, LINE_PROTOCOL_VERSION,
    PROTOCOL_PREFIX, STREAM_PROTOCOL_VERSION, SUBSCRIBE_VERB, SUB_ACK_VERB, SUB_CANCEL_VERB,
    SUB_END_VERB, SUB_ERR_VERB, SUB_EVENT_VERB, SUB_WATERMARK_VERB,
};
