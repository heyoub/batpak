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

/// Stable crate-layer rule for docs, diagnostics, and tests.
pub const LAYER_RULE: &str = "nb exposes, sb dispatches, bp records";

/// A syncbat operation exposed at a server/network boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Endpoint {
    operation_name: String,
    path: String,
}

impl Endpoint {
    /// Create an endpoint for an operation and boundary path.
    #[must_use]
    pub fn new(operation_name: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            operation_name: operation_name.into(),
            path: path.into(),
        }
    }

    /// Stable syncbat operation name exposed by this endpoint.
    #[must_use]
    pub fn operation_name(&self) -> &str {
        &self.operation_name
    }

    /// Boundary path associated with this endpoint.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }
}

/// A mounted boundary route.
///
/// A route maps boundary metadata to a syncbat operation name. It is not a
/// dispatcher and carries no transport server implementation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Route {
    method: &'static str,
    endpoint: Endpoint,
}

impl Route {
    /// Create a route with a stable method label and endpoint.
    #[must_use]
    pub fn new(method: &'static str, endpoint: Endpoint) -> Self {
        Self { method, endpoint }
    }

    /// Stable method label for the boundary route.
    #[must_use]
    pub fn method(&self) -> &'static str {
        self.method
    }

    /// Endpoint exposed by this route.
    #[must_use]
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Stable syncbat operation name exposed by this route.
    #[must_use]
    pub fn operation_name(&self) -> &str {
        self.endpoint.operation_name()
    }

    /// Boundary path associated with this route.
    #[must_use]
    pub fn path(&self) -> &str {
        self.endpoint.path()
    }
}

/// Server-facing wrapper for a data-oriented syncbat module.
///
/// `ServerModule` owns the syncbat module descriptor so it can be mounted into
/// a [`syncbat::CoreBuilder`] later by the caller. It only derives route
/// metadata from operation descriptors.
pub struct ServerModule {
    module: syncbat::Module,
    routes: Vec<Route>,
}

impl ServerModule {
    /// Wrap a syncbat module and expose each operation under `base_path`.
    ///
    /// Paths are formed as `{base_path}/{operation_name}` with a single slash
    /// between the base and the operation name.
    #[must_use]
    pub fn expose(module: syncbat::Module, base_path: impl AsRef<str>) -> Self {
        let base_path = normalize_base_path(base_path.as_ref());
        let routes = module
            .operations()
            .map(|(name, _)| Route::new("CALL", Endpoint::new(name, format!("{base_path}/{name}"))))
            .collect();

        Self { module, routes }
    }

    /// Wrapped syncbat module descriptor.
    #[must_use]
    pub fn module(&self) -> &syncbat::Module {
        &self.module
    }

    /// Stable module name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.module.name()
    }

    /// Exposed routes derived from the module operation descriptors.
    #[must_use]
    pub fn routes(&self) -> &[Route] {
        &self.routes
    }

    /// Number of exposed operations.
    #[must_use]
    pub fn operation_count(&self) -> usize {
        self.module.operation_count()
    }

    /// Consume the wrapper and return the syncbat module descriptor.
    #[must_use]
    pub fn into_module(self) -> syncbat::Module {
        self.module
    }
}

/// Minimal server-boundary registry.
///
/// `Server` stores exposed modules and route metadata. Transport helpers in
/// this crate dispatch only by calling [`syncbat::Core`] APIs; the server
/// registry itself stays metadata-only.
#[derive(Default)]
pub struct Server {
    modules: Vec<ServerModule>,
}

impl Server {
    /// Create an empty server-boundary registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mount server-facing module metadata.
    pub fn mount(&mut self, module: ServerModule) -> &mut Self {
        self.modules.push(module);
        self
    }

    /// Mounted server-facing modules.
    #[must_use]
    pub fn modules(&self) -> &[ServerModule] {
        &self.modules
    }

    /// Iterate all exposed routes in mount order.
    pub fn routes(&self) -> impl Iterator<Item = &Route> {
        self.modules.iter().flat_map(|module| module.routes())
    }

    /// Build an introspection report over mounted module metadata.
    #[must_use]
    pub fn introspect(&self) -> Introspection {
        introspect_modules(&self.modules)
    }
}

