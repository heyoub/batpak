use std::error::Error;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Default maximum request line size accepted by the line transport.
pub const DEFAULT_MAX_LINE_BYTES: usize = 64 * 1024;
/// Default maximum operation name size accepted by the line transport.
pub const DEFAULT_MAX_OPERATION_NAME_BYTES: usize = syncbat::MAX_OPERATION_NAME_BYTES;
/// Default maximum decoded input size accepted by the line transport.
pub const DEFAULT_MAX_INPUT_BYTES: usize = 32 * 1024;
/// Default maximum handler output size encoded into a response frame.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 32 * 1024;

macro_rules! protocol_prefix {
    () => {
        "NETBAT/"
    };
}

/// Prefix used by every versioned netbat line-protocol token.
pub const PROTOCOL_PREFIX: &str = protocol_prefix!();
/// Current version token accepted by netbat's versioned line protocol.
pub const LINE_PROTOCOL_VERSION: &str = concat!(protocol_prefix!(), "1");
/// Request verb used by netbat's line protocol.
pub const CALL_VERB: &str = "CALL";

/// Bounded transport limits for netbat's blocking line protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    /// Maximum bytes read before a newline terminator is required.
    pub max_line_bytes: usize,
    /// Maximum bytes allowed in the operation name token.
    pub max_operation_name_bytes: usize,
    /// Maximum decoded input bytes accepted by dispatch.
    pub max_input_bytes: usize,
    /// Maximum output bytes encoded into a response frame.
    pub max_output_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            max_operation_name_bytes: DEFAULT_MAX_OPERATION_NAME_BYTES,
            max_input_bytes: DEFAULT_MAX_INPUT_BYTES,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }
}

/// Optional read/write timeout hints for listener owners.
///
/// The generic [`serve_stream`] helper works with any [`Read`] + [`Write`]
/// value and cannot apply timeouts itself. Listener owners that use
/// `std::net::TcpStream` can apply these values before passing the stream in.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IoTimeouts {
    /// Read timeout hint.
    pub read: Option<std::time::Duration>,
    /// Write timeout hint.
    pub write: Option<std::time::Duration>,
}

/// Default maximum accepted connections for [`serve_tcp_listener`].
pub const DEFAULT_MAX_CONNECTIONS: usize = 1024;
/// Default maximum requests served from one accepted TCP connection.
pub const DEFAULT_MAX_REQUESTS_PER_CONNECTION: usize = 1;

/// Blocking TCP server limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TcpServerConfig {
    /// Line-protocol request and response limits.
    pub limits: Limits,
    /// Optional per-connection read/write timeouts.
    pub timeouts: IoTimeouts,
    /// Maximum accepted connections before the listener returns.
    pub max_connections: usize,
    /// Maximum requests served per accepted connection.
    pub max_requests_per_connection: usize,
    /// Sleep interval used by the nonblocking accept loop when no connection
    /// is ready.
    pub idle_sleep: Duration,
}

impl Default for TcpServerConfig {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            timeouts: IoTimeouts::default(),
            max_connections: DEFAULT_MAX_CONNECTIONS,
            max_requests_per_connection: DEFAULT_MAX_REQUESTS_PER_CONNECTION,
            idle_sleep: Duration::from_millis(10),
        }
    }
}

/// Shared shutdown flag for blocking TCP listener loops.
#[derive(Clone, Debug, Default)]
pub struct ShutdownHandle {
    inner: Arc<AtomicBool>,
}

impl ShutdownHandle {
    /// Create a new unset shutdown handle.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Request listener shutdown.
    pub fn shutdown(&self) {
        self.inner.store(true, Ordering::Release);
    }

    /// Return true once shutdown has been requested.
    #[must_use]
    pub fn is_shutdown(&self) -> bool {
        self.inner.load(Ordering::Acquire)
    }
}

