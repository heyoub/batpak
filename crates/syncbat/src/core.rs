//! Synchronous runtime composition root.

use std::collections::BTreeMap;

use crate::error::RuntimeError;
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
    /// present, or a handler-provided runtime error from the invoked handler.
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
        let mut cx = Cx::new(&descriptor, self.receipt_sink.as_deref());
        let output = handler
            .handle(&input, &mut cx)
            .map_err(|error| RuntimeError::handler(name, error.class(), error.message()))?;

        Ok(InvokeResult { descriptor, output })
    }
}

/// Minimal borrowed invocation context passed to handlers.
pub struct Cx<'a> {
    descriptor: &'a operation::OperationDescriptor,
    receipt_sink: Option<&'a (dyn receipt::ReceiptSink + 'static)>,
}

impl<'a> Cx<'a> {
    pub(crate) fn new(
        descriptor: &'a operation::OperationDescriptor,
        receipt_sink: Option<&'a (dyn receipt::ReceiptSink + 'static)>,
    ) -> Self {
        Self {
            descriptor,
            receipt_sink,
        }
    }

    /// Descriptor for the operation currently being invoked.
    #[must_use]
    pub fn descriptor(&self) -> &'a operation::OperationDescriptor {
        self.descriptor
    }

    /// Optional receipt sink configured on the runtime.
    #[must_use]
    pub fn receipt_sink(&self) -> Option<&'a (dyn receipt::ReceiptSink + 'static)> {
        self.receipt_sink
    }
}

/// Result returned by a successful invocation.
pub struct InvokeResult {
    descriptor: operation::OperationDescriptor,
    output: operation::OperationOutput,
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

    /// Consume the result and return the handler output.
    #[must_use]
    pub fn into_output(self) -> operation::OperationOutput {
        self.output
    }
}
