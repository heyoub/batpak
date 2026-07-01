//! Declared and observed operation effect rows.
//!
//! Every observed effect axis is an enforced boundary, not a cooperative audit
//! trail. An operation may read events, append events, query projections, emit
//! receipts, or use host controls ONLY through the matching `Ctx` capability
//! handle. Each handle performs its effect through the runtime-owned
//! [`EffectBackend`] and records it into the observed row in the same step, so
//! an operation cannot observe an effect the runtime did not mediate; `checkout`
//! then fails closed when the observed row is not a subset of the declared row.

use serde::{Deserialize, Serialize};

use batpak::event::EventKind;

use crate::effect_backend::{EffectBackend, EffectError};
use crate::operation::{DescriptorValidationError, EffectClass, MAX_DESCRIPTOR_REF_BYTES};
use crate::receipt::ReceiptMetadata;

const EVENT_APPEND_CAPABILITY: &str = "event.append";
const EVENT_READ_CAPABILITY: &str = "event.read";
const HOST_CONTROL_CAPABILITY: &str = "host.control";
const PROJECTION_QUERY_CAPABILITY: &str = "projection.query";
const RECEIPT_EMIT_CAPABILITY: &str = "receipt.emit";

/// Return true when `token` is an effect-axis capability auto-declared by an
/// effect builder (`reads_event`, `appends_event`, `queries_projection`,
/// `emits_receipt`, `uses_host_control`).
///
/// Those axes are already mediated by the observed-effect subset check at
/// checkout, so their tokens are ambient: the runtime grant gate skips them and
/// the Core need not be granted them explicitly. Every OTHER declared capability
/// token (e.g. one added via [`OperationEffectRow::requires_capability`] or the
/// `#[operation]` macro) is gated against the Core's granted capability set.
pub(crate) fn is_reserved_effect_capability(token: &str) -> bool {
    matches!(
        token,
        EVENT_APPEND_CAPABILITY
            | EVENT_READ_CAPABILITY
            | HOST_CONTROL_CAPABILITY
            | PROJECTION_QUERY_CAPABILITY
            | RECEIPT_EMIT_CAPABILITY
    )
}

/// Canonical, stable append-target identity for an event kind.
///
/// The runtime records this for every event an operation appends, so an
/// operation that wants its appends authorized declares the same value with
/// [`OperationEffectRow::appends_event`]. Declaring a non-canonical free string
/// is allowed (documentation) but will not match a real append.
#[must_use]
pub fn append_target(kind: EventKind) -> String {
    format!("evt.{:04x}", kind.as_raw_u16())
}

/// A stable operation effect declaration or observation.
///
/// Each target list is kept sorted and deduplicated so subset checks are
/// deterministic. The row is intentionally data-only: descriptors declare it,
/// invocation contexts observe it through capability handles, and the runtime
/// checks observed effects against declared authority.
#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct OperationEffectRow {
    /// Event categories read by the operation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    reads_events: Vec<String>,
    /// Event append-targets appended by the operation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    appends_events: Vec<String>,
    /// Projection ids queried by the operation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    queries_projections: Vec<String>,
    /// Receipt kinds emitted by the operation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    emits_receipts: Vec<String>,
    /// Host-control ids used by the operation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    uses_host_controls: Vec<String>,
    /// Capability tokens required or observed for this row.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    requires_capabilities: Vec<String>,
}

