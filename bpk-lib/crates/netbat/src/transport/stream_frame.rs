//! NETBAT/2 subscription streaming line frames.
//!
//! One newline-terminated ASCII line per frame. Cursor bytes are opaque on the
//! wire: runtime producers map existing core ordering surfaces
//! (`global_sequence`, HLC/frontier, `ProjectionFrontier`, source-specific
//! resume tokens) into bytes without netbat interpreting or minting cursors.
//!
//! Packet B is encode/decode only — no TCP streaming service, no async API, and
//! no subscription serving claim.

use super::error::NetbatError;
use super::hex::{decode_hex, encode_hex_into};
use super::limits::{
    Limits, STREAM_PROTOCOL_VERSION, SUBSCRIBE_VERB, SUB_ACK_VERB, SUB_CANCEL_VERB, SUB_END_VERB,
    SUB_ERR_VERB, SUB_EVENT_VERB, SUB_WATERMARK_VERB,
};

/// Maximum bytes accepted for a [`SubscriptionToken`] (matches hostbat grammar).
const MAX_SUBSCRIPTION_ID_BYTES: usize = 128;
/// Maximum bytes accepted for a payload schema ref token.
const MAX_PAYLOAD_SCHEMA_REF_BYTES: usize = 256;
/// Maximum bytes accepted for a stream reason/code token.
const MAX_STREAM_REASON_CODE_BYTES: usize = 128;

/// Globally unique subscription id (`orders.open.v1` grammar).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubscriptionToken(String);

impl SubscriptionToken {
    /// Construct a subscription token from wire text.
    ///
    /// # Errors
    /// [`NetbatError::SubscriptionIdTooLong`] or [`NetbatError::MalformedStreamFrame`].
    pub fn new(id: impl Into<String>, limits: &Limits) -> Result<Self, NetbatError> {
        let id = id.into();
        if id.len() > limits.max_subscription_id_bytes {
            return Err(NetbatError::SubscriptionIdTooLong {
                max: limits.max_subscription_id_bytes,
            });
        }
        validate_subscription_id(&id)
            .map_err(|reason| NetbatError::MalformedStreamFrame { reason })?;
        Ok(Self(id))
    }

    /// Token as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Opaque cursor bytes carried on the wire as lowercase hex.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CursorBytes(Vec<u8>);

impl CursorBytes {
    /// Wrap already-bounded opaque cursor bytes.
    #[must_use]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// Borrow the opaque bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Consume into owned bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

/// Optional cursor field encoded as `-` when absent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MaybeCursor {
    /// No cursor value on this field.
    Absent,
    /// Opaque cursor bytes.
    Present(CursorBytes),
}

impl MaybeCursor {
    fn encode_token(&self, out: &mut Vec<u8>) {
        match self {
            Self::Absent => out.push(b'-'),
            Self::Present(cursor) => encode_hex_into(cursor.as_bytes(), out),
        }
    }
}

/// Monotonic per-subscription delivery index (nonzero).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct DeliveryIndex(u64);

impl DeliveryIndex {
    /// Construct a nonzero delivery index.
    ///
    /// # Errors
    /// [`NetbatError::MalformedStreamFrame`] when `value` is zero.
    pub fn new(value: u64) -> Result<Self, NetbatError> {
        if value == 0 {
            return Err(NetbatError::MalformedStreamFrame {
                reason: "delivery index must be nonzero",
            });
        }
        Ok(Self(value))
    }

    /// Raw delivery index value.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Client-side receive window (nonzero).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ClientWindow(u32);

impl ClientWindow {
    /// Construct a nonzero client window.
    ///
    /// # Errors
    /// [`NetbatError::MalformedStreamFrame`] when `value` is zero.
    pub fn new(value: u32) -> Result<Self, NetbatError> {
        if value == 0 {
            return Err(NetbatError::MalformedStreamFrame {
                reason: "client window must be nonzero",
            });
        }
        Ok(Self(value))
    }

    /// Raw client window value.
    #[must_use]
    pub fn get(self) -> u32 {
        self.0
    }
}

/// Stable lowercase stream reason/code token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamReasonCode(String);

impl StreamReasonCode {
    /// Construct a reason/code token.
    ///
    /// # Errors
    /// [`NetbatError::MalformedStreamFrame`] when grammar checks fail.
    pub fn new(code: impl Into<String>) -> Result<Self, NetbatError> {
        let code = code.into();
        validate_reason_code(&code)
            .map_err(|reason| NetbatError::MalformedStreamFrame { reason })?;
        Ok(Self(code))
    }

