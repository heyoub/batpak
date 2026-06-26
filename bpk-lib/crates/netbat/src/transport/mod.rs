mod error;
mod frame;
mod hex;
mod limits;
mod stream_frame;
mod tcp;

pub use error::NetbatError;
pub use frame::{
    decode_line, dispatch_frame, encode_request, encode_response, RequestFrame, ResponseFrame,
};
pub use hex::{decode_hex, decode_hex_str, encode_hex, encode_hex_into, encode_hex_str};
pub use limits::{
    IoTimeouts, Limits, CALL_VERB, DEFAULT_MAX_CURSOR_BYTES, DEFAULT_MAX_INPUT_BYTES,
    DEFAULT_MAX_LINE_BYTES, DEFAULT_MAX_OPERATION_NAME_BYTES, DEFAULT_MAX_OUTPUT_BYTES,
    DEFAULT_MAX_STREAM_ERROR_MESSAGE_BYTES, DEFAULT_MAX_STREAM_PAYLOAD_BYTES,
    DEFAULT_MAX_SUBSCRIPTION_ID_BYTES, LINE_PROTOCOL_VERSION, PROTOCOL_PREFIX,
    STREAM_PROTOCOL_VERSION, SUBSCRIBE_VERB, SUB_ACK_VERB, SUB_CANCEL_VERB, SUB_END_VERB,
    SUB_ERR_VERB, SUB_EVENT_VERB, SUB_WATERMARK_VERB,
};
pub use stream_frame::{
    decode_stream_line, encode_stream_frame, ClientWindow, CursorBytes, DeliveryIndex, MaybeCursor,
    PayloadSchemaRef, StreamFrame, StreamReasonCode, SubAckFrame, SubCancelFrame, SubEndFrame,
    SubErrFrame, SubEventFrame, SubWatermarkFrame, SubscribeFrame, SubscriptionToken,
};
pub use tcp::{
    serve_stream, serve_tcp_listener, ShutdownHandle, TcpServeStats, TcpServerConfig,
    DEFAULT_MAX_CONNECTIONS, DEFAULT_MAX_REQUESTS_PER_CONNECTION,
};
