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
    /// A client-visible schema reference resolves but the descriptor carries no
    /// [`crate::schema_shape::SchemaShape`].
    SchemaShapeMissing {
        /// Module containing the reference or owning the exported payload schema.
        module: String,
        /// Operation containing the reference, when operation-owned.
        operation: Option<String>,
        /// Referenced schema id.
        reference: String,
        /// Required role for the reference.
        role: String,
    },
    /// `build` was called with no mounted modules — an empty host has no
    /// operations to serve and no identity to fingerprint.
    EmptyHost,
    /// Lowering the mounted modules into a `syncbat` runtime failed.
    Build(syncbat::BuildError),
    /// A subscription id failed grammar or length validation.
    SubscriptionInvalidId {
        /// Attempted subscription id.
        id: String,
        /// Stable detail describing the violation.
        detail: String,
    },
    /// A projection id failed grammar or length validation.
    SubscriptionInvalidProjectionId {
        /// Attempted projection id.
        id: String,
        /// Stable detail describing the violation.
        detail: String,
    },
    /// Two subscriptions in one module declare the same globally unique id.
    SubscriptionDuplicateWithinModule {
        /// The offending module id.
        module: String,
        /// The duplicated subscription id.
        id: String,
    },
    /// Two mounted modules declare the same globally unique subscription id.
    DuplicateSubscriptionId {
        /// The duplicated subscription id.
        id: String,
        /// The module that re-declared it.
        module: String,
    },
    /// An exported subscription source uses a reserved or out-of-range category.
    SubscriptionReservedCategory {
        /// Rejected category value.
        category: u8,
    },
    /// A subscription references a payload schema id missing from the composition
    /// for the source-required role.
    SubscriptionPayloadSchemaMissing {
        /// Module containing the subscription.
        module: String,
        /// Subscription id.
        subscription: String,
        /// Referenced schema id.
        reference: String,
        /// Required schema role.
        role: String,
    },
    /// An event payload binding is malformed.
    EventPayloadBindingInvalid {
        /// Bound event kind (raw u16).
        kind: u16,
        /// Stable detail describing the violation.
        detail: String,
    },
    /// Two bindings in one module declare the same event kind.
    EventPayloadBindingDuplicateWithinModule {
        /// The offending module id.
        module: String,
        /// Duplicated event kind (raw u16).
        kind: u16,
    },
    /// Two mounted modules bind the same event kind.
    DuplicateEventPayloadBinding {
        /// Duplicated event kind (raw u16).
        kind: u16,
        /// The module that re-declared the binding.
        module: String,
    },
    /// Two mounted modules bind the same event kind to different payload schemas.
    EventPayloadBindingConflict {
        /// Conflicted event kind (raw u16).
        kind: u16,
        /// Module that first bound the kind.
        first_module: String,
        /// First binding's payload schema id.
        first_schema_ref: String,
        /// Module whose conflicting binding was rejected.
        second_module: String,
        /// Rejected binding's payload schema id.
        second_schema_ref: String,
    },
    /// An event payload binding references a schema id missing from the composition.
    EventPayloadBindingSchemaMissing {
        /// Module containing the binding.
        module: String,
        /// Bound event kind (raw u16).
        kind: u16,
        /// Referenced schema id.
        reference: String,
    },
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
            Self::DuplicateModuleId { .. }
            | Self::DuplicateOperation { .. }
            | Self::EffectConflict { .. }
            | Self::DuplicateReceiptNamespace { .. }
            | Self::DuplicateJobKind { .. }
            | Self::DuplicateSubscriptionId { .. }
            | Self::ModuleHashMismatch { .. }
            | Self::ModuleCoherence { .. }
            | Self::EmptyHost
            | Self::Build(_) => fmt_host_wiring_error(self, f),
            Self::CanonicalEncoding { detail } => write!(f, "canonical encoding failed: {detail}"),
            Self::SchemaInvalid { .. }
            | Self::SchemaCollision(_)
            | Self::SchemaReferenceMissing { .. }
            | Self::SchemaReferenceAmbiguous { .. }
            | Self::SchemaValidation { .. }
            | Self::SchemaShapeMissing { .. }
            | Self::SubscriptionInvalidId { .. }
            | Self::SubscriptionInvalidProjectionId { .. }
            | Self::SubscriptionDuplicateWithinModule { .. }
            | Self::SubscriptionReservedCategory { .. }
            | Self::SubscriptionPayloadSchemaMissing { .. }
            | Self::EventPayloadBindingInvalid { .. }
            | Self::EventPayloadBindingDuplicateWithinModule { .. }
            | Self::DuplicateEventPayloadBinding { .. }
            | Self::EventPayloadBindingConflict { .. }
            | Self::EventPayloadBindingSchemaMissing { .. } => fmt_schema_error(self, f),
        }
    }
}