/// Summary returned after a blocking TCP listener exits.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TcpServeStats {
    /// Number of accepted TCP connections.
    pub accepted_connections: usize,
    /// Number of request frames that produced a successful response.
    pub served_requests: usize,
    /// Number of request frames that produced an error response.
    pub failed_requests: usize,
    /// Failed requests rejected by malformed framing or unsupported protocol.
    pub malformed_requests: usize,
    /// Failed requests rejected by configured line/input/output limits.
    pub limit_failures: usize,
    /// Failed requests rejected by syncbat dispatch.
    pub runtime_failures: usize,
    /// True when the listener exited because its shutdown handle was set.
    pub shutdown_requested: bool,
}

/// Error returned by netbat transport framing or syncbat dispatch.
#[derive(Debug, Eq, PartialEq)]
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
    /// The same token is emitted by [`encode_response`] in the `ERR <code> ...`
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
            Self::Runtime(syncbat::RuntimeError::UnknownOperation { .. }) => "unknown_operation",
            Self::Runtime(syncbat::RuntimeError::MissingHandler { .. }) => "missing_handler",
            Self::Runtime(syncbat::RuntimeError::Handler { .. }) => "handler",
            Self::Runtime(syncbat::RuntimeError::ReceiptSink { .. }) => "receipt_sink",
        }
    }
}

/// Decoded request frame for netbat's blocking line protocol.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestFrame {
    operation: String,
    input: Vec<u8>,
}

impl RequestFrame {
    /// Build a request frame from an operation name and input bytes.
    #[must_use]
    pub fn new(operation: impl Into<String>, input: impl Into<Vec<u8>>) -> Self {
        Self {
            operation: operation.into(),
            input: input.into(),
        }
    }

    /// Requested syncbat operation name.
    #[must_use]
    pub fn operation(&self) -> &str {
        &self.operation
    }

    /// Decoded input bytes.
    #[must_use]
    pub fn input(&self) -> &[u8] {
        &self.input
    }

    /// Consume this request frame and return its parts.
    #[must_use]
    pub fn into_parts(self) -> (String, Vec<u8>) {
        (self.operation, self.input)
    }
}

/// Encoded runtime output returned through a netbat transport frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResponseFrame {
    output: Vec<u8>,
}

impl ResponseFrame {
    /// Build a response frame from output bytes.
    #[must_use]
    pub fn new(output: impl Into<Vec<u8>>) -> Self {
        Self {
            output: output.into(),
        }
    }

    /// Handler output bytes.
    #[must_use]
    pub fn output(&self) -> &[u8] {
        &self.output
    }

    /// Consume this response and return output bytes.
    #[must_use]
    pub fn into_output(self) -> Vec<u8> {
        self.output
    }
}

/// Decode one netbat line-protocol request.
///
/// Format:
///
/// ```text
/// NETBAT/1 CALL <operation-name> <hex-input>\n
/// ```
///
/// The legacy first-rung frame is still accepted for callers that already
/// speak it:
///
/// ```text
/// CALL <operation-name> <hex-input>\n
/// ```
///
/// `operation-name` must be non-empty ASCII graphic bytes with no whitespace.
/// Input bytes are hex-encoded to keep the transport line deterministic and
/// byte-safe without introducing a protocol dependency.
///
/// # Errors
/// Returns [`NetbatError`] when the frame is malformed or exceeds limits.
pub fn decode_line(line: &[u8], limits: &Limits) -> Result<RequestFrame, NetbatError> {
    if line.len() > limits.max_line_bytes {
        return Err(NetbatError::LineTooLong {
            max: limits.max_line_bytes,
        });
    }

    let line = strip_line_ending(line);
    if line.is_empty() {
        return Err(NetbatError::MalformedRequest {
            reason: "empty line",
        });
    }

    let mut parts = line.split(|byte| *byte == b' ');
    let first = parts.next().ok_or(NetbatError::MalformedRequest {
        reason: "missing verb",
    })?;
    let (verb, operation, input) = if first.starts_with(PROTOCOL_PREFIX.as_bytes()) {
        validate_protocol_version(first)?;
        let verb = parts.next().ok_or(NetbatError::MalformedRequest {
            reason: "missing verb",
        })?;
        let operation = parts.next().ok_or(NetbatError::MalformedRequest {
            reason: "missing operation",
        })?;
        let input = parts.next().ok_or(NetbatError::MalformedRequest {
            reason: "missing input",
        })?;
        (verb, operation, input)
    } else {
        let operation = parts.next().ok_or(NetbatError::MalformedRequest {
            reason: "missing operation",
        })?;
        let input = parts.next().ok_or(NetbatError::MalformedRequest {
            reason: "missing input",
        })?;
        (first, operation, input)
    };

    if parts.next().is_some() {
        return Err(NetbatError::MalformedRequest {
            reason: "too many fields",
        });
    }
    if verb != CALL_VERB.as_bytes() {
        return Err(NetbatError::MalformedRequest {
            reason: "unsupported verb",
        });
    }
    validate_operation_name(operation, limits)?;

    let input = decode_hex(input, limits.max_input_bytes)?;
    let operation = std::str::from_utf8(operation)
        .map_err(|_| NetbatError::MalformedRequest {
            reason: "operation is not utf-8",
        })?
        .to_owned();

    Ok(RequestFrame::new(operation, input))
}

