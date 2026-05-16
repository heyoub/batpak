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

use std::error::Error;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Stable crate-layer rule for docs, diagnostics, and tests.
pub const LAYER_RULE: &str = "nb exposes, sb dispatches, bp records";

/// Maximum bytes accepted for a boundary route path.
pub const MAX_ROUTE_PATH_BYTES: usize = 512;

/// Boundary route validation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RouteValidationError {
    /// Operation name failed boundary validation.
    InvalidOperationName {
        /// Invalid operation name.
        name: String,
        /// Stable validation message.
        message: &'static str,
    },
    /// Boundary path failed validation.
    InvalidPath {
        /// Invalid boundary path.
        path: String,
        /// Stable validation message.
        message: &'static str,
    },
    /// Boundary method label failed validation.
    InvalidMethod {
        /// Invalid method label.
        method: String,
        /// Stable validation message.
        message: &'static str,
    },
    /// Wrapped syncbat module descriptor failed validation.
    InvalidModule(syncbat::RegisterValidationError),
    /// Two mounted routes would expose the same method/path pair.
    DuplicateRoute {
        /// Boundary method label.
        method: &'static str,
        /// Boundary path.
        path: String,
    },
}

impl fmt::Display for RouteValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOperationName { name, message } => {
                write!(
                    f,
                    "operation name `{name}` is invalid for a route: {message}"
                )
            }
            Self::InvalidPath { path, message } => {
                write!(f, "route path `{path}` is invalid: {message}")
            }
            Self::InvalidMethod { method, message } => {
                write!(f, "route method `{method}` is invalid: {message}")
            }
            Self::InvalidModule(error) => write!(f, "module is invalid for exposure: {error}"),
            Self::DuplicateRoute { method, path } => {
                write!(f, "duplicate boundary route {method} {path}")
            }
        }
    }
}

impl Error for RouteValidationError {}

impl From<syncbat::RegisterValidationError> for RouteValidationError {
    fn from(error: syncbat::RegisterValidationError) -> Self {
        Self::InvalidModule(error)
    }
}

/// A syncbat operation exposed at a server/network boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Endpoint {
    operation_name: String,
    path: String,
}

