//! Synchronous runtime composition root.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::admission::{AdmissionDecision, AdmissionGuard};
use crate::effect::{
    is_reserved_effect_capability, EventAppendHandle, EventReadHandle, HostControlHandle,
    OperationEffectRow, ProjectionReadHandle, ReceiptEmitHandle,
};
use crate::effect_backend::EffectBackend;
use crate::error::{ReceiptSinkHandlerCause, RuntimeError};
use crate::operation_status::{OperationStatusFactV1, OperationStatusLifecycle};
use crate::operation_status_sink::OperationStatusSink;
use crate::receipt::{ReceiptHashPolicy, ReceiptMetadata, ReceiptOutcome, RecordedReceipt};
use crate::{handler, operation, receipt};

type HandlerResult = handler::HandlerResult;

type BoxedHandler = Box<dyn handler::Handler + 'static>;
type BoxedReceiptSink = Box<dyn receipt::ReceiptSink + 'static>;
type BoxedStatusSink = Arc<dyn OperationStatusSink + Send + Sync>;
type BoxedAdmissionGuard = Box<dyn AdmissionGuard + 'static>;
type BoxedEffectBackend = Box<dyn EffectBackend + 'static>;

/// Composition root for a sync-first operation runtime.
///
/// `Core` owns the operation descriptors, the matching handler table, an
/// optional pre-handler admission guard, and the optional receipt sink shared
/// with each invocation context. It performs only synchronous dispatch;
/// handlers run on the caller's thread.
pub struct Core {
    pub(crate) descriptors: BTreeMap<String, operation::OperationDescriptor>,
    pub(crate) handlers: BTreeMap<String, BoxedHandler>,
    pub(crate) admission_guard: Option<BoxedAdmissionGuard>,
    pub(crate) receipt_sink: Option<BoxedReceiptSink>,
    pub(crate) status_sink: Option<BoxedStatusSink>,
    pub(crate) receipt_hash_policy: ReceiptHashPolicy,
    /// Runtime-owned capability backend that performs declared effects (event
    /// appends) so observation through `Ctx` is authoritative.
    pub(crate) effect_backend: Option<BoxedEffectBackend>,
    /// Capability tokens this Core is granted. Checkout fails closed when a
    /// dispatched operation declares a required capability token (other than the
    /// ambient effect-axis tokens) that is not in this set.
    pub(crate) granted_capabilities: BTreeSet<String>,
}

impl Core {
    /// Start a new runtime builder.
    #[must_use]
    pub fn builder() -> crate::builder::CoreBuilder {
        crate::builder::CoreBuilder::new()
    }

    /// Return true when an operation descriptor is mounted for `name`.
    #[must_use]
    pub fn contains_operation(&self, name: impl AsRef<str>) -> bool {
        self.descriptors.contains_key(name.as_ref())
    }

    /// Look up an operation descriptor by name.
    #[must_use]
    pub fn descriptor(&self, name: impl AsRef<str>) -> Option<&operation::OperationDescriptor> {
        self.descriptors.get(name.as_ref())
    }

    /// Invoke a registered operation by name.
    ///
    /// The dispatch path intentionally stays small: resolve the descriptor,
    /// resolve the handler, borrow a [`Ctx`], and run the handler synchronously.
    ///
    /// # Errors
    /// Returns [`RuntimeError::UnknownOperation`] when no descriptor is mounted
    /// for `name`, [`RuntimeError::MissingHandler`] when no matching handler is
    /// present, a handler-provided runtime error from the invoked handler, or a
    /// fail-closed receipt-sink error after a resolved handler invocation.
    pub fn invoke(
        &mut self,
        name: impl AsRef<str>,
        input: operation::OperationInput,
    ) -> Result<CheckoutResult, RuntimeError> {
        self.checkout_frame(CheckoutFrame::new(name.as_ref(), input))
    }

    /// Invoke a checkout frame by resolving it against this runtime.
    ///
    /// # Errors
    /// Returns the same errors as [`Core::invoke`].
    pub fn checkout_frame(&mut self, frame: CheckoutFrame) -> Result<CheckoutResult, RuntimeError> {
        let descriptor = self
            .descriptors
            .get(frame.name())
            .cloned()
            .ok_or_else(|| RuntimeError::unknown_operation(frame.name()))?;
        self.checkout(Checkout::from_frame(descriptor, frame))
    }