fn fmt_host_wiring_error(error: &HostError, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match error {
        HostError::DuplicateModuleId { id } => write!(f, "module id {id:?} is already mounted"),
        HostError::DuplicateOperation { operation, module } => write!(
            f,
            "operation {operation:?} re-declared by module {module:?} is already mounted"
        ),
        HostError::EffectConflict { operation, module } => write!(
            f,
            "operation {operation:?} re-declared by module {module:?} has conflicting effect authority"
        ),
        HostError::DuplicateReceiptNamespace { namespace, module } => write!(
            f,
            "receipt namespace {namespace:?} re-claimed by module {module:?} is already mounted"
        ),
        HostError::DuplicateJobKind { kind, module } => write!(
            f,
            "supervised-job kind {kind:?} re-declared by module {module:?} is already mounted"
        ),
        HostError::DuplicateSubscriptionId { id, module } => write!(
            f,
            "subscription id {id:?} re-declared by module {module:?} is already mounted"
        ),
        HostError::ModuleHashMismatch { module } => write!(
            f,
            "module {module:?} manifest hash does not match its declared parts"
        ),
        HostError::ModuleCoherence { module, detail } => {
            write!(f, "module {module:?} is incoherent: {detail}")
        }
        HostError::EmptyHost => write!(f, "cannot build a host with no mounted modules"),
        HostError::Build(error) => write!(f, "lowering into the syncbat runtime failed: {error}"),
        HostError::SubscriptionInvalidId { .. }
        | HostError::SubscriptionInvalidProjectionId { .. }
        | HostError::SubscriptionDuplicateWithinModule { .. }
        | HostError::SubscriptionReservedCategory { .. }
        | HostError::SubscriptionPayloadSchemaMissing { .. }
        | HostError::EventPayloadBindingInvalid { .. }
        | HostError::EventPayloadBindingDuplicateWithinModule { .. }
        | HostError::DuplicateEventPayloadBinding { .. }
        | HostError::EventPayloadBindingConflict { .. }
        | HostError::EventPayloadBindingSchemaMissing { .. }
        | HostError::SchemaInvalid { .. }
        | HostError::SchemaCollision(_)
        | HostError::SchemaReferenceMissing { .. }
        | HostError::SchemaReferenceAmbiguous { .. }
        | HostError::SchemaValidation { .. }
        | HostError::SchemaShapeMissing { .. } => fmt_schema_error(error, f),
        HostError::CanonicalEncoding { detail } => write!(f, "canonical encoding failed: {detail}"),
    }
}