    /// Token as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Opaque payload schema ref token (interpreted above netbat).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PayloadSchemaRef(String);

impl PayloadSchemaRef {
    /// Construct a payload schema ref token.
    ///
    /// # Errors
    /// [`NetbatError::MalformedStreamFrame`] when grammar checks fail.
    pub fn new(reference: impl Into<String>) -> Result<Self, NetbatError> {
        let reference = reference.into();
        validate_payload_schema_ref(&reference)
            .map_err(|reason| NetbatError::MalformedStreamFrame { reason })?;
        Ok(Self(reference))
    }

    /// Reference as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// `NETBAT/2 SUBSCRIBE` frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubscribeFrame {
    /// Globally unique subscription id.
    pub subscription_id: SubscriptionToken,
    /// Resume cursor, if any.
    pub resume_cursor: MaybeCursor,
    /// Client-side receive window.
    pub client_window: ClientWindow,
}

/// `NETBAT/2 SUB_EVENT` frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubEventFrame {
    /// Globally unique subscription id.
    pub subscription_id: SubscriptionToken,
    /// Monotonic delivery index for this event.
    pub delivery_index: DeliveryIndex,
    /// Cursor before this delivery (opaque bytes).
    pub cursor_before: MaybeCursor,
    /// Cursor after this delivery (opaque bytes).
    pub cursor_after: MaybeCursor,
    /// Payload schema ref token.
    pub payload_schema_ref: PayloadSchemaRef,
    /// Canonical payload bytes.
    pub payload: Vec<u8>,
}

/// `NETBAT/2 SUB_WATERMARK` frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubWatermarkFrame {
    /// Globally unique subscription id.
    pub subscription_id: SubscriptionToken,
    /// Monotonic delivery index watermark.
    pub delivery_index: DeliveryIndex,
    /// Cursor after watermark (opaque bytes).
    pub cursor_after: CursorBytes,
}

/// `NETBAT/2 SUB_ACK` frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubAckFrame {
    /// Globally unique subscription id.
    pub subscription_id: SubscriptionToken,
    /// Cumulative delivery index acknowledged.
    pub delivery_index: DeliveryIndex,
    /// Cumulative cursor after acknowledged deliveries.
    pub cursor_after: CursorBytes,
}

/// `NETBAT/2 SUB_CANCEL` frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubCancelFrame {
    /// Globally unique subscription id.
    pub subscription_id: SubscriptionToken,
    /// Stable cancel reason code.
    pub reason_code: StreamReasonCode,
}

/// `NETBAT/2 SUB_ERR` frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubErrFrame {
    /// Globally unique subscription id.
    pub subscription_id: SubscriptionToken,
    /// Stable error code token.
    pub code: StreamReasonCode,
    /// Last delivered cursor, if any.
    pub last_delivered_cursor: MaybeCursor,
    /// Last acknowledged cursor, if any.
    pub last_acked_cursor: MaybeCursor,
    /// UTF-8 error message bytes (hex on wire).
    pub message: Vec<u8>,
}

/// `NETBAT/2 SUB_END` frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubEndFrame {
    /// Globally unique subscription id.
    pub subscription_id: SubscriptionToken,
    /// Final cursor after stream end, if any.
    pub cursor_after: MaybeCursor,
    /// Stable end reason code.
    pub reason_code: StreamReasonCode,
}

/// Decoded NETBAT/2 streaming frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamFrame {
    /// Open or resume a subscription stream.
    Subscribe(SubscribeFrame),
    /// Deliver one subscription event/payload.
    SubEvent(SubEventFrame),
    /// Publish a cumulative delivery watermark.
    SubWatermark(SubWatermarkFrame),
    /// Acknowledge cumulative delivery progress.
    SubAck(SubAckFrame),
    /// Cancel a subscription stream.
    SubCancel(SubCancelFrame),
    /// Report a subscription stream error.
    SubErr(SubErrFrame),
    /// End a subscription stream.
    SubEnd(SubEndFrame),
}