impl OperationEffectRow {
    /// Empty effect row for pure inspect/compute operations.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            reads_events: Vec::new(),
            appends_events: Vec::new(),
            queries_projections: Vec::new(),
            emits_receipts: Vec::new(),
            uses_host_controls: Vec::new(),
            requires_capabilities: Vec::new(),
        }
    }

    /// New empty effect row.
    #[must_use]
    pub const fn new() -> Self {
        Self::empty()
    }

    /// Event categories read by the operation.
    #[must_use]
    pub fn reads_events(&self) -> &[String] {
        &self.reads_events
    }

    /// Event append-targets appended by the operation.
    #[must_use]
    pub fn appends_events(&self) -> &[String] {
        &self.appends_events
    }

    /// Projection ids queried by the operation.
    #[must_use]
    pub fn queries_projections(&self) -> &[String] {
        &self.queries_projections
    }

    /// Receipt kinds emitted by the operation.
    #[must_use]
    pub fn emits_receipts(&self) -> &[String] {
        &self.emits_receipts
    }

    /// Host-control ids used by the operation.
    #[must_use]
    pub fn uses_host_controls(&self) -> &[String] {
        &self.uses_host_controls
    }

    /// Capability tokens required by the declaration or observed by handles.
    #[must_use]
    pub fn requires_capabilities(&self) -> &[String] {
        &self.requires_capabilities
    }

    /// Return true when the row declares or observes no effects.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.reads_events.is_empty()
            && self.appends_events.is_empty()
            && self.queries_projections.is_empty()
            && self.emits_receipts.is_empty()
            && self.uses_host_controls.is_empty()
            && self.requires_capabilities.is_empty()
    }

    /// Canonical effect identities keyed by `(kind, target-id)`.
    ///
    /// The returned identities are encoded with BatPak's named-field
    /// MessagePack helper and sorted by their canonical bytes. This is the
    /// stable projection transport/tooling layers can use when they need the
    /// same deterministic effect-key surface as runtime enforcement.
    ///
    /// # Errors
    /// Returns [`EffectIdentityError`] if the canonical encoder refuses one of
    /// the simple `(kind, target-id)` records.
    pub fn canonical_identities(&self) -> Result<Vec<EffectIdentity>, EffectIdentityError> {
        let mut identities = Vec::new();
        push_identities(&mut identities, "reads_events", &self.reads_events)?;
        push_identities(&mut identities, "appends_events", &self.appends_events)?;
        push_identities(
            &mut identities,
            "queries_projections",
            &self.queries_projections,
        )?;
        push_identities(&mut identities, "emits_receipts", &self.emits_receipts)?;
        push_identities(
            &mut identities,
            "uses_host_controls",
            &self.uses_host_controls,
        )?;
        push_identities(
            &mut identities,
            "requires_capabilities",
            &self.requires_capabilities,
        )?;
        identities.sort();
        Ok(identities)
    }

    /// Declare that this operation reads an event category.
    #[must_use]
    pub fn reads_event(mut self, event_category: impl Into<String>) -> Self {
        insert_sorted(&mut self.reads_events, event_category.into());
        insert_sorted(
            &mut self.requires_capabilities,
            EVENT_READ_CAPABILITY.to_owned(),
        );
        self
    }

    /// Declare that this operation may append events of `event_target`.
    ///
    /// Use [`append_target`] to derive the canonical target for an `EventKind`
    /// so the declaration matches what the runtime records when the operation
    /// appends that kind through its `Ctx` handle.
    #[must_use]
    pub fn appends_event(mut self, event_target: impl Into<String>) -> Self {
        insert_sorted(&mut self.appends_events, event_target.into());
        insert_sorted(
            &mut self.requires_capabilities,
            EVENT_APPEND_CAPABILITY.to_owned(),
        );
        self
    }

    /// Declare that this operation queries a projection id.
    #[must_use]
    pub fn queries_projection(mut self, projection_id: impl Into<String>) -> Self {
        insert_sorted(&mut self.queries_projections, projection_id.into());
        insert_sorted(
            &mut self.requires_capabilities,
            PROJECTION_QUERY_CAPABILITY.to_owned(),
        );
        self
    }

    /// Declare that this operation emits a receipt kind.
    #[must_use]
    pub fn emits_receipt(mut self, receipt_kind: impl Into<String>) -> Self {
        insert_sorted(&mut self.emits_receipts, receipt_kind.into());
        insert_sorted(
            &mut self.requires_capabilities,
            RECEIPT_EMIT_CAPABILITY.to_owned(),
        );
        self
    }

    /// Declare that this operation uses the host control identified by `control`.
    #[must_use]
    pub fn uses_host_control(mut self, control: impl Into<String>) -> Self {
        insert_sorted(&mut self.uses_host_controls, control.into());
        insert_sorted(
            &mut self.requires_capabilities,
            HOST_CONTROL_CAPABILITY.to_owned(),
        );
        self
    }

    /// Declare an additional capability token required by this operation.
    #[must_use]
    pub fn requires_capability(mut self, capability: impl Into<String>) -> Self {
        insert_sorted(&mut self.requires_capabilities, capability.into());
        self
    }

    fn record_appends_event(&mut self, event_target: impl Into<String>) {
        insert_sorted(&mut self.appends_events, event_target.into());
        insert_sorted(
            &mut self.requires_capabilities,
            EVENT_APPEND_CAPABILITY.to_owned(),
        );
    }

    pub(crate) fn record_reads_event(&mut self, event_category: impl Into<String>) {
        insert_sorted(&mut self.reads_events, event_category.into());
        insert_sorted(
            &mut self.requires_capabilities,
            EVENT_READ_CAPABILITY.to_owned(),
        );
    }

    pub(crate) fn record_queries_projection(&mut self, projection_id: impl Into<String>) {
        insert_sorted(&mut self.queries_projections, projection_id.into());
        insert_sorted(
            &mut self.requires_capabilities,
            PROJECTION_QUERY_CAPABILITY.to_owned(),
        );
    }

    pub(crate) fn record_emits_receipt(&mut self, receipt_kind: impl Into<String>) {
        insert_sorted(&mut self.emits_receipts, receipt_kind.into());
        insert_sorted(
            &mut self.requires_capabilities,
            RECEIPT_EMIT_CAPABILITY.to_owned(),
        );
    }

    pub(crate) fn record_uses_host_control(&mut self, control: impl Into<String>) {
        insert_sorted(&mut self.uses_host_controls, control.into());
        insert_sorted(
            &mut self.requires_capabilities,
            HOST_CONTROL_CAPABILITY.to_owned(),
        );
    }

    /// First observed effect outside the declared authority, if any.
    ///
    /// `self` is the authoritative observed row (built only from effects that
    /// flowed through the `Ctx` handles); `declared` is the descriptor's row.
    /// A violation is any observed target the declaration did not authorize.
    pub(crate) fn first_violation_against(
        &self,
        declared: &Self,
    ) -> Option<ObservedEffectViolation> {
        first_missing("reads_events", &self.reads_events, &declared.reads_events)
            .or_else(|| {
                first_missing(
                    "appends_events",
                    &self.appends_events,
                    &declared.appends_events,
                )
            })
            .or_else(|| {
                first_missing(
                    "queries_projections",
                    &self.queries_projections,
                    &declared.queries_projections,
                )
            })
            .or_else(|| {
                first_missing(
                    "emits_receipts",
                    &self.emits_receipts,
                    &declared.emits_receipts,
                )
            })
            .or_else(|| {
                first_missing(
                    "uses_host_controls",
                    &self.uses_host_controls,
                    &declared.uses_host_controls,
                )
            })
            .or_else(|| {
                first_missing(
                    "requires_capabilities",
                    &self.requires_capabilities,
                    &declared.requires_capabilities,
                )
            })
    }

    pub(crate) fn validate_for_descriptor(
        &self,
        effect: EffectClass,
        receipt_kind: &str,
    ) -> Result<(), DescriptorValidationError> {
        self.validate_targets()?;
        match effect {
            EffectClass::Inspect => {
                if !self.appends_events.is_empty()
                    || !self.emits_receipts.is_empty()
                    || !self.uses_host_controls.is_empty()
                {
                    return Err(DescriptorValidationError::new(
                        "effect_row",
                        effect.as_str(),
                        "inspect operations cannot declare append, receipt emit, or host control effects",
                    ));
                }
            }
            EffectClass::Compute => {
                if !self.is_empty() {
                    return Err(DescriptorValidationError::new(
                        "effect_row",
                        effect.as_str(),
                        "compute operations cannot declare runtime effects",
                    ));
                }
            }
            EffectClass::Persist => {
                if self.appends_events.is_empty() {
                    return Err(DescriptorValidationError::new(
                        "effect_row",
                        effect.as_str(),
                        "persist operations must declare event appends",
                    ));
                }
                if !self.emits_receipts.is_empty() || !self.uses_host_controls.is_empty() {
                    return Err(DescriptorValidationError::new(
                        "effect_row",
                        effect.as_str(),
                        "persist operations cannot declare receipt emit or host control effects",
                    ));
                }
            }
            EffectClass::Emit => {
                if !contains(&self.emits_receipts, receipt_kind) {
                    return Err(DescriptorValidationError::new(
                        "effect_row",
                        effect.as_str(),
                        "emit operations must declare their receipt kind",
                    ));
                }
                if !self.appends_events.is_empty() || !self.uses_host_controls.is_empty() {
                    return Err(DescriptorValidationError::new(
                        "effect_row",
                        effect.as_str(),
                        "emit operations cannot declare event append or host control effects",
                    ));
                }
            }
            EffectClass::Control => {
                if self.uses_host_controls.is_empty() {
                    return Err(DescriptorValidationError::new(
                        "effect_row",
                        effect.as_str(),
                        "control operations must declare host control use",
                    ));
                }
            }
        }
        Ok(())
    }

    fn validate_targets(&self) -> Result<(), DescriptorValidationError> {
        for (field, targets) in [
            ("reads_events", self.reads_events.as_slice()),
            ("appends_events", self.appends_events.as_slice()),
            ("queries_projections", self.queries_projections.as_slice()),
            ("emits_receipts", self.emits_receipts.as_slice()),
            ("uses_host_controls", self.uses_host_controls.as_slice()),
            (
                "requires_capabilities",
                self.requires_capabilities.as_slice(),
            ),
        ] {
            validate_target_list(field, targets)?;
        }
        Ok(())
    }
}