fn fmt_schema_error(error: &HostError, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match error {
        HostError::SchemaInvalid { schema, detail } => {
            write!(f, "schema {schema:?} is invalid: {detail}")
        }
        HostError::SchemaCollision(collision) => fmt_schema_collision(collision, f),
        HostError::SchemaReferenceMissing {
            module,
            operation,
            reference,
            role,
        } => fmt_schema_reference_missing(module, operation.as_deref(), reference, role, f),
        HostError::SchemaReferenceAmbiguous {
            module,
            operation,
            reference,
            role,
            versions,
        } => fmt_schema_reference_ambiguous(
            module,
            operation.as_deref(),
            reference,
            role,
            versions,
            f,
        ),
        HostError::SchemaValidation {
            schema,
            role,
            detail,
        } => write!(
            f,
            "schema validation failed for {schema:?} ({role}): {detail}"
        ),
        HostError::SchemaShapeMissing {
            module,
            operation,
            reference,
            role,
        } => {
            if let Some(operation) = operation {
                write!(
                    f,
                    "module {module:?} operation {operation:?} references client-visible schema {reference:?} ({role}) without a structural shape"
                )
            } else {
                write!(
                    f,
                    "module {module:?} references client-visible schema {reference:?} ({role}) without a structural shape"
                )
            }
        }
        HostError::SubscriptionInvalidId { .. }
        | HostError::SubscriptionInvalidProjectionId { .. }
        | HostError::SubscriptionDuplicateWithinModule { .. }
        | HostError::DuplicateSubscriptionId { .. }
        | HostError::SubscriptionReservedCategory { .. }
        | HostError::SubscriptionPayloadSchemaMissing { .. } => fmt_subscription_error(error, f),
        HostError::EventPayloadBindingInvalid { .. }
        | HostError::EventPayloadBindingDuplicateWithinModule { .. }
        | HostError::DuplicateEventPayloadBinding { .. }
        | HostError::EventPayloadBindingConflict { .. }
        | HostError::EventPayloadBindingSchemaMissing { .. } => {
            fmt_event_payload_binding_error(error, f)
        }
        HostError::CanonicalEncoding { detail } => write!(f, "canonical encoding failed: {detail}"),
        HostError::DuplicateModuleId { .. }
        | HostError::DuplicateOperation { .. }
        | HostError::EffectConflict { .. }
        | HostError::DuplicateReceiptNamespace { .. }
        | HostError::DuplicateJobKind { .. }
        | HostError::ModuleHashMismatch { .. }
        | HostError::ModuleCoherence { .. }
        | HostError::EmptyHost
        | HostError::Build(_) => fmt_host_wiring_error(error, f),
    }
}

fn fmt_subscription_error(error: &HostError, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match error {
        HostError::SubscriptionInvalidId { id, detail } => {
            write!(f, "subscription id {id:?} is invalid: {detail}")
        }
        HostError::SubscriptionInvalidProjectionId { id, detail } => {
            write!(f, "projection id {id:?} is invalid: {detail}")
        }
        HostError::SubscriptionDuplicateWithinModule { module, id } => write!(
            f,
            "module {module:?} declares subscription id {id:?} twice"
        ),
        HostError::DuplicateSubscriptionId { id, module } => write!(
            f,
            "subscription id {id:?} re-declared by module {module:?} is already mounted"
        ),
        HostError::SubscriptionReservedCategory { category } => write!(
            f,
            "subscription event category 0x{category:02x} is reserved or out of range"
        ),
        HostError::SubscriptionPayloadSchemaMissing {
            module,
            subscription,
            reference,
            role,
        } => write!(
            f,
            "module {module:?} subscription {subscription:?} references missing schema {reference:?} for role {role}"
        ),
        HostError::SchemaInvalid { .. }
        | HostError::SchemaCollision(_)
        | HostError::SchemaReferenceMissing { .. }
        | HostError::SchemaReferenceAmbiguous { .. }
        | HostError::SchemaValidation { .. }
        | HostError::SchemaShapeMissing { .. }
        | HostError::EventPayloadBindingInvalid { .. }
        | HostError::EventPayloadBindingDuplicateWithinModule { .. }
        | HostError::DuplicateEventPayloadBinding { .. }
        | HostError::EventPayloadBindingConflict { .. }
        | HostError::EventPayloadBindingSchemaMissing { .. }
        | HostError::CanonicalEncoding { .. }
        | HostError::DuplicateModuleId { .. }
        | HostError::DuplicateOperation { .. }
        | HostError::EffectConflict { .. }
        | HostError::DuplicateReceiptNamespace { .. }
        | HostError::DuplicateJobKind { .. }
        | HostError::ModuleHashMismatch { .. }
        | HostError::ModuleCoherence { .. }
        | HostError::EmptyHost
        | HostError::Build(_) => fmt_schema_error(error, f),
    }
}