/// Introspection report for exposed boundary metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Introspection {
    /// Number of exposed modules.
    pub module_count: usize,
    /// Number of exposed operations.
    pub operation_count: usize,
    /// Number of exposed routes.
    pub route_count: usize,
    /// Human-readable layer rule.
    pub layer_rule: &'static str,
}

/// Build an introspection report over server-facing module metadata.
#[must_use]
pub fn introspect_modules(modules: &[ServerModule]) -> Introspection {
    let operation_count = modules
        .iter()
        .map(ServerModule::operation_count)
        .sum::<usize>();
    let route_count = modules
        .iter()
        .map(|module| module.routes().len())
        .sum::<usize>();

    Introspection {
        module_count: modules.len(),
        operation_count,
        route_count,
        layer_rule: LAYER_RULE,
    }
}

/// Borrowed health check over a syncbat core's mounted operation descriptors.
///
/// This report is descriptor-only. It does not invoke handlers or claim
/// transport readiness.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoreHealth {
    /// Operation names present in the borrowed syncbat core.
    pub mounted_operations: Vec<String>,
    /// Operation names absent from the borrowed syncbat core.
    pub missing_operations: Vec<String>,
    /// Human-readable layer rule.
    pub layer_rule: &'static str,
}

impl CoreHealth {
    /// Return true when every inspected operation name is mounted.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.missing_operations.is_empty()
    }
}

/// Inspect whether named operations are mounted in a borrowed syncbat core.
///
/// This is a boundary health/introspection helper only; syncbat remains the
/// owner of dispatch and batpak remains the owner of durable records.
#[must_use]
pub fn inspect_core_operations<I, S>(core: &syncbat::Core, operation_names: I) -> CoreHealth
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut mounted_operations = Vec::new();
    let mut missing_operations = Vec::new();

    for name in operation_names {
        let name = name.as_ref();
        if core.contains_operation(name) {
            mounted_operations.push(name.to_owned());
        } else {
            missing_operations.push(name.to_owned());
        }
    }

    CoreHealth {
        mounted_operations,
        missing_operations,
        layer_rule: LAYER_RULE,
    }
}

fn normalize_base_path(base_path: &str) -> String {
    let trimmed = base_path.trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("/{trimmed}")
    }
}

use std::error::Error;
use std::fmt;
use std::io::{self, Read, Write};

/// Default maximum request line size accepted by the line transport.
pub const DEFAULT_MAX_LINE_BYTES: usize = 64 * 1024;
/// Default maximum operation name size accepted by the line transport.
pub const DEFAULT_MAX_OPERATION_NAME_BYTES: usize = 256;
/// Default maximum decoded input size accepted by the line transport.
pub const DEFAULT_MAX_INPUT_BYTES: usize = 32 * 1024;
/// Default maximum handler output size encoded into a response frame.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 32 * 1024;

/// Bounded transport limits for netbat's blocking line protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    /// Maximum bytes read before a newline terminator is required.
    pub max_line_bytes: usize,
    /// Maximum bytes allowed in the operation name token.
    pub max_operation_name_bytes: usize,
    /// Maximum decoded input bytes accepted by dispatch.
    pub max_input_bytes: usize,
    /// Maximum output bytes encoded into a success response.
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
    fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } => "io",
            Self::EmptyStream => "empty_stream",
            Self::LineTooLong { .. } => "line_too_long",
            Self::MalformedRequest { .. } => "malformed_request",
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
    let verb = parts.next().ok_or(NetbatError::MalformedRequest {
        reason: "missing verb",
    })?;
    let operation = parts.next().ok_or(NetbatError::MalformedRequest {
        reason: "missing operation",
    })?;
    let input = parts.next().ok_or(NetbatError::MalformedRequest {
        reason: "missing input",
    })?;

    if parts.next().is_some() {
        return Err(NetbatError::MalformedRequest {
            reason: "too many fields",
        });
    }
    if verb != b"CALL" {
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
    if operation
        .iter()
        .any(|byte| !byte.is_ascii_graphic() || byte.is_ascii_whitespace())
    {
        return Err(NetbatError::MalformedRequest {
            reason: "operation has invalid bytes",
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

fn decode_hex(input: &[u8], max_input_bytes: usize) -> Result<Vec<u8>, NetbatError> {
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

fn encode_hex_into(bytes: &[u8], output: &mut Vec<u8>) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    output.reserve(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize]);
        output.push(HEX[(byte & 0x0f) as usize]);
    }
}