/// Canonical byte identity for one effect target.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EffectIdentity {
    bytes: Vec<u8>,
}

impl EffectIdentity {
    fn new(kind: &'static str, target_id: &str) -> Result<Self, EffectIdentityError> {
        let view = EffectIdentityView { kind, target_id };
        let bytes = batpak::canonical::to_bytes(&view).map_err(|error| EffectIdentityError {
            message: error.to_string(),
        })?;
        Ok(Self { bytes })
    }

    /// Encoded canonical identity bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Failure to encode an effect identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectIdentityError {
    message: String,
}

impl std::fmt::Display for EffectIdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "effect identity encoding failed: {}", self.message)
    }
}

impl std::error::Error for EffectIdentityError {}

#[derive(Serialize)]
struct EffectIdentityView<'a> {
    kind: &'static str,
    target_id: &'a str,
}

/// One observed effect outside the descriptor's declared authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedEffectViolation {
    code: &'static str,
    message: String,
}

impl ObservedEffectViolation {
    fn undeclared(field: &'static str, target: impl Into<String>) -> Self {
        Self {
            code: "effect.violation",
            message: format!("observed undeclared {field} target {:?}", target.into()),
        }
    }

    /// Stable denial code for this violation.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    /// Stable denial message for this violation.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Capability handle that performs and records event appends.
///
/// This is the only path an operation has to the runtime's event log. Calling
/// [`append_event`](Self::append_event) writes through the runtime-owned
/// [`EffectBackend`] and records the append into the observed row in one step,
/// so an operation cannot append an event without it being observed and checked.
pub struct EventAppendHandle<'a> {
    row: &'a mut OperationEffectRow,
    backend: Option<&'a mut (dyn EffectBackend + 'static)>,
}