/// Decode one NETBAT/2 streaming line frame.
///
/// # Errors
/// Returns [`NetbatError`] when the frame is malformed or exceeds limits.
pub fn decode_stream_line(line: &[u8], limits: &Limits) -> Result<StreamFrame, NetbatError> {
    if line.len() > limits.max_line_bytes {
        return Err(NetbatError::LineTooLong {
            max: limits.max_line_bytes,
        });
    }

    let line = strip_line_ending(line);
    if line.is_empty() {
        return Err(NetbatError::MalformedStreamFrame {
            reason: "empty line",
        });
    }

    let mut parts = line.split(|byte| *byte == b' ');
    let version = parts.next().ok_or(NetbatError::MalformedStreamFrame {
        reason: "missing protocol version",
    })?;
    if version != STREAM_PROTOCOL_VERSION.as_bytes() {
        return Err(NetbatError::UnsupportedProtocolVersion {
            version: std::str::from_utf8(version)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|_| format!("0x{}", encode_hex_into_lossy(version))),
        });
    }

    let verb = parts.next().ok_or(NetbatError::MalformedStreamFrame {
        reason: "missing stream verb",
    })?;
    let frame = decode_stream_frame_body(verb, &mut parts, limits)?;

    if parts.next().is_some() {
        return Err(NetbatError::MalformedStreamFrame {
            reason: "too many fields",
        });
    }
    Ok(frame)
}

fn decode_stream_frame_body<'a>(
    verb: &[u8],
    parts: &mut impl Iterator<Item = &'a [u8]>,
    limits: &Limits,
) -> Result<StreamFrame, NetbatError> {
    match verb {
        b if b == SUBSCRIBE_VERB.as_bytes() => {
            Ok(StreamFrame::Subscribe(decode_subscribe(parts, limits)?))
        }
        b if b == SUB_EVENT_VERB.as_bytes() => {
            Ok(StreamFrame::SubEvent(decode_sub_event(parts, limits)?))
        }
        b if b == SUB_WATERMARK_VERB.as_bytes() => Ok(StreamFrame::SubWatermark(
            decode_sub_watermark(parts, limits)?,
        )),
        b if b == SUB_ACK_VERB.as_bytes() => {
            Ok(StreamFrame::SubAck(decode_sub_ack(parts, limits)?))
        }
        b if b == SUB_CANCEL_VERB.as_bytes() => {
            Ok(StreamFrame::SubCancel(decode_sub_cancel(parts, limits)?))
        }
        b if b == SUB_ERR_VERB.as_bytes() => {
            Ok(StreamFrame::SubErr(decode_sub_err(parts, limits)?))
        }
        b if b == SUB_END_VERB.as_bytes() => {
            Ok(StreamFrame::SubEnd(decode_sub_end(parts, limits)?))
        }
        _ => Err(NetbatError::MalformedStreamFrame {
            reason: "unknown stream verb",
        }),
    }
}

