#![deny(missing_docs)]
//! Thin sync-first server/network boundary exposure layer.
//!
//! `netbat` is intentionally thin: nb exposes, sb dispatches, bp records. This
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
pub use transport::{
    decode_hex, decode_hex_str, decode_line, dispatch_frame, encode_hex, encode_hex_into,
    encode_hex_str, encode_request, encode_response, serve_stream, serve_tcp_listener, IoTimeouts,
    Limits, NetbatError, RequestFrame, ResponseFrame, ShutdownHandle, TcpServeStats,
    TcpServerConfig, CALL_VERB, DEFAULT_MAX_CONNECTIONS, DEFAULT_MAX_INPUT_BYTES,
    DEFAULT_MAX_LINE_BYTES, DEFAULT_MAX_OPERATION_NAME_BYTES, DEFAULT_MAX_OUTPUT_BYTES,
    DEFAULT_MAX_REQUESTS_PER_CONNECTION, LINE_PROTOCOL_VERSION, PROTOCOL_PREFIX,
};
