//! Typed host-composition failures.
//!
//! Every error names exactly what made the composition refuse: a collision
//! between two mounted modules, a module whose stored manifest hash does not
//! match its derived parts (tamper detection), an internally incoherent module,
//! a canonical-encoding failure, or a lowered `syncbat` build error. The host
//! fails closed: any one of these aborts `mount`/`build` rather than producing a
//! partially-wired runtime.

use crate::descriptor::HookPhase;

/// Detail of a cross-module schema-identity collision (see
/// [`HostError::SchemaCollision`]). Boxed inside the error so the common
/// `Result<_, HostError>` stays small.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaCollision {
    /// The conflicting schema id.
    pub schema: String,
    /// The conflicting schema version.
    pub version: u32,
    /// The conflicting schema role (lowercase spelling).
    pub role: String,
    /// The module that first declared this identity.
    pub first_module: String,
    /// The first declaration's canonical encoding (hex).
    pub first_encoding: String,
    /// The module whose differing declaration was rejected.
    pub second_module: String,
    /// The rejected declaration's canonical encoding (hex).
    pub second_encoding: String,
}

/// Why a host composition refused.
#[derive(Debug)]
#[non_exhaustive]
pub enum HostError {
    /// Two mounted modules declare the same module id.
    DuplicateModuleId {
        /// The duplicated module id.
        id: String,
    },
    /// Two mounted modules declare the same operation name (operation names are
    /// globally unique across the composed host).
    DuplicateOperation {
        /// The duplicated operation name.
        operation: String,
        /// The id of the module that re-declared it.
        module: String,
    },
    /// Two mounted modules redeclare an operation name with different
    /// fine-grained effect authority.
    EffectConflict {
        /// The conflicted operation name.
        operation: String,
        /// The id of the module that re-declared it.
        module: String,
    },
    /// Two mounted modules claim the same receipt-extension namespace.
    DuplicateReceiptNamespace {
        /// The duplicated namespace.
        namespace: String,
        /// The id of the module that re-claimed it.
        module: String,
    },
    /// Two mounted modules declare the same supervised-job kind (job kinds are
    /// globally unique — one supervisor owns them all).
    DuplicateJobKind {
        /// The duplicated job kind.
        kind: String,
        /// The id of the module that re-declared it.
        module: String,
    },
    /// A module's stored manifest hash does not match the hash recomputed from
    /// its declared parts — the manifest was tampered with or constructed out of
    /// band. Detected at mount, before any wiring.
    ModuleHashMismatch {
        /// The id of the offending module.
        module: String,
    },
    /// A module is internally incoherent (e.g. an operation with no handler, a
    /// guard impl with no guard descriptor, or a duplicated hook order).
    ModuleCoherence {
        /// The id of the offending module.
        module: String,
        /// Stable detail describing the incoherence.
        detail: String,
    },
    /// Canonical encoding of a manifest or the host fingerprint failed. The
    /// frozen wire shapes make this unreachable in practice; it is surfaced
    /// rather than panicked.
    CanonicalEncoding {
        /// Stable detail from the canonical encoder.
        detail: String,
    },
    /// A schema descriptor is malformed (e.g. an invalid [`crate::schema::SchemaId`]
    /// grammar or a duplicated golden-vector case).
    SchemaInvalid {
        /// The offending schema id (or attempted id).
        schema: String,
        /// Stable detail describing the violation.
        detail: String,
    },
    /// Two mounted modules declare the same `(SchemaId, SchemaVersion, role)`
    /// with *different* canonical encodings — a wire-identity conflict the
    /// composition refuses to seal (fail-closed). Identical re-declarations are
    /// allowed; this fires only on a byte divergence at a fixed identity. The
    /// payload is boxed to keep [`HostError`] small.
    SchemaCollision(Box<SchemaCollision>),
    /// An operation descriptor references a schema id that the mounted
    /// composition does not declare for the required role.
    SchemaReferenceMissing {
        /// Module containing the reference.
        module: String,
        /// Operation containing the reference, when the reference is operation-owned.
        operation: Option<String>,
        /// Referenced schema id.
        reference: String,
        /// Required role for the reference.
        role: String,
    },
    /// An operation descriptor references a schema id with more than one
    /// declared version for the required role. The v1 string-ref surface admits
    /// only exact unique refs; callers must pick one id per version or move to a
    /// future typed ref.
    SchemaReferenceAmbiguous {
        /// Module containing the reference.
        module: String,
        /// Operation containing the reference, when the reference is operation-owned.
        operation: Option<String>,
        /// Referenced schema id.
        reference: String,
        /// Required role for the reference.
        role: String,
        /// Versions found for the referenced id and role.
        versions: Vec<u32>,
    },
    /// Runtime schema validation failed for canonical payload bytes.
    SchemaValidation {
        /// Schema id being validated.
        schema: String,
        /// Required schema role.
        role: String,
        /// Stable validation detail.
        detail: String,
    },
    /// `build` was called with no mounted modules — an empty host has no
    /// operations to serve and no identity to fingerprint.
    EmptyHost,
    /// Lowering the mounted modules into a `syncbat` runtime failed.
    Build(syncbat::BuildError),
}