/// Encode one NETBAT/2 streaming frame as a newline-terminated line.
#[must_use]
pub fn encode_stream_frame(frame: &StreamFrame) -> Vec<u8> {
    let mut line = Vec::new();
    line.extend_from_slice(STREAM_PROTOCOL_VERSION.as_bytes());
    line.push(b' ');
    match frame {
        StreamFrame::Subscribe(frame) => {
            line.extend_from_slice(SUBSCRIBE_VERB.as_bytes());
            line.push(b' ');
            line.extend_from_slice(frame.subscription_id.as_str().as_bytes());
            line.push(b' ');
            frame.resume_cursor.encode_token(&mut line);
            line.push(b' ');
            encode_decimal_u32(frame.client_window.get(), &mut line);
        }
        StreamFrame::SubEvent(frame) => {
            line.extend_from_slice(SUB_EVENT_VERB.as_bytes());
            line.push(b' ');
            line.extend_from_slice(frame.subscription_id.as_str().as_bytes());
            line.push(b' ');
            encode_decimal_u64(frame.delivery_index.get(), &mut line);
            line.push(b' ');
            frame.cursor_before.encode_token(&mut line);
            line.push(b' ');
            frame.cursor_after.encode_token(&mut line);
            line.push(b' ');
            line.extend_from_slice(frame.payload_schema_ref.as_str().as_bytes());
            line.push(b' ');
            encode_hex_into(&frame.payload, &mut line);
        }
        StreamFrame::SubWatermark(frame) => {
            line.extend_from_slice(SUB_WATERMARK_VERB.as_bytes());
            line.push(b' ');
            line.extend_from_slice(frame.subscription_id.as_str().as_bytes());
            line.push(b' ');
            encode_decimal_u64(frame.delivery_index.get(), &mut line);
            line.push(b' ');
            encode_hex_into(frame.cursor_after.as_bytes(), &mut line);
        }
        StreamFrame::SubAck(frame) => {
            line.extend_from_slice(SUB_ACK_VERB.as_bytes());
            line.push(b' ');
            line.extend_from_slice(frame.subscription_id.as_str().as_bytes());
            line.push(b' ');
            encode_decimal_u64(frame.delivery_index.get(), &mut line);
            line.push(b' ');
            encode_hex_into(frame.cursor_after.as_bytes(), &mut line);
        }
        StreamFrame::SubCancel(frame) => {
            line.extend_from_slice(SUB_CANCEL_VERB.as_bytes());
            line.push(b' ');
            line.extend_from_slice(frame.subscription_id.as_str().as_bytes());
            line.push(b' ');
            line.extend_from_slice(frame.reason_code.as_str().as_bytes());
        }
        StreamFrame::SubErr(frame) => {
            line.extend_from_slice(SUB_ERR_VERB.as_bytes());
            line.push(b' ');
            line.extend_from_slice(frame.subscription_id.as_str().as_bytes());
            line.push(b' ');
            line.extend_from_slice(frame.code.as_str().as_bytes());
            line.push(b' ');
            frame.last_delivered_cursor.encode_token(&mut line);
            line.push(b' ');
            frame.last_acked_cursor.encode_token(&mut line);
            line.push(b' ');
            encode_hex_into(&frame.message, &mut line);
        }
        StreamFrame::SubEnd(frame) => {
            line.extend_from_slice(SUB_END_VERB.as_bytes());
            line.push(b' ');
            line.extend_from_slice(frame.subscription_id.as_str().as_bytes());
            line.push(b' ');
            frame.cursor_after.encode_token(&mut line);
            line.push(b' ');
            line.extend_from_slice(frame.reason_code.as_str().as_bytes());
        }
    }
    line.push(b'\n');
    line
}

fn decode_subscribe<'a>(
    parts: &mut impl Iterator<Item = &'a [u8]>,
    limits: &Limits,
) -> Result<SubscribeFrame, NetbatError> {
    let subscription_id = next_token(parts, "missing subscription id")?;
    let resume = next_maybe_cursor(parts, limits, "missing resume cursor")?;
    let window = next_token(parts, "missing client window")?;
    Ok(SubscribeFrame {
        subscription_id: SubscriptionToken::new(parse_utf8(subscription_id)?, limits)?,
        resume_cursor: resume,
        client_window: ClientWindow::new(parse_u32(window)?)?,
    })
}

fn decode_sub_event<'a>(
    parts: &mut impl Iterator<Item = &'a [u8]>,
    limits: &Limits,
) -> Result<SubEventFrame, NetbatError> {
    let subscription_id = next_token(parts, "missing subscription id")?;
    let delivery_index = next_token(parts, "missing delivery index")?;
    let cursor_before = next_maybe_cursor(parts, limits, "missing cursor_before")?;
    let cursor_after = next_maybe_cursor(parts, limits, "missing cursor_after")?;
    let payload_schema_ref = next_token(parts, "missing payload schema ref")?;
    let payload_hex = next_token(parts, "missing payload hex")?;
    Ok(SubEventFrame {
        subscription_id: SubscriptionToken::new(parse_utf8(subscription_id)?, limits)?,
        delivery_index: DeliveryIndex::new(parse_u64(delivery_index)?)?,
        cursor_before,
        cursor_after,
        payload_schema_ref: PayloadSchemaRef::new(parse_utf8(payload_schema_ref)?)?,
        payload: decode_stream_hex(payload_hex, limits.max_stream_payload_bytes, |max| {
            NetbatError::StreamPayloadTooLarge { max }
        })?,
    })
}

fn decode_sub_watermark<'a>(
    parts: &mut impl Iterator<Item = &'a [u8]>,
    limits: &Limits,
) -> Result<SubWatermarkFrame, NetbatError> {
    let subscription_id = next_token(parts, "missing subscription id")?;
    let delivery_index = next_token(parts, "missing delivery index")?;
    let cursor_after = next_token(parts, "missing cursor_after")?;
    Ok(SubWatermarkFrame {
        subscription_id: SubscriptionToken::new(parse_utf8(subscription_id)?, limits)?,
        delivery_index: DeliveryIndex::new(parse_u64(delivery_index)?)?,
        cursor_after: decode_required_cursor(cursor_after, limits)?,
    })
}