    /// Invoke a checkout that has already been resolved against a register.
    ///
    /// Runtime receipts are owned by this method. Handlers receive only their
    /// invocation context and cannot write directly to the configured sink.
    ///
    /// # Errors
    /// Returns [`RuntimeError::UnknownOperation`] when this runtime does not
    /// mount the checkout descriptor, [`RuntimeError::MissingHandler`] when no
    /// matching handler is present, a handler-provided runtime error, or a
    /// fail-closed receipt-sink error after a resolved handler invocation.
    #[tracing::instrument(
        name = "syncbat.checkout",
        skip_all,
        fields(
            operation = %checkout.descriptor.name(),
            input_bytes = checkout.input.len(),
            output_bytes = tracing::field::Empty,
            outcome = tracing::field::Empty,
        ),
    )]
    pub fn checkout(&mut self, checkout: Checkout) -> Result<CheckoutResult, RuntimeError> {
        let descriptor = {
            let name = checkout.descriptor.name();
            self.descriptors.get(name).cloned().ok_or_else(|| {
                tracing::warn!(operation = %name, outcome = "unknown_operation", "checkout rejected");
                RuntimeError::unknown_operation(name)
            })?
        };
        let name = descriptor.name();
        let input = checkout.input;
        let status_sink = self.status_sink.clone();
        let receipt_hash_policy = self.receipt_hash_policy.clone();

        // Capability authz: an operation may declare required capability tokens
        // the Core must have been granted. This is a declared-row gate, so it
        // fails closed BEFORE the handler runs (and before any admission guard),
        // emitting the same denial outcome shape the observed-effect check uses.
        self.enforce_granted_capabilities(
            &descriptor,
            &input,
            status_sink.as_deref(),
            &receipt_hash_policy,
        )?;

        // One borrowed context spans the optional guard and the handler, so a
        // guard may stamp receipt metadata (e.g. correlation identity) that
        // survives into the handler's eventual receipt.
        let mut ctx = Ctx::new(&descriptor, self.effect_backend.as_deref_mut());

        // Pre-handler admission: a guard may DENY before the handler runs. This
        // is the only place `Core` dispatch emits `ReceiptOutcome::Denied`.
        if let Some(guard) = self.admission_guard.as_deref() {
            if let AdmissionDecision::Deny { code, message } =
                guard.admit(&descriptor, &input, &mut ctx)
            {
                let metadata = ctx.into_metadata();
                tracing::warn!(
                    operation = %name,
                    code = %code,
                    message = %message,
                    outcome = "denied",
                    "checkout denied by admission guard",
                );
                let outcome = ReceiptOutcome::denied(code.clone(), message.clone());
                record_runtime_status(RuntimeStatusRecord {
                    status_sink: status_sink.as_deref(),
                    receipt_hash_policy: &receipt_hash_policy,
                    descriptor: &descriptor,
                    lifecycle: OperationStatusLifecycle::Denied,
                    input: &input,
                    output: None,
                    code: Some(code.clone()),
                    message: Some(message.clone()),
                    handler_cause: None,
                })?;
                self.record_runtime_receipt(&descriptor, &input, None, outcome, None, metadata)?;
                tracing::Span::current().record("outcome", "denied");
                return Err(RuntimeError::denied(name, code, message));
            }
        }

        record_runtime_status(RuntimeStatusRecord {
            status_sink: status_sink.as_deref(),
            receipt_hash_policy: &receipt_hash_policy,
            descriptor: &descriptor,
            lifecycle: OperationStatusLifecycle::Started,
            input: &input,
            output: None,
            code: None,
            message: None,
            handler_cause: None,
        })?;

        let Some(handler) = self.handlers.get_mut(name) else {
            tracing::error!(operation = %name, outcome = "missing_handler", "checkout rejected");
            record_runtime_status(RuntimeStatusRecord {
                status_sink: status_sink.as_deref(),
                receipt_hash_policy: &receipt_hash_policy,
                descriptor: &descriptor,
                lifecycle: OperationStatusLifecycle::Failed,
                input: &input,
                output: None,
                code: Some("missing_handler".to_owned()),
                message: Some("operation descriptor has no registered handler".to_owned()),
                handler_cause: None,
            })?;
            return Err(RuntimeError::missing_handler(name));
        };
        let handler_result = handler.handle(&input, &mut ctx);
        let observed_effects = ctx.observed_effects().clone();
        let metadata = ctx.into_metadata();

        self.finish_handler_phase(HandlerPhase {
            name,
            descriptor: &descriptor,
            input: &input,
            handler_result,
            observed_effects: &observed_effects,
            metadata,
            status_sink: &status_sink,
            receipt_hash_policy: &receipt_hash_policy,
        })
    }

    /// Fail closed when the descriptor declares a required capability token the
    /// Core was not granted.
    ///
    /// Effect-axis tokens auto-declared by the effect builders are ambient
    /// (mediated by the observed-effect subset check) and skipped here; only the
    /// remaining declared capability tokens are gated against the granted set. On
    /// a missing capability this records a `Denied` status and runtime receipt
    /// and returns [`RuntimeError::denied`], mirroring the observed-effect denial.
    fn enforce_granted_capabilities(
        &self,
        descriptor: &operation::OperationDescriptor,
        input: &[u8],
        status_sink: Option<&(dyn OperationStatusSink + Send + Sync)>,
        receipt_hash_policy: &ReceiptHashPolicy,
    ) -> Result<(), RuntimeError> {
        let missing = descriptor
            .effect_row()
            .requires_capabilities()
            .iter()
            .map(String::as_str)
            .find(|token| {
                !is_reserved_effect_capability(token) && !self.granted_capabilities.contains(*token)
            });
        let Some(missing) = missing else {
            return Ok(());
        };

        let name = descriptor.name();
        let code = "capability.denied";
        let message = format!("operation requires ungranted capability {missing:?}");
        tracing::warn!(
            operation = %name,
            code = %code,
            capability = %missing,
            outcome = "capability_denied",
            "checkout denied by ungranted capability",
        );
        let outcome = ReceiptOutcome::denied(code, message.clone());
        record_runtime_status(RuntimeStatusRecord {
            status_sink,
            receipt_hash_policy,
            descriptor,
            lifecycle: OperationStatusLifecycle::Denied,
            input,
            output: None,
            code: Some(code.to_owned()),
            message: Some(message.clone()),
            handler_cause: None,
        })?;
        self.record_runtime_receipt(
            descriptor,
            input,
            None,
            outcome,
            None,
            ReceiptMetadata::default(),
        )?;
        tracing::Span::current().record("outcome", "capability_denied");
        Err(RuntimeError::denied(name, code, message))
    }

    fn finish_handler_phase(
        &self,
        phase: HandlerPhase<'_>,
    ) -> Result<CheckoutResult, RuntimeError> {
        let HandlerPhase {
            name,
            descriptor,
            input,
            handler_result,
            observed_effects,
            metadata,
            status_sink,
            receipt_hash_policy,
        } = phase;
        if let Some(violation) = observed_effects.first_violation_against(descriptor.effect_row()) {
            tracing::warn!(
                operation = %name,
                code = %violation.code(),
                message = %violation.message(),
                outcome = "effect_denied",
                "checkout denied by observed effect row",
            );
            let outcome = ReceiptOutcome::denied(violation.code(), violation.message());
            record_runtime_status(RuntimeStatusRecord {
                status_sink: status_sink.as_deref(),
                receipt_hash_policy,
                descriptor,
                lifecycle: OperationStatusLifecycle::Denied,
                input,
                output: None,
                code: Some(violation.code().to_owned()),
                message: Some(violation.message().to_owned()),
                handler_cause: None,
            })?;
            self.record_runtime_receipt(descriptor, input, None, outcome, None, metadata)?;
            tracing::Span::current().record("outcome", "effect_denied");
            return Err(RuntimeError::denied(
                name,
                violation.code(),
                violation.message(),
            ));
        }

        let output = match handler_result {
            Ok(output) => output,
            Err(error) => {
                let cause = ReceiptSinkHandlerCause::new(error.class(), error.message());
                let outcome = ReceiptOutcome::failed(cause.code(), cause.message());
                tracing::warn!(
                    operation = %name,
                    code = %cause.code(),
                    message = %cause.message(),
                    outcome = "handler_failed",
                    "checkout failed in handler",
                );
                record_runtime_status(RuntimeStatusRecord {
                    status_sink: status_sink.as_deref(),
                    receipt_hash_policy,
                    descriptor,
                    lifecycle: OperationStatusLifecycle::Failed,
                    input,
                    output: None,
                    code: Some(cause.code().to_owned()),
                    message: Some(cause.message().to_owned()),
                    handler_cause: Some(cause.clone()),
                })?;
                self.record_runtime_receipt(
                    descriptor,
                    input,
                    None,
                    outcome,
                    Some(cause.clone()),
                    metadata,
                )?;
                return Err(RuntimeError::handler(name, cause.code(), cause.message()));
            }
        };
        let recorded_receipt = self.record_runtime_receipt(
            descriptor,
            input,
            Some(output.as_slice()),
            ReceiptOutcome::Completed,
            None,
            metadata,
        )?;
        record_runtime_status(RuntimeStatusRecord {
            status_sink: status_sink.as_deref(),
            receipt_hash_policy,
            descriptor,
            lifecycle: OperationStatusLifecycle::Completed,
            input,
            output: Some(output.as_slice()),
            code: None,
            message: None,
            handler_cause: None,
        })?;
        let span = tracing::Span::current();
        span.record("output_bytes", output.len());
        span.record("outcome", "completed");

        Ok(CheckoutResult {
            descriptor: descriptor.clone(),
            output,
            recorded_receipt,
        })
    }

    fn record_runtime_receipt(
        &self,
        descriptor: &operation::OperationDescriptor,
        input: &[u8],
        output: Option<&[u8]>,
        outcome: ReceiptOutcome,
        handler_cause: Option<ReceiptSinkHandlerCause>,
        metadata: ReceiptMetadata,
    ) -> Result<Option<RecordedReceipt>, RuntimeError> {
        let Some(sink) = self.receipt_sink.as_deref() else {
            return Ok(None);
        };

        let mut envelope = receipt::ReceiptEnvelope::new(descriptor, outcome);
        if let Some(hash) = self.receipt_hash_policy.hash(input) {
            envelope = envelope.with_input_hash(hash);
        }
        if let Some(output) = output {
            if let Some(hash) = self.receipt_hash_policy.hash(output) {
                envelope = envelope.with_output_hash(hash);
            }
        }
        // Drain handler/guard-attached metadata into the receipt drawers. The
        // runtime still owns the envelope; the handler only contributed opaque
        // bytes via its `Ctx`.
        envelope.signed_extensions.extend(metadata.signed);
        envelope.local_extensions.extend(metadata.local);

        sink.record_receipt(&envelope).map(Some).map_err(|error| {
            let message = error.to_string();
            if let Some(cause) = handler_cause {
                RuntimeError::receipt_sink_after_handler_failure(descriptor.name(), message, cause)
            } else {
                RuntimeError::receipt_sink(descriptor.name(), message)
            }
        })
    }
}