/// Encode a stable versioned request line.
///
/// Format:
///
/// ```text
/// NETBAT/1 CALL <operation-name> <hex-input>\n
/// ```
///
/// This helper intentionally does not validate the operation name. The decoder
/// remains the validation boundary so invalid names round-trip into the same
/// [`NetbatError::MalformedRequest`] shape as hand-written frames.
#[must_use]
pub fn encode_request(operation: &str, input: &[u8]) -> Vec<u8> {
    let mut line = Vec::with_capacity(
        LINE_PROTOCOL_VERSION.len()
            + 1
            + CALL_VERB.len()
            + 1
            + operation.len()
            + 1
            + input.len() * 2
            + 1,
    );
    line.extend_from_slice(LINE_PROTOCOL_VERSION.as_bytes());
    line.push(b' ');
    line.extend_from_slice(CALL_VERB.as_bytes());
    line.push(b' ');
    line.extend_from_slice(operation.as_bytes());
    line.push(b' ');
    encode_hex_into(input, &mut line);
    line.push(b'\n');
    line
}

/// Encode a stable response line.
///
/// Success format:
///
/// ```text
/// OK <hex-output>\n
/// ```
///
/// Error format:
///
/// ```text
/// ERR <code> <hex-message>\n
/// ```
#[must_use]
pub fn encode_response(result: Result<&[u8], &NetbatError>) -> Vec<u8> {
    match result {
        Ok(output) => {
            let mut response = b"OK ".to_vec();
            encode_hex_into(output, &mut response);
            response.push(b'\n');
            response
        }
        Err(error) => {
            let mut response = format!("ERR {} ", error.code()).into_bytes();
            encode_hex_into(error.to_string().as_bytes(), &mut response);
            response.push(b'\n');
            response
        }
    }
}

/// Dispatch a decoded request frame through syncbat.
///
/// # Errors
/// Returns [`NetbatError`] when syncbat rejects the checkout or output exceeds
/// configured transport limits.
pub fn dispatch_frame(
    core: &mut syncbat::Core,
    frame: RequestFrame,
    limits: &Limits,
) -> Result<ResponseFrame, NetbatError> {
    validate_request_frame(&frame, limits)?;
    let (operation, input) = frame.into_parts();
    let result = core.checkout_frame(syncbat::CheckoutFrame::new(operation, input))?;
    let output = result.into_output();
    if output.len() > limits.max_output_bytes {
        return Err(NetbatError::OutputTooLarge {
            max: limits.max_output_bytes,
        });
    }
    Ok(ResponseFrame::new(output))
}

