//! Synchronous runtime composition root.

use std::collections::BTreeMap;

use crate::error::{ReceiptSinkHandlerCause, RuntimeError};
use crate::receipt::{ReceiptHashPolicy, ReceiptOutcome, RecordedReceipt};
use crate::{handler, operation, receipt};

type BoxedHandler = Box<dyn handler::Handler + 'static>;
type BoxedReceiptSink = Box<dyn receipt::ReceiptSink + 'static>;

/// Composition root for a sync-first operation runtime.
///
/// `Core` owns the operation descriptors, the matching handler table, and the
/// optional receipt sink shared with each invocation context. It performs only
/// synchronous dispatch; handlers run on the caller's thread.
pub struct Core {
    pub(crate) descriptors: BTreeMap<String, operation::OperationDescriptor>,
    pub(crate) handlers: BTreeMap<String, BoxedHandler>,
    pub(crate) receipt_sink: Option<BoxedReceiptSink>,
    pub(crate) receipt_hash_policy: ReceiptHashPolicy,
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
        let name = checkout.descriptor.name();
        let descriptor = self.descriptors.get(name).cloned().ok_or_else(|| {
            tracing::warn!(operation = %name, outcome = "unknown_operation", "checkout rejected");
            RuntimeError::unknown_operation(name)
        })?;
        let handler = self.handlers.get_mut(name).ok_or_else(|| {
            tracing::error!(operation = %name, outcome = "missing_handler", "checkout rejected");
            RuntimeError::missing_handler(name)
        })?;
        let input = checkout.input;
        let handler_result = {
            let mut ctx = Ctx::new(&descriptor);
            handler.handle(&input, &mut ctx)
        };

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
                self.record_runtime_receipt(
                    &descriptor,
                    &input,
                    None,
                    outcome,
                    Some(cause.clone()),
                )?;
                return Err(RuntimeError::handler(name, cause.code(), cause.message()));
            }
        };
        let recorded_receipt = self.record_runtime_receipt(
            &descriptor,
            &input,
            Some(output.as_slice()),
            ReceiptOutcome::Completed,
            None,
        )?;
        let span = tracing::Span::current();
        span.record("output_bytes", output.len());
        span.record("outcome", "completed");

        Ok(CheckoutResult {
            descriptor,
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

/// Minimal borrowed invocation context passed to handlers.
pub struct Ctx<'a> {
    descriptor: &'a operation::OperationDescriptor,
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(descriptor: &'a operation::OperationDescriptor) -> Self {
        Self { descriptor }
    }

    /// Descriptor for the operation currently being invoked.
    #[must_use]
    pub fn descriptor(&self) -> &'a operation::OperationDescriptor {
        self.descriptor
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