struct HandlerPhase<'a> {
    name: &'a str,
    descriptor: &'a operation::OperationDescriptor,
    input: &'a [u8],
    handler_result: HandlerResult,
    observed_effects: &'a OperationEffectRow,
    metadata: ReceiptMetadata,
    status_sink: &'a Option<BoxedStatusSink>,
    receipt_hash_policy: &'a ReceiptHashPolicy,
}

struct RuntimeStatusRecord<'a> {
    status_sink: Option<&'a (dyn OperationStatusSink + Send + Sync)>,
    receipt_hash_policy: &'a ReceiptHashPolicy,
    descriptor: &'a operation::OperationDescriptor,
    lifecycle: OperationStatusLifecycle,
    input: &'a [u8],
    output: Option<&'a [u8]>,
    code: Option<String>,
    message: Option<String>,
    handler_cause: Option<ReceiptSinkHandlerCause>,
}

fn record_runtime_status(record: RuntimeStatusRecord<'_>) -> Result<(), RuntimeError> {
    let RuntimeStatusRecord {
        status_sink,
        receipt_hash_policy,
        descriptor,
        lifecycle,
        input,
        output,
        code,
        message,
        handler_cause,
    } = record;
    let Some(sink) = status_sink else {
        return Ok(());
    };

    let fact = if lifecycle == OperationStatusLifecycle::Started {
        OperationStatusFactV1::started(descriptor.name(), descriptor.receipt_kind())
    } else {
        let input_hash = receipt_hash_policy.hash(input);
        let output_hash = output.and_then(|bytes| receipt_hash_policy.hash(bytes));
        OperationStatusFactV1::terminal(
            descriptor.name(),
            lifecycle,
            descriptor.receipt_kind(),
            code,
            message,
            input_hash,
            output_hash,
        )
    };

    sink.record_fact(&fact).map_err(|error| {
        let message = error.to_string();
        if let Some(cause) = handler_cause {
            RuntimeError::status_sink_after_handler_failure(descriptor.name(), message, cause)
        } else {
            RuntimeError::status_sink(descriptor.name(), message)
        }
    })
}