/// Serve one request from an already-accepted blocking stream.
///
/// The caller owns listener setup, accept loops, thread pools, TLS, shutdown,
/// admission, and any timeout application. This helper reads one bounded
/// request line, dispatches it through syncbat, writes one response line, and
/// returns the dispatch result.
///
/// # Errors
/// Returns [`NetbatError`] when reading, decoding, dispatching, or writing
/// fails.
///
/// `max_output_bytes` is a transport serialization limit. It is enforced after
/// syncbat dispatch returns output bytes; use runtime gates or handler-level
/// validation when output size must be an admission rule.
pub fn serve_stream<S>(
    stream: &mut S,
    core: &mut syncbat::Core,
    limits: &Limits,
) -> Result<ResponseFrame, NetbatError>
where
    S: Read + Write,
{
    let line = match read_line(stream, limits.max_line_bytes) {
        Ok(line) => line,
        Err(error) => {
            let encoded = encode_response(Err(&error));
            stream.write_all(&encoded)?;
            return Err(error);
        }
    };
    let frame = decode_line(&line, limits);
    let response = match frame {
        Ok(frame) => match dispatch_frame(core, frame, limits) {
            Ok(response) => {
                let encoded = encode_response(Ok(response.output()));
                stream.write_all(&encoded)?;
                return Ok(response);
            }
            Err(error) => {
                let encoded = encode_response(Err(&error));
                stream.write_all(&encoded)?;
                Err(error)
            }
        },
        Err(error) => {
            let encoded = encode_response(Err(&error));
            stream.write_all(&encoded)?;
            Err(error)
        }
    };
    response
}

