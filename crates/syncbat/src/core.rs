//! Synchronous runtime composition root.

use std::collections::BTreeMap;

use crate::error::RuntimeError;
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
    /// resolve the handler, borrow a [`Cx`], and run the handler synchronously.
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
    ) -> Result<InvokeResult, RuntimeError> {
        let name = name.as_ref();
        let descriptor = *self
            .descriptors
            .get(name)
            .ok_or_else(|| RuntimeError::unknown_operation(name))?;
        let handler = self
            .handlers
            .get_mut(name)
            .ok_or_else(|| RuntimeError::missing_handler(name))?;
        let handler_result = {
            let mut cx = Cx::new(&descriptor);
            handler.handle(&input, &mut cx)
        };

        let output = match handler_result {
            Ok(output) => output,
            Err(error) => {
                let outcome = ReceiptOutcome::failed(error.class(), error.message());
                self.record_runtime_receipt(&descriptor, &input, None, outcome)?;
                return Err(RuntimeError::handler(name, error.class(), error.message()));
            }
        };
        let recorded_receipt = self.record_runtime_receipt(
            &descriptor,
            &input,
            Some(output.as_slice()),
            ReceiptOutcome::Completed,
        )?;

        Ok(InvokeResult {
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

        sink.record_receipt(&envelope)
            .map(Some)
            .map_err(|error| RuntimeError::receipt_sink(descriptor.name(), error.to_string()))
    }
}

/// Minimal borrowed invocation context passed to handlers.
pub struct Cx<'a> {
    descriptor: &'a operation::OperationDescriptor,
}

impl<'a> Cx<'a> {
    pub(crate) fn new(descriptor: &'a operation::OperationDescriptor) -> Self {
        Self { descriptor }
    }

    /// Descriptor for the operation currently being invoked.
    #[must_use]
    pub fn descriptor(&self) -> &'a operation::OperationDescriptor {
        self.descriptor
    }
}

/// Result returned by a successful invocation.
pub struct InvokeResult {
    descriptor: operation::OperationDescriptor,
    output: operation::OperationOutput,
    recorded_receipt: Option<RecordedReceipt>,
}

impl InvokeResult {
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