/// Unresolved checkout request passed to a runtime.
pub struct CheckoutFrame {
    name: String,
    input: operation::OperationInput,
}

impl CheckoutFrame {
    /// Build an unresolved checkout frame.
    #[must_use]
    pub fn new(name: impl Into<String>, input: operation::OperationInput) -> Self {
        Self {
            name: name.into(),
            input,
        }
    }

    /// Requested operation name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Input bytes for the requested checkout.
    #[must_use]
    pub fn input(&self) -> &[u8] {
        &self.input
    }

    /// Consume the frame and return its parts.
    #[must_use]
    pub fn into_parts(self) -> (String, operation::OperationInput) {
        (self.name, self.input)
    }
}

/// Checkout request resolved to an operation descriptor.
pub struct Checkout {
    descriptor: operation::OperationDescriptor,
    input: operation::OperationInput,
}

impl Checkout {
    /// Build a resolved checkout request.
    #[must_use]
    pub fn new(
        descriptor: operation::OperationDescriptor,
        input: operation::OperationInput,
    ) -> Self {
        Self { descriptor, input }
    }

    fn from_frame(descriptor: operation::OperationDescriptor, frame: CheckoutFrame) -> Self {
        Self {
            descriptor,
            input: frame.input,
        }
    }

    /// Descriptor that will be used for dispatch.
    #[must_use]
    pub fn descriptor(&self) -> &operation::OperationDescriptor {
        &self.descriptor
    }