fn decode_sub_ack<'a>(
    parts: &mut impl Iterator<Item = &'a [u8]>,
    limits: &Limits,
) -> Result<SubAckFrame, NetbatError> {
    let subscription_id = next_token(parts, "missing subscription id")?;
    let delivery_index = next_token(parts, "missing delivery index")?;
    let cursor_after = next_token(parts, "missing cursor_after")?;
    Ok(SubAckFrame {
        subscription_id: SubscriptionToken::new(parse_utf8(subscription_id)?, limits)?,
        delivery_index: DeliveryIndex::new(parse_u64(delivery_index)?)?,
        cursor_after: decode_required_cursor(cursor_after, limits)?,
    })
}

fn decode_sub_cancel<'a>(
    parts: &mut impl Iterator<Item = &'a [u8]>,
    limits: &Limits,
) -> Result<SubCancelFrame, NetbatError> {
    let subscription_id = next_token(parts, "missing subscription id")?;
    let reason_code = next_token(parts, "missing reason code")?;
    Ok(SubCancelFrame {
        subscription_id: SubscriptionToken::new(parse_utf8(subscription_id)?, limits)?,
        reason_code: StreamReasonCode::new(parse_utf8(reason_code)?)?,
    })
}

fn decode_sub_err<'a>(
    parts: &mut impl Iterator<Item = &'a [u8]>,
    limits: &Limits,
) -> Result<SubErrFrame, NetbatError> {
    let subscription_id = next_token(parts, "missing subscription id")?;
    let code = next_token(parts, "missing error code")?;
    let last_delivered = next_maybe_cursor(parts, limits, "missing last delivered cursor")?;
    let last_acked = next_maybe_cursor(parts, limits, "missing last acked cursor")?;
    let message_hex = next_token(parts, "missing message hex")?;
    Ok(SubErrFrame {
        subscription_id: SubscriptionToken::new(parse_utf8(subscription_id)?, limits)?,
        code: StreamReasonCode::new(parse_utf8(code)?)?,
        last_delivered_cursor: last_delivered,
        last_acked_cursor: last_acked,
        message: decode_stream_hex(message_hex, limits.max_stream_error_message_bytes, |max| {
            NetbatError::StreamMessageTooLarge { max }
        })?,
    })
}

fn decode_sub_end<'a>(
    parts: &mut impl Iterator<Item = &'a [u8]>,
    limits: &Limits,
) -> Result<SubEndFrame, NetbatError> {
    let subscription_id = next_token(parts, "missing subscription id")?;
    let cursor_after = next_maybe_cursor(parts, limits, "missing cursor_after")?;
    let reason_code = next_token(parts, "missing reason code")?;
    Ok(SubEndFrame {
        subscription_id: SubscriptionToken::new(parse_utf8(subscription_id)?, limits)?,
        cursor_after,
        reason_code: StreamReasonCode::new(parse_utf8(reason_code)?)?,
    })
}

fn next_token<'a>(
    parts: &mut impl Iterator<Item = &'a [u8]>,
    missing: &'static str,
) -> Result<&'a [u8], NetbatError> {
    parts
        .next()
        .ok_or(NetbatError::MalformedStreamFrame { reason: missing })
}

fn next_maybe_cursor<'a>(
    parts: &mut impl Iterator<Item = &'a [u8]>,
    limits: &Limits,
    missing: &'static str,
) -> Result<MaybeCursor, NetbatError> {
    let token = next_token(parts, missing)?;
    decode_maybe_cursor(token, limits)
}

fn decode_maybe_cursor(token: &[u8], limits: &Limits) -> Result<MaybeCursor, NetbatError> {
    if token == b"-" {
        return Ok(MaybeCursor::Absent);
    }
    Ok(MaybeCursor::Present(decode_required_cursor(token, limits)?))
}

fn decode_required_cursor(token: &[u8], limits: &Limits) -> Result<CursorBytes, NetbatError> {
    Ok(CursorBytes::new(decode_stream_hex(
        token,
        limits.max_cursor_bytes,
        |max| NetbatError::CursorTooLarge { max },
    )?))
}

