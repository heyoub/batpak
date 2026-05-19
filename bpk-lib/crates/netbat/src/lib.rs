#![warn(missing_docs)]
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

mod route;
mod transport;

pub use route::{
    inspect_core_operations, introspect_modules, CoreHealth, Endpoint, Introspection, Route,
    RouteValidationError, Server, ServerModule, LAYER_RULE, MAX_ROUTE_PATH_BYTES,
};
pub use transport::{
    decode_hex, decode_line, dispatch_frame, encode_hex, encode_hex_into, encode_request,
    encode_response, serve_stream, serve_tcp_listener, IoTimeouts, Limits, NetbatError,
    RequestFrame, ResponseFrame, ShutdownHandle, TcpServeStats, TcpServerConfig, CALL_VERB,
    DEFAULT_MAX_CONNECTIONS, DEFAULT_MAX_INPUT_BYTES, DEFAULT_MAX_LINE_BYTES,
    DEFAULT_MAX_OPERATION_NAME_BYTES, DEFAULT_MAX_OUTPUT_BYTES,
    DEFAULT_MAX_REQUESTS_PER_CONNECTION, LINE_PROTOCOL_VERSION, PROTOCOL_PREFIX,
};