fn fmt_event_payload_binding_error(
    error: &HostError,
    f: &mut std::fmt::Formatter<'_>,
) -> std::fmt::Result {
    match error {
        HostError::EventPayloadBindingInvalid { kind, detail } => write!(
            f,
            "event payload binding for kind 0x{kind:04x} is invalid: {detail}"
        ),
        HostError::EventPayloadBindingDuplicateWithinModule { module, kind } => {
            write!(f, "module {module:?} binds event kind 0x{kind:04x} twice")
        }
        HostError::DuplicateEventPayloadBinding { kind, module } => write!(
            f,
            "event kind 0x{kind:04x} re-bound by module {module:?} is already mounted"
        ),
        HostError::EventPayloadBindingConflict {
            kind,
            first_module,
            first_schema_ref,
            second_module,
            second_schema_ref,
        } => write!(
            f,
            "event kind 0x{kind:04x} bound to conflicting payload schemas: \
             module {first_module:?} => {first_schema_ref:?}, \
             module {second_module:?} => {second_schema_ref:?}"
        ),
        HostError::EventPayloadBindingSchemaMissing {
            module,
            kind,
            reference,
        } => write!(
            f,
            "module {module:?} binds event kind 0x{kind:04x} to missing schema {reference:?}"
        ),
        HostError::SchemaInvalid { .. }
        | HostError::SchemaCollision(_)
        | HostError::SchemaReferenceMissing { .. }
        | HostError::SchemaReferenceAmbiguous { .. }
        | HostError::SchemaValidation { .. }
        | HostError::SchemaShapeMissing { .. }
        | HostError::SubscriptionInvalidId { .. }
        | HostError::SubscriptionInvalidProjectionId { .. }
        | HostError::SubscriptionDuplicateWithinModule { .. }
        | HostError::DuplicateSubscriptionId { .. }
        | HostError::SubscriptionReservedCategory { .. }
        | HostError::SubscriptionPayloadSchemaMissing { .. }
        | HostError::CanonicalEncoding { .. }
        | HostError::DuplicateModuleId { .. }
        | HostError::DuplicateOperation { .. }
        | HostError::EffectConflict { .. }
        | HostError::DuplicateReceiptNamespace { .. }
        | HostError::DuplicateJobKind { .. }
        | HostError::ModuleHashMismatch { .. }
        | HostError::ModuleCoherence { .. }
        | HostError::EmptyHost
        | HostError::Build(_) => fmt_schema_error(error, f),
    }
}

fn fmt_schema_collision(
    collision: &SchemaCollision,
    f: &mut std::fmt::Formatter<'_>,
) -> std::fmt::Result {
    let SchemaCollision {
        schema,
        version,
        role,
        first_module,
        first_encoding,
        second_module,
        second_encoding,
    } = collision;
    write!(
        f,
        "schema {schema:?} v{version} ({role}) declared with conflicting encodings: \
         module {first_module:?} => {first_encoding}, \
        module {second_module:?} => {second_encoding}"
    )
}

fn fmt_schema_reference_missing(
    module: &str,
    operation: Option<&str>,
    reference: &str,
    role: &str,
    f: &mut std::fmt::Formatter<'_>,
) -> std::fmt::Result {
    write!(
        f,
        "module {module:?} references missing schema {reference:?} for role {role}"
    )?;
    if let Some(operation) = operation {
        write!(f, " on operation {operation:?}")?;
    }
    Ok(())
}

fn fmt_schema_reference_ambiguous(
    module: &str,
    operation: Option<&str>,
    reference: &str,
    role: &str,
    versions: &[u32],
    f: &mut std::fmt::Formatter<'_>,
) -> std::fmt::Result {
    write!(
        f,
        "module {module:?} references ambiguous schema {reference:?} for role {role}; versions: {versions:?}"
    )?;
    if let Some(operation) = operation {
        write!(f, " on operation {operation:?}")?;
    }
    Ok(())
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
            | Self::DuplicateSubscriptionId { .. }
            | Self::ModuleHashMismatch { .. }
            | Self::ModuleCoherence { .. }
            | Self::CanonicalEncoding { .. }
            | Self::SchemaInvalid { .. }
            | Self::SchemaCollision(_)
            | Self::SchemaReferenceMissing { .. }
            | Self::SchemaReferenceAmbiguous { .. }
            | Self::SchemaValidation { .. }
            | Self::SchemaShapeMissing { .. }
            | Self::SubscriptionInvalidId { .. }
            | Self::SubscriptionInvalidProjectionId { .. }
            | Self::SubscriptionDuplicateWithinModule { .. }
            | Self::SubscriptionReservedCategory { .. }
            | Self::SubscriptionPayloadSchemaMissing { .. }
            | Self::EventPayloadBindingInvalid { .. }
            | Self::EventPayloadBindingDuplicateWithinModule { .. }
            | Self::DuplicateEventPayloadBinding { .. }
            | Self::EventPayloadBindingConflict { .. }
            | Self::EventPayloadBindingSchemaMissing { .. }
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