/// Serve a blocking TCP listener sequentially until shutdown or limits stop it.
///
/// The listener is switched to nonblocking mode so [`ShutdownHandle`] can stop
/// the accept loop without opening a synthetic connection. Each accepted
/// connection is served on the caller's thread; `netbat` does not spawn worker
/// threads and does not require syncbat handlers to be `Send`.
///
/// # Errors
/// Returns [`NetbatError`] when listener configuration, accept, timeout
/// configuration, or response writes fail. Per-request decode/runtime errors
/// are counted in [`TcpServeStats::failed_requests`] after their error response
/// is written.
pub fn serve_tcp_listener(
    listener: TcpListener,
    core: &mut syncbat::Core,
    config: &TcpServerConfig,
    shutdown: &ShutdownHandle,
) -> Result<TcpServeStats, NetbatError> {
    listener.set_nonblocking(true)?;
    let mut stats = TcpServeStats::default();

    while !shutdown.is_shutdown() && stats.accepted_connections < config.max_connections {
        match listener.accept() {
            Ok((stream, _addr)) => {
                stats.accepted_connections += 1;
                apply_timeouts(&stream, config.timeouts)?;
                serve_tcp_connection(stream, core, config, &mut stats)?;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(config.idle_sleep);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }

    stats.shutdown_requested = shutdown.is_shutdown();
    drop(listener);
    Ok(stats)
}

fn serve_tcp_connection(
    mut stream: TcpStream,
    core: &mut syncbat::Core,
    config: &TcpServerConfig,
    stats: &mut TcpServeStats,
) -> Result<(), NetbatError> {
    for _ in 0..config.max_requests_per_connection {
        match serve_stream(&mut stream, core, &config.limits) {
            Ok(_) => stats.served_requests += 1,
            Err(NetbatError::EmptyStream) => return Ok(()),
            Err(error @ NetbatError::Io { .. }) => return Err(error),
            Err(error) => {
                stats.failed_requests += 1;
                record_request_failure(stats, &error);
            }
        }
    }
    Ok(())
}

fn apply_timeouts(stream: &TcpStream, timeouts: IoTimeouts) -> Result<(), NetbatError> {
    stream.set_read_timeout(timeouts.read)?;
    stream.set_write_timeout(timeouts.write)?;
    Ok(())
}

fn read_line<R: Read>(reader: &mut R, max_line_bytes: usize) -> Result<Vec<u8>, NetbatError> {
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];

    loop {
        match reader.read(&mut byte) {
            Ok(0) if line.is_empty() => return Err(NetbatError::EmptyStream),
            Ok(0) => return Ok(line),
            Ok(_) => {
                line.push(byte[0]);
                if line.len() > max_line_bytes {
                    return Err(NetbatError::LineTooLong {
                        max: max_line_bytes,
                    });
                }
                if byte[0] == b'\n' {
                    return Ok(line);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
}

fn strip_line_ending(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n")
        .unwrap_or(line)
        .strip_suffix(b"\r")
        .unwrap_or_else(|| line.strip_suffix(b"\n").unwrap_or(line))
}

fn validate_protocol_version(version: &[u8]) -> Result<(), NetbatError> {
    if version == LINE_PROTOCOL_VERSION.as_bytes() {
        return Ok(());
    }
    Err(NetbatError::UnsupportedProtocolVersion {
        version: String::from_utf8_lossy(version).into_owned(),
    })
}

fn validate_operation_name(operation: &[u8], limits: &Limits) -> Result<(), NetbatError> {
    if operation.is_empty() {
        return Err(NetbatError::MalformedRequest {
            reason: "empty operation",
        });
    }
    if operation.len() > limits.max_operation_name_bytes {
        return Err(NetbatError::OperationNameTooLong {
            max: limits.max_operation_name_bytes,
        });
    }
    if operation.iter().any(|byte| {
        !matches!(
            byte,
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b'-'
        )
    }) {
        return Err(NetbatError::MalformedRequest {
            reason: "operation has invalid bytes",
        });
    }
    if operation.starts_with(b".")
        || operation.ends_with(b".")
        || operation.windows(2).any(|w| w == b"..")
    {
        return Err(NetbatError::MalformedRequest {
            reason: "operation dot segments must be non-empty",
        });
    }
    Ok(())
}

fn validate_request_frame(frame: &RequestFrame, limits: &Limits) -> Result<(), NetbatError> {
    validate_operation_name(frame.operation().as_bytes(), limits)?;
    if frame.input().len() > limits.max_input_bytes {
        return Err(NetbatError::InputTooLarge {
            max: limits.max_input_bytes,
        });
    }
    Ok(())
}

fn record_request_failure(stats: &mut TcpServeStats, error: &NetbatError) {
    match error {
        NetbatError::LineTooLong { .. }
        | NetbatError::OperationNameTooLong { .. }
        | NetbatError::InputTooLarge { .. }
        | NetbatError::OutputTooLarge { .. } => stats.limit_failures += 1,
        NetbatError::MalformedRequest { .. } | NetbatError::UnsupportedProtocolVersion { .. } => {
            stats.malformed_requests += 1;
        }
        NetbatError::Runtime(_) => stats.runtime_failures += 1,
        NetbatError::Io { .. } | NetbatError::EmptyStream => {}
    }
}

/// Decode a lowercase or uppercase hexadecimal byte string with a decoded-size limit.
///
/// # Errors
/// Returns [`NetbatError`] when the hex string has odd length, contains a
/// non-hex byte, or decodes past `max_input_bytes`.
pub fn decode_hex(input: &[u8], max_input_bytes: usize) -> Result<Vec<u8>, NetbatError> {
    if !input.len().is_multiple_of(2) {
        return Err(NetbatError::MalformedRequest {
            reason: "hex input has odd length",
        });
    }
    if input.len() / 2 > max_input_bytes {
        return Err(NetbatError::InputTooLarge {
            max: max_input_bytes,
        });
    }

    let mut output = Vec::with_capacity(input.len() / 2);
    for pair in input.chunks_exact(2) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        output.push((high << 4) | low);
    }
    Ok(output)
}

fn hex_value(byte: u8) -> Result<u8, NetbatError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(NetbatError::MalformedRequest {
            reason: "input is not hex",
        }),
    }
}

/// Append lowercase hexadecimal encoding of `bytes` into `output`.
pub fn encode_hex_into(bytes: &[u8], output: &mut Vec<u8>) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    output.reserve(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize]);
        output.push(HEX[(byte & 0x0f) as usize]);
    }
}