fn decode_stream_hex(
    token: &[u8],
    max_bytes: usize,
    too_large: impl FnOnce(usize) -> NetbatError,
) -> Result<Vec<u8>, NetbatError> {
    decode_hex(token, max_bytes).map_err(|error| match error {
        NetbatError::InputTooLarge { max } => too_large(max),
        NetbatError::MalformedRequest { reason } => NetbatError::MalformedStreamFrame { reason },
        NetbatError::Io { .. }
        | NetbatError::EmptyStream
        | NetbatError::LineTooLong { .. }
        | NetbatError::UnsupportedProtocolVersion { .. }
        | NetbatError::OperationNameTooLong { .. }
        | NetbatError::OutputTooLarge { .. }
        | NetbatError::Runtime(_)
        | NetbatError::MalformedStreamFrame { .. }
        | NetbatError::SubscriptionIdTooLong { .. }
        | NetbatError::CursorTooLarge { .. }
        | NetbatError::StreamPayloadTooLarge { .. }
        | NetbatError::StreamMessageTooLarge { .. } => error,
    })
}

fn parse_utf8(token: &[u8]) -> Result<String, NetbatError> {
    std::str::from_utf8(token)
        .map(ToOwned::to_owned)
        .map_err(|_| NetbatError::MalformedStreamFrame {
            reason: "token is not utf-8",
        })
}

fn parse_u64(token: &[u8]) -> Result<u64, NetbatError> {
    let text = parse_utf8(token)?;
    text.parse::<u64>()
        .map_err(|_| NetbatError::MalformedStreamFrame {
            reason: "integer field is not decimal",
        })
}

fn parse_u32(token: &[u8]) -> Result<u32, NetbatError> {
    let text = parse_utf8(token)?;
    text.parse::<u32>()
        .map_err(|_| NetbatError::MalformedStreamFrame {
            reason: "integer field is not decimal",
        })
}

fn encode_decimal_u64(value: u64, out: &mut Vec<u8>) {
    out.extend_from_slice(value.to_string().as_bytes());
}

fn encode_decimal_u32(value: u32, out: &mut Vec<u8>) {
    out.extend_from_slice(value.to_string().as_bytes());
}

fn strip_line_ending(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n")
        .unwrap_or(line)
        .strip_suffix(b"\r")
        .unwrap_or_else(|| line.strip_suffix(b"\n").unwrap_or(line))
}

fn encode_hex_into_lossy(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Validate subscription id grammar:
/// `^[a-z0-9][a-z0-9._-]*\.v[1-9][0-9]*$` with length and dot rules.
fn validate_subscription_id(id: &str) -> Result<(), &'static str> {
    if id.is_empty() {
        return Err("empty subscription id");
    }
    if id.len() > MAX_SUBSCRIPTION_ID_BYTES {
        return Err("subscription id longer than 128 bytes");
    }
    if id.starts_with('.') || id.ends_with('.') {
        return Err("subscription id has a leading or trailing '.'");
    }
    if id.contains("..") {
        return Err("subscription id has a doubled '.'");
    }
    let Some(dot_v) = id.rfind(".v") else {
        return Err("subscription id must contain a .v version suffix");
    };
    let name = &id[..dot_v];
    let version = &id[dot_v + 2..];
    if name.is_empty() {
        return Err("subscription id name prefix is empty");
    }
    if !name
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return Err("subscription id must start with [a-z0-9]");
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err("subscription id has characters outside [a-z0-9._-]");
    }
    if version.is_empty() {
        return Err("subscription id missing version digits after .v");
    }
    let first = version.as_bytes()[0];
    if !first.is_ascii_digit() || first == b'0' {
        return Err("subscription id version must start with 1-9");
    }
    if !version.chars().all(|c| c.is_ascii_digit()) {
        return Err("subscription id version must be digits only");
    }
    Ok(())
}

fn validate_reason_code(code: &str) -> Result<(), &'static str> {
    if code.is_empty() {
        return Err("empty reason code");
    }
    if code.len() > MAX_STREAM_REASON_CODE_BYTES {
        return Err("reason code longer than 128 bytes");
    }
    if !code
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err("reason code has characters outside [a-z0-9._-]");
    }
    Ok(())
}

fn validate_payload_schema_ref(reference: &str) -> Result<(), &'static str> {
    if reference.is_empty() {
        return Err("empty payload schema ref");
    }
    if reference.len() > MAX_PAYLOAD_SCHEMA_REF_BYTES {
        return Err("payload schema ref longer than 256 bytes");
    }
    if reference.contains(char::is_whitespace) {
        return Err("payload schema ref contains whitespace");
    }
    if !reference
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err("payload schema ref has characters outside [a-z0-9._-]");
    }
    Ok(())
}