impl HostError {
    /// Construct a [`HostError::ModuleCoherence`].
    pub(crate) fn coherence(module: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::ModuleCoherence {
            module: module.into(),
            detail: detail.into(),
        }
    }

    /// Construct a [`HostError::ModuleCoherence`] for a within-module hook-order
    /// collision in `phase`.
    pub(crate) fn hook_order_collision(
        module: impl Into<String>,
        phase: HookPhase,
        order: u32,
    ) -> Self {
        Self::coherence(
            module,
            format!("two {phase} hooks share order {order} (ambiguous ordering)"),
        )
    }
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateModuleId { id } => {
                write!(f, "module id {id:?} is already mounted")
            }
            Self::DuplicateOperation { operation, module } => {
                write!(
                    f,
                    "operation {operation:?} re-declared by module {module:?} is already mounted"
                )
            }
            Self::EffectConflict { operation, module } => {
                write!(
                    f,
                    "operation {operation:?} re-declared by module {module:?} has conflicting effect authority"
                )
            }
            Self::DuplicateReceiptNamespace { namespace, module } => {
                write!(
                    f,
                    "receipt namespace {namespace:?} re-claimed by module {module:?} is already mounted"
                )
            }
            Self::DuplicateJobKind { kind, module } => {
                write!(
                    f,
                    "supervised-job kind {kind:?} re-declared by module {module:?} is already mounted"
                )
            }
            Self::ModuleHashMismatch { module } => {
                write!(
                    f,
                    "module {module:?} manifest hash does not match its declared parts"
                )
            }
            Self::ModuleCoherence { module, detail } => {
                write!(f, "module {module:?} is incoherent: {detail}")
            }
            Self::CanonicalEncoding { detail } => {
                write!(f, "canonical encoding failed: {detail}")
            }
            Self::SchemaInvalid { schema, detail } => {
                write!(f, "schema {schema:?} is invalid: {detail}")
            }
            Self::SchemaCollision(collision) => {
                let SchemaCollision {
                    schema,
                    version,
                    role,
                    first_module,
                    first_encoding,
                    second_module,
                    second_encoding,
                } = collision.as_ref();
                write!(
                    f,
                    "schema {schema:?} v{version} ({role}) declared with conflicting encodings: \
                     module {first_module:?} => {first_encoding}, \
                    module {second_module:?} => {second_encoding}"
                )
            }
            Self::SchemaReferenceMissing {
                module,
                operation,
                reference,
                role,
            } => {
                write!(
                    f,
                    "module {module:?} references missing schema {reference:?} for role {role}"
                )?;
                if let Some(operation) = operation {
                    write!(f, " on operation {operation:?}")?;
                }
                Ok(())
            }
            Self::SchemaReferenceAmbiguous {
                module,
                operation,
                reference,
                role,
                versions,
            } => {
                write!(
                    f,
                    "module {module:?} references ambiguous schema {reference:?} for role {role}; versions: {versions:?}"
                )?;
                if let Some(operation) = operation {
                    write!(f, " on operation {operation:?}")?;
                }
                Ok(())
            }
            Self::SchemaValidation {
                schema,
                role,
                detail,
            } => write!(
                f,
                "schema validation failed for {schema:?} ({role}): {detail}"
            ),
            Self::EmptyHost => write!(f, "cannot build a host with no mounted modules"),
            Self::Build(error) => write!(f, "lowering into the syncbat runtime failed: {error}"),
        }
    }
}

