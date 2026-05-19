use std::error::Error;
use std::fmt;

use crate::transport::CALL_VERB;

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
            routes.push(Route::new(CALL_VERB, endpoint)?);
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
    // Boundary check defers to the syncbat operation-name newtype so the
    // route layer never re-parses the grammar.
    syncbat::OperationName::new(name)
        .map(|_| ())
        .map_err(|error| {
            let message: &'static str = match error {
                syncbat::OperationNameError::Empty => "empty",
                syncbat::OperationNameError::TooLong { .. } => "too long",
                syncbat::OperationNameError::LeadingOrTrailingDot
                | syncbat::OperationNameError::ConsecutiveDots => {
                    "dot-separated tokens must be non-empty"
                }
                syncbat::OperationNameError::IllegalCharacter { .. } => {
                    "expected ASCII letters, digits, '.', '_' or '-'"
                }
                // `OperationNameError` is `#[non_exhaustive]`; any variant added
                // post-1.0 surfaces under a generic message until this layer
                // learns a more specific one.
                _ => "invalid operation name",
            };
            RouteValidationError::InvalidOperationName {
                name: name.to_owned(),
                message,
            }
        })
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