impl<'a> EventAppendHandle<'a> {
    pub(crate) fn new(
        row: &'a mut OperationEffectRow,
        backend: Option<&'a mut (dyn EffectBackend + 'static)>,
    ) -> Self {
        Self { row, backend }
    }

    /// Append one event of `kind` carrying `payload` through the runtime backend
    /// and record it as an observed append.
    ///
    /// # Errors
    /// Returns [`EffectError`] when no backend is bound for this invocation or
    /// the backend rejects the append.
    pub fn append_event(&mut self, kind: EventKind, payload: &[u8]) -> Result<(), EffectError> {
        let backend = self.backend.as_deref_mut().ok_or_else(|| {
            EffectError::new("no effect backend is bound for this invocation; cannot append events")
        })?;
        backend.append_event(kind, payload)?;
        self.row.record_appends_event(append_target(kind));
        Ok(())
    }
}

/// Capability handle that performs and records event-category reads.
pub struct EventReadHandle<'a> {
    row: &'a mut OperationEffectRow,
    backend: Option<&'a mut (dyn EffectBackend + 'static)>,
}

impl<'a> EventReadHandle<'a> {
    pub(crate) fn new(
        row: &'a mut OperationEffectRow,
        backend: Option<&'a mut (dyn EffectBackend + 'static)>,
    ) -> Self {
        Self { row, backend }
    }

    /// Read one event category through the runtime backend and record it as an
    /// observed read.
    ///
    /// # Errors
    /// Returns [`EffectError`] when no backend is bound for this invocation or
    /// the backend rejects the read.
    pub fn read_event(&mut self, event_category: impl Into<String>) -> Result<(), EffectError> {
        let event_category = event_category.into();
        let backend = require_effect_backend(self.backend.as_deref_mut(), "read events")?;
        backend.read_event(&event_category)?;
        self.row.record_reads_event(event_category);
        Ok(())
    }
}

