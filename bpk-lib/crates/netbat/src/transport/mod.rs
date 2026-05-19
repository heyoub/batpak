mod error;
mod frame;
mod hex;
mod limits;
mod tcp;

pub use error::NetbatError;
pub use frame::{
    decode_line, dispatch_frame, encode_request, encode_response, RequestFrame, ResponseFrame,
};
pub use hex::{decode_hex, decode_hex_str, encode_hex, encode_hex_into, encode_hex_str};
pub use limits::{
    IoTimeouts, Limits, CALL_VERB, DEFAULT_MAX_INPUT_BYTES, DEFAULT_MAX_LINE_BYTES,
    DEFAULT_MAX_OPERATION_NAME_BYTES, DEFAULT_MAX_OUTPUT_BYTES, LINE_PROTOCOL_VERSION,
    PROTOCOL_PREFIX,
};
pub use tcp::{
    serve_stream, serve_tcp_listener, ShutdownHandle, TcpServeStats, TcpServerConfig,
    DEFAULT_MAX_CONNECTIONS, DEFAULT_MAX_REQUESTS_PER_CONNECTION,
};