impl std::error::Error for HostError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Build(error) => Some(error),
            Self::DuplicateModuleId { .. }
            | Self::DuplicateOperation { .. }
            | Self::EffectConflict { .. }
            | Self::DuplicateReceiptNamespace { .. }
            | Self::DuplicateJobKind { .. }
            | Self::ModuleHashMismatch { .. }
            | Self::ModuleCoherence { .. }
            | Self::CanonicalEncoding { .. }
            | Self::SchemaInvalid { .. }
            | Self::SchemaCollision(_)
            | Self::SchemaReferenceMissing { .. }
            | Self::SchemaReferenceAmbiguous { .. }
            | Self::SchemaValidation { .. }
            | Self::EmptyHost => None,
        }
    }
}

impl From<syncbat::BuildError> for HostError {
    fn from(error: syncbat::BuildError) -> Self {
        Self::Build(error)
    }
}

/// A lifecycle hook signalled failure during host start or shutdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookFailure {
    /// The phase the hook ran in.
    pub phase: HookPhase,
    /// The module that owns the hook.
    pub module: String,
    /// The hook's stable name.
    pub hook: String,
    /// Stable failure detail returned by the hook.
    pub detail: String,
}

impl std::fmt::Display for HookFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{phase} hook {hook:?} of module {module:?} failed: {detail}",
            phase = self.phase,
            hook = self.hook,
            module = self.module,
            detail = self.detail,
        )
    }
}

impl std::error::Error for HookFailure {}

/// Why a host runtime operation (start, shutdown, or job spawn) failed.
///
/// Distinct from [`HostError`], which is composition-time. A started host runs
/// its hooks in deterministic order and aborts on the first failure (fail-closed:
/// startup stops, shutdown surfaces the failure after attempting the rest).
#[derive(Debug)]
#[non_exhaustive]
pub enum HostRuntimeError {
    /// A startup hook failed; the host did not finish starting.
    StartupHook(HookFailure),
    /// A shutdown hook failed.
    ShutdownHook(HookFailure),
    /// A job was spawned for a kind no mounted module declares.
    UnknownJobKind {
        /// The requested job kind.
        kind: String,
    },
    /// The supervisor could not spawn the job body over the [`batpak::store::Spawn`]
    /// seam.
    Spawn(batpak::store::SpawnError),
}

impl std::fmt::Display for HostRuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StartupHook(failure) => write!(f, "host startup failed: {failure}"),
            Self::ShutdownHook(failure) => write!(f, "host shutdown failed: {failure}"),
            Self::UnknownJobKind { kind } => {
                write!(f, "no mounted module declares supervised-job kind {kind:?}")
            }
            Self::Spawn(error) => write!(f, "supervisor could not spawn the job: {error}"),
        }
    }
}

impl std::error::Error for HostRuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::StartupHook(failure) | Self::ShutdownHook(failure) => Some(failure),
            Self::Spawn(error) => Some(error),
            Self::UnknownJobKind { .. } => None,
        }
    }
}

impl From<batpak::store::SpawnError> for HostRuntimeError {
    fn from(error: batpak::store::SpawnError) -> Self {
        Self::Spawn(error)
    }
}