/// Capability handle that performs and records projection queries.
pub struct ProjectionReadHandle<'a> {
    row: &'a mut OperationEffectRow,
    backend: Option<&'a mut (dyn EffectBackend + 'static)>,
}

impl<'a> ProjectionReadHandle<'a> {
    pub(crate) fn new(
        row: &'a mut OperationEffectRow,
        backend: Option<&'a mut (dyn EffectBackend + 'static)>,
    ) -> Self {
        Self { row, backend }
    }

    /// Query one projection id through the runtime backend and record it as an
    /// observed projection query.
    ///
    /// # Errors
    /// Returns [`EffectError`] when no backend is bound for this invocation or
    /// the backend rejects the query.
    pub fn query_projection(
        &mut self,
        projection_id: impl Into<String>,
    ) -> Result<(), EffectError> {
        let projection_id = projection_id.into();
        let backend = require_effect_backend(self.backend.as_deref_mut(), "query projections")?;
        backend.query_projection(&projection_id)?;
        self.row.record_queries_projection(projection_id);
        Ok(())
    }
}

/// Capability handle that performs and records receipt emission.
///
/// Emission is a declared effect axis that also contributes evidence: the
/// runtime already banks exactly one invocation receipt per op, and this handle
/// stamps the emitted opaque payload into that receipt's LOCAL drawer. So an
/// `emits_receipt` declaration is not decorative — the emitted bytes ride the
/// handle into the runtime's single banked receipt.
pub struct ReceiptEmitHandle<'a> {
    row: &'a mut OperationEffectRow,
    backend: Option<&'a mut (dyn EffectBackend + 'static)>,
    emit_meta: &'a mut ReceiptMetadata,
}

impl<'a> ReceiptEmitHandle<'a> {
    pub(crate) fn new(
        row: &'a mut OperationEffectRow,
        backend: Option<&'a mut (dyn EffectBackend + 'static)>,
        emit_meta: &'a mut ReceiptMetadata,
    ) -> Self {
        Self {
            row,
            backend,
            emit_meta,
        }
    }

    /// Emit one receipt kind through the runtime backend, stamp `payload` as
    /// opaque evidence into the invocation's LOCAL receipt drawer, and record it
    /// as an observed emission.
    ///
    /// The backend mediates the emission first (fail-closed when unbound); only
    /// on a successful mediation is `payload` stamped, under the runtime-owned
    /// LOCAL drawer key `syncbat.emit_receipt.{receipt_kind}`, so the runtime's
    /// single banked invocation receipt carries the emitted evidence. The
    /// payload rides this handle into [`ReceiptMetadata`], so the
    /// [`EffectBackend`] trait keeps its `&str`-only `emit_receipt` signature.
    ///
    /// # Errors
    /// Returns [`EffectError`] when no backend is bound for this invocation or
    /// the backend rejects the emission; in either case nothing is stamped.
    pub fn emit_receipt(
        &mut self,
        receipt_kind: impl Into<String>,
        payload: impl Into<Vec<u8>>,
    ) -> Result<(), EffectError> {
        let receipt_kind = receipt_kind.into();
        let backend = require_effect_backend(self.backend.as_deref_mut(), "emit receipts")?;
        backend.emit_receipt(&receipt_kind)?;
        self.emit_meta
            .local
            .insert(emit_receipt_local_key(&receipt_kind), payload.into());
        self.row.record_emits_receipt(receipt_kind);
        Ok(())
    }
}