impl Endpoint {
    /// Create an endpoint for an operation and boundary path.
    ///
    /// # Errors
    /// Returns [`RouteValidationError`] when the operation name or path is not
    /// valid for a server boundary route.
    pub fn new(
        operation_name: impl Into<String>,
        path: impl Into<String>,
    ) -> Result<Self, RouteValidationError> {
        let operation_name = operation_name.into();
        let path = path.into();
        validate_route_operation_name(&operation_name)?;
        validate_route_path(&path)?;
        Ok(Self {
            operation_name,
            path,
        })
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
    ///
    /// # Errors
    /// Returns [`RouteValidationError`] when the method label is not valid for
    /// a stable boundary route.
    pub fn new(method: &'static str, endpoint: Endpoint) -> Result<Self, RouteValidationError> {
        validate_route_method(method)?;
        Ok(Self { method, endpoint })
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
    ///
    /// # Errors
    /// Returns [`RouteValidationError`] when the module descriptor or derived
    /// route metadata fails boundary validation.
    pub fn expose(
        module: syncbat::Module,
        base_path: impl AsRef<str>,
    ) -> Result<Self, RouteValidationError> {
        module.validate()?;
        let base_path = normalize_base_path(base_path.as_ref());
        validate_base_path(&base_path)?;
        let mut routes = Vec::with_capacity(module.operation_count());
        for (name, _) in module.operations() {
            let endpoint = Endpoint::new(name, format!("{base_path}/{name}"))?;
            routes.push(Route::new("CALL", endpoint)?);
        }

        Ok(Self { module, routes })
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
    ///
    /// # Errors
    /// Returns [`RouteValidationError::DuplicateRoute`] if a mounted module
    /// already exposes the same method/path pair.
    pub fn mount(&mut self, module: ServerModule) -> Result<&mut Self, RouteValidationError> {
        for route in module.routes() {
            if self.routes().any(|existing| {
                existing.method() == route.method() && existing.path() == route.path()
            }) {
                return Err(RouteValidationError::DuplicateRoute {
                    method: route.method(),
                    path: route.path().to_owned(),
                });
            }
        }
        self.modules.push(module);
        Ok(self)
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

fn validate_base_path(path: &str) -> Result<(), RouteValidationError> {
    if path.is_empty() {
        return Ok(());
    }
    validate_route_path(path)
}

fn validate_route_method(method: &str) -> Result<(), RouteValidationError> {
    if method.is_empty() {
        return Err(RouteValidationError::InvalidMethod {
            method: method.to_owned(),
            message: "empty",
        });
    }
    if method
        .bytes()
        .any(|byte| !matches!(byte, b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-'))
    {
        return Err(RouteValidationError::InvalidMethod {
            method: method.to_owned(),
            message: "expected ASCII uppercase letters, digits, '_' or '-'",
        });
    }
    Ok(())
}

fn validate_route_operation_name(name: &str) -> Result<(), RouteValidationError> {
    if name.is_empty() {
        return Err(RouteValidationError::InvalidOperationName {
            name: name.to_owned(),
            message: "empty",
        });
    }
    if name.len() > syncbat::MAX_OPERATION_NAME_BYTES {
        return Err(RouteValidationError::InvalidOperationName {
            name: name.to_owned(),
            message: "too long",
        });
    }
    if name
        .bytes()
        .any(|byte| !matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
    {
        return Err(RouteValidationError::InvalidOperationName {
            name: name.to_owned(),
            message: "expected ASCII letters, digits, '.', '_' or '-'",
        });
    }
    if name.starts_with('.') || name.ends_with('.') || name.contains("..") {
        return Err(RouteValidationError::InvalidOperationName {
            name: name.to_owned(),
            message: "dot-separated tokens must be non-empty",
        });
    }
    Ok(())
}

fn validate_route_path(path: &str) -> Result<(), RouteValidationError> {
    if path.is_empty() {
        return Err(RouteValidationError::InvalidPath {
            path: path.to_owned(),
            message: "empty",
        });
    }
    if path.len() > MAX_ROUTE_PATH_BYTES {
        return Err(RouteValidationError::InvalidPath {
            path: path.to_owned(),
            message: "too long",
        });
    }
    if !path.starts_with('/') {
        return Err(RouteValidationError::InvalidPath {
            path: path.to_owned(),
            message: "must start with '/'",
        });
    }
    if path == "/" {
        return Err(RouteValidationError::InvalidPath {
            path: path.to_owned(),
            message: "must include at least one segment",
        });
    }
    if path.len() > 1 && path.ends_with('/') {
        return Err(RouteValidationError::InvalidPath {
            path: path.to_owned(),
            message: "must not end with '/'",
        });
    }
    if path.contains("//") {
        return Err(RouteValidationError::InvalidPath {
            path: path.to_owned(),
            message: "empty path segments are not allowed",
        });
    }
    for segment in path.split('/').skip(1) {
        if segment == "." || segment == ".." {
            return Err(RouteValidationError::InvalidPath {
                path: path.to_owned(),
                message: "relative path segments are not allowed",
            });
        }
        if segment.bytes().any(
            |byte| !matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b'-'),
        ) {
            return Err(RouteValidationError::InvalidPath {
                path: path.to_owned(),
                message: "expected ASCII letters, digits, '/', '.', '_' or '-'",
            });
        }
    }
    Ok(())
}

/// Default maximum request line size accepted by the line transport.
pub const DEFAULT_MAX_LINE_BYTES: usize = 64 * 1024;
/// Default maximum operation name size accepted by the line transport.
pub const DEFAULT_MAX_OPERATION_NAME_BYTES: usize = 256;
/// Default maximum decoded input size accepted by the line transport.
pub const DEFAULT_MAX_INPUT_BYTES: usize = 32 * 1024;
/// Default maximum handler output size encoded into a response frame.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 32 * 1024;
/// Current version token accepted by netbat's versioned line protocol.
pub const LINE_PROTOCOL_VERSION: &str = "NETBAT/1";

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
    fn code(&self) -> &'static str {
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
    let (verb, operation, input) = if first.starts_with(b"NETBAT/") {
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
                return Ok(());
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

fn record_request_failure(stats: &mut TcpServeStats, error: &NetbatError) {
    match error {
        NetbatError::LineTooLong { .. }
        | NetbatError::OperationNameTooLong { .. }
        | NetbatError::InputTooLarge { .. }
        | NetbatError::OutputTooLarge { .. } => stats.limit_failures += 1,
        NetbatError::MalformedRequest { .. } | NetbatError::UnsupportedProtocolVersion { .. } => {
            stats.malformed_requests += 1
        }
        NetbatError::Runtime(_) => stats.runtime_failures += 1,
        NetbatError::Io { .. } | NetbatError::EmptyStream => {}
    }
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