    /// Input bytes for this checkout.
    #[must_use]
    pub fn input(&self) -> &[u8] {
        &self.input
    }

    /// Consume the checkout and return its parts.
    #[must_use]
    pub fn into_parts(self) -> (operation::OperationDescriptor, operation::OperationInput) {
        (self.descriptor, self.input)
    }
}

/// Minimal borrowed invocation context passed to handlers (and the admission
/// guard).
///
/// Beyond the descriptor, a handler or guard may attach opaque receipt metadata
/// to the current invocation; the runtime drains it into the recorded receipt.
/// The handler never owns the receipt envelope — it only contributes bytes.
pub struct Ctx<'a> {
    descriptor: &'a operation::OperationDescriptor,
    metadata: ReceiptMetadata,
    observed_effects: OperationEffectRow,
    effect_backend: Option<&'a mut (dyn EffectBackend + 'static)>,
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(
        descriptor: &'a operation::OperationDescriptor,
        effect_backend: Option<&'a mut (dyn EffectBackend + 'static)>,
    ) -> Self {
        Self {
            descriptor,
            metadata: ReceiptMetadata::default(),
            observed_effects: OperationEffectRow::empty(),
            effect_backend,
        }
    }

    /// Descriptor for the operation currently being invoked.
    #[must_use]
    pub fn descriptor(&self) -> &'a operation::OperationDescriptor {
        self.descriptor
    }

    /// Attach one entry to the SIGNED receipt drawer of this invocation. The
    /// store sink copies signed entries into batpak receipt extensions, so this
    /// is where correlation/attempt identity belongs.
    pub fn attach_signed_extension(&mut self, key: impl Into<String>, value: impl Into<Vec<u8>>) {
        self.metadata.signed.insert(key.into(), value.into());
    }

    /// Attach one entry to the LOCAL receipt drawer of this invocation. Local
    /// entries stay in the receipt envelope body and are not promoted to batpak
    /// receipt extensions.
    pub fn attach_local_extension(&mut self, key: impl Into<String>, value: impl Into<Vec<u8>>) {
        self.metadata.local.insert(key.into(), value.into());
    }

    /// Borrow an event-read capability handle for this invocation.
    pub fn event_read_handle(&mut self) -> EventReadHandle<'_> {
        EventReadHandle::new(
            &mut self.observed_effects,
            self.effect_backend.as_deref_mut(),
        )
    }

    /// Borrow an event-append capability handle for this invocation. This is the
    /// only path to the runtime's event log; the handle performs the append and
    /// records it so the observed row is authoritative.
    pub fn event_append_handle(&mut self) -> EventAppendHandle<'_> {
        EventAppendHandle::new(
            &mut self.observed_effects,
            self.effect_backend.as_deref_mut(),
        )
    }

    /// Borrow a projection-read capability handle for this invocation.
    pub fn projection_read_handle(&mut self) -> ProjectionReadHandle<'_> {
        ProjectionReadHandle::new(
            &mut self.observed_effects,
            self.effect_backend.as_deref_mut(),
        )
    }

    /// Borrow a receipt-emission capability handle for this invocation.
    pub fn receipt_emit_handle(&mut self) -> ReceiptEmitHandle<'_> {
        ReceiptEmitHandle::new(
            &mut self.observed_effects,
            self.effect_backend.as_deref_mut(),
        )
    }

    /// Borrow a host-control capability handle for this invocation.
    pub fn host_control_handle(&mut self) -> HostControlHandle<'_> {
        HostControlHandle::new(
            &mut self.observed_effects,
            self.effect_backend.as_deref_mut(),
        )
    }

    /// Effects observed so far through this invocation context.
    #[must_use]
    pub fn observed_effects(&self) -> &OperationEffectRow {
        &self.observed_effects
    }

    pub(crate) fn into_metadata(self) -> ReceiptMetadata {
        self.metadata
    }
}