/// Runtime-owned LOCAL receipt-drawer key an emitted receipt's opaque payload is
/// stamped under, namespaced by `receipt_kind` so distinct emitted kinds do not
/// collide within one invocation's banked receipt.
fn emit_receipt_local_key(receipt_kind: &str) -> String {
    format!("syncbat.emit_receipt.{receipt_kind}")
}

/// Capability handle that performs and records host-control use.
pub struct HostControlHandle<'a> {
    row: &'a mut OperationEffectRow,
    backend: Option<&'a mut (dyn EffectBackend + 'static)>,
}

impl<'a> HostControlHandle<'a> {
    pub(crate) fn new(
        row: &'a mut OperationEffectRow,
        backend: Option<&'a mut (dyn EffectBackend + 'static)>,
    ) -> Self {
        Self { row, backend }
    }

    /// Use the host control identified by `control` through the runtime backend
    /// and record it as an observed host-control effect.
    ///
    /// # Errors
    /// Returns [`EffectError`] when no backend is bound for this invocation or
    /// the backend rejects the use.
    pub fn use_host_control(&mut self, control: impl Into<String>) -> Result<(), EffectError> {
        let control = control.into();
        let backend = require_effect_backend(self.backend.as_deref_mut(), "use host controls")?;
        backend.use_host_control(&control)?;
        self.row.record_uses_host_control(control);
        Ok(())
    }
}

fn require_effect_backend<'a>(
    backend: Option<&'a mut (dyn EffectBackend + 'static)>,
    capability: &str,
) -> Result<&'a mut (dyn EffectBackend + 'static), EffectError> {
    backend.ok_or_else(|| {
        EffectError::new(format!(
            "no effect backend is bound for this invocation; cannot {capability}"
        ))
    })
}

fn first_missing(
    field: &'static str,
    observed: &[String],
    declared: &[String],
) -> Option<ObservedEffectViolation> {
    observed
        .iter()
        .find(|target| !contains(declared, target))
        .map(|target| ObservedEffectViolation::undeclared(field, target.clone()))
}

fn push_identities(
    identities: &mut Vec<EffectIdentity>,
    kind: &'static str,
    targets: &[String],
) -> Result<(), EffectIdentityError> {
    for target in targets {
        identities.push(EffectIdentity::new(kind, target)?);
    }
    Ok(())
}

fn contains(targets: &[String], target: &str) -> bool {
    // Linear, not binary search: the subset check must stay correct even when a
    // row is reconstructed via `Deserialize` (e.g. a catalog round-trip), where
    // the vec ordering is not guaranteed. Effect rows are tiny, so the linear
    // scan is cheap. `insert_sorted` still keeps builder-made rows sorted for
    // deterministic canonical-identity bytes.
    targets.iter().any(|candidate| candidate == target)
}

fn insert_sorted(targets: &mut Vec<String>, target: String) {
    match targets.binary_search(&target) {
        Ok(_) => {}
        Err(index) => targets.insert(index, target),
    }
}

fn validate_target_list(
    field: &'static str,
    targets: &[String],
) -> Result<(), DescriptorValidationError> {
    for target in targets {
        validate_effect_target(field, target)?;
    }
    Ok(())
}

fn validate_effect_target(
    field: &'static str,
    value: &str,
) -> Result<(), DescriptorValidationError> {
    if value.is_empty() {
        return Err(DescriptorValidationError::new(field, value, "empty"));
    }
    if value.len() > MAX_DESCRIPTOR_REF_BYTES {
        return Err(DescriptorValidationError::new(field, value, "too long"));
    }
    if value
        .bytes()
        .any(|byte| !matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
    {
        return Err(DescriptorValidationError::new(
            field,
            value,
            "expected ASCII letters, digits, '.', '_' or '-'",
        ));
    }
    if value.starts_with('.') || value.ends_with('.') || value.contains("..") {
        return Err(DescriptorValidationError::new(
            field,
            value,
            "dot-separated tokens must be non-empty",
        ));
    }
    Ok(())
}