/// Result returned by a successful checkout.
pub struct CheckoutResult {
    descriptor: operation::OperationDescriptor,
    output: operation::OperationOutput,
    recorded_receipt: Option<RecordedReceipt>,
}

impl CheckoutResult {
    /// Descriptor that was used for dispatch.
    #[must_use]
    pub fn descriptor(&self) -> &operation::OperationDescriptor {
        &self.descriptor
    }

    /// Handler output.
    #[must_use]
    pub fn output(&self) -> &operation::OperationOutput {
        &self.output
    }

    /// Receipt recorded by the runtime for this invocation, when configured.
    #[must_use]
    pub fn recorded_receipt(&self) -> Option<&RecordedReceipt> {
        self.recorded_receipt.as_ref()
    }

    /// Consume the result and return the handler output.
    #[must_use]
    pub fn into_output(self) -> operation::OperationOutput {
        self.output
    }
}

/// Factory for opening independent [`Core`] instances.
///
/// Concurrent netbat TCP listeners call this once per accepted connection
/// because [`Core`] dispatch is `&mut` and handlers are not required to be
/// `Send`. Implementors must be [`Send`] so the accept loop can share the
/// factory across worker threads behind a mutex.
pub trait CoreFactory: Send {
    /// Open a fresh core for one connection or invocation scope.
    fn open_core(&mut self) -> Core;
}

impl<F> CoreFactory for F
where
    F: FnMut() -> Core + Send,
{
    fn open_core(&mut self) -> Core {
        self()
    }
}

#[cfg(test)]
mod checkout_tests {
    use super::Checkout;
    use crate::operation::{EffectClass, OperationDescriptor};

    #[test]
    fn input_exposes_the_checkout_bytes() {
        // Pins `Checkout::input`: a stubbed body (e.g. `Vec::leak(vec![1])`)
        // would hand handlers fabricated bytes instead of the real payload.
        let descriptor = OperationDescriptor::new(
            "echo",
            EffectClass::Compute,
            "schema.echo.input.v1",
            "schema.echo.output.v1",
            "receipt.echo.v1",
        );
        let payload = b"real-payload".to_vec();
        let checkout = Checkout::new(descriptor, payload.clone());
        assert_eq!(checkout.input(), payload.as_slice());
    }
}
