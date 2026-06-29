//! Builder for the synchronous runtime composition root.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::admission::AdmissionGuard;
use crate::core::Core;
use crate::effect_backend::EffectBackend;
use crate::error::BuildError;
use crate::operation_status_sink::OperationStatusSink;
use crate::receipt::ReceiptHashPolicy;
use crate::{handler, module, operation, receipt, register};

type BoxedHandler = Box<dyn handler::Handler + 'static>;
type BoxedReceiptSink = Box<dyn receipt::ReceiptSink + 'static>;
type BoxedStatusSink = Arc<dyn OperationStatusSink + Send + Sync>;
type BoxedAdmissionGuard = Box<dyn AdmissionGuard + 'static>;
type BoxedEffectBackend = Box<dyn EffectBackend + 'static>;

/// Builder for [`Core`].
///
/// The builder separates operation descriptors from handlers so modules can
/// mount descriptors first and callers can register concrete behavior later.
/// [`CoreBuilder::build`] validates that both sides are present before it
/// returns a runnable runtime.
#[derive(Default)]
pub struct CoreBuilder {
    descriptors: BTreeMap<String, operation::OperationDescriptor>,
    handlers: BTreeMap<String, BoxedHandler>,
    admission_guard: Option<BoxedAdmissionGuard>,
    receipt_sink: Option<BoxedReceiptSink>,
    status_sink: Option<BoxedStatusSink>,
    receipt_hash_policy: ReceiptHashPolicy,
    effect_backend: Option<BoxedEffectBackend>,
}

impl CoreBuilder {
    /// Create an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mount a data-oriented module descriptor into this builder.
    ///
    /// # Errors
    /// Returns a build error when the module is invalid or when any descriptor
    /// duplicates an operation already registered in this builder.
    pub fn mount(&mut self, module: module::Module) -> Result<&mut Self, BuildError> {
        let module_name = module.name().to_owned();
        let register = module
            .into_register()
            .map_err(|error| BuildError::invalid_module(&module_name, error.to_string()))?;
        for (_, descriptor) in register.into_map() {
            self.register_operation(descriptor)?;
        }
        Ok(self)
    }

    /// Register an operation descriptor without a handler.
    ///
    /// # Errors
    /// Returns [`BuildError::DuplicateOperation`] when the descriptor name is
    /// already present.
    pub fn register_operation(
        &mut self,
        descriptor: operation::OperationDescriptor,
    ) -> Result<&mut Self, BuildError> {
        let name = descriptor.name().to_owned();
        descriptor
            .validate()
            .map_err(|error| BuildError::invalid_operation(&name, error.to_string()))?;
        if self.descriptors.contains_key(&name) {
            return Err(BuildError::duplicate_operation(name));
        }

        self.descriptors.insert(name, descriptor);
        Ok(self)
    }

    /// Register a handler for an operation name.
    ///
    /// # Errors
    /// Returns [`BuildError::DuplicateHandler`] when a handler for `name` is
    /// already present.
    pub fn register_handler<H>(
        &mut self,
        name: impl Into<String>,
        handler: H,
    ) -> Result<&mut Self, BuildError>
    where
        H: handler::Handler + 'static,
    {
        let name = name.into();
        register::validate_module_name(&name)
            .map_err(|error| BuildError::invalid_handler(&name, error.to_string()))?;
        if self.handlers.contains_key(&name) {
            return Err(BuildError::duplicate_handler(name));
        }

        self.handlers.insert(name, Box::new(handler));
        Ok(self)
    }

    /// Register a descriptor and handler together.
    ///
    /// # Errors
    /// Returns a duplicate descriptor or duplicate handler error if either side
    /// is already present.
    pub fn register<H>(
        &mut self,
        descriptor: operation::OperationDescriptor,
        handler: H,
    ) -> Result<&mut Self, BuildError>
    where
        H: handler::Handler + 'static,
    {
        let name = descriptor.name().to_owned();
        descriptor
            .validate()
            .map_err(|error| BuildError::invalid_operation(&name, error.to_string()))?;
        if self.descriptors.contains_key(&name) {
            return Err(BuildError::duplicate_operation(name));
        }
        if self.handlers.contains_key(&name) {
            return Err(BuildError::duplicate_handler(name));
        }

        self.descriptors.insert(name.clone(), descriptor);
        self.handlers.insert(name, Box::new(handler));
        Ok(self)
    }

    /// Register a macro-generated operation item.
    ///
    /// # Errors
    /// Returns the same duplicate or validation errors as [`Self::register`].
    pub fn register_item(
        &mut self,
        item: operation::OperationRegisterItem,
    ) -> Result<&mut Self, BuildError> {
        let (descriptor, handler) = item.into_parts();
        self.register(descriptor, handler)
    }

    /// Register a descriptor and an already-boxed handler together.
    ///
    /// A composition layer above this builder (such as a module host) holds its
    /// handlers as trait objects; this admits them without re-boxing. Validation
    /// and duplicate detection match [`Self::register`].
    ///
    /// # Errors
    /// Returns a duplicate descriptor or duplicate handler error if either side
    /// is already present, or an invalid-operation error if the descriptor fails
    /// validation.
    pub fn register_boxed(
        &mut self,
        descriptor: operation::OperationDescriptor,
        handler: BoxedHandler,
    ) -> Result<&mut Self, BuildError> {
        let name = descriptor.name().to_owned();
        descriptor
            .validate()
            .map_err(|error| BuildError::invalid_operation(&name, error.to_string()))?;
        if self.descriptors.contains_key(&name) {
            return Err(BuildError::duplicate_operation(name));
        }
        if self.handlers.contains_key(&name) {
            return Err(BuildError::duplicate_handler(name));
        }

        self.descriptors.insert(name.clone(), descriptor);
        self.handlers.insert(name, handler);
        Ok(self)
    }

    /// Configure the optional pre-handler admission guard.
    ///
    /// The guard runs before every handler dispatch and may deny the call (the
    /// handler never runs, and the runtime records a `Denied` receipt). At most
    /// one guard is held; compose multiple policies inside a single guard.
    pub fn admission_guard<G>(&mut self, guard: G) -> &mut Self
    where
        G: AdmissionGuard + 'static,
    {
        self.admission_guard = Some(Box::new(guard));
        self
    }

    /// Clear any configured admission guard.
    pub fn clear_admission_guard(&mut self) -> &mut Self {
        self.admission_guard = None;
        self
    }

    /// Configure the optional receipt sink made available to invocation
    /// contexts.
    pub fn receipt_sink<S>(&mut self, sink: S) -> &mut Self
    where
        S: receipt::ReceiptSink + 'static,
    {
        self.receipt_sink = Some(Box::new(sink));
        self
    }

    /// Configure the optional receipt sink from an already-boxed trait object.
    ///
    /// Equivalent to [`Self::receipt_sink`] for a composition layer that holds
    /// its sink as a trait object.
    pub fn receipt_sink_boxed(&mut self, sink: BoxedReceiptSink) -> &mut Self {
        self.receipt_sink = Some(sink);
        self
    }

    /// Clear any configured receipt sink.
    pub fn clear_receipt_sink(&mut self) -> &mut Self {
        self.receipt_sink = None;
        self
    }

    /// Configure the optional operation-status sink used during checkout.
    pub fn status_sink<S>(&mut self, sink: S) -> &mut Self
    where
        S: OperationStatusSink + Send + Sync + 'static,
    {
        self.status_sink = Some(Arc::new(sink));
        self
    }

    /// Configure the operation-status sink from an already-boxed trait object.
    pub fn status_sink_boxed(&mut self, sink: BoxedStatusSink) -> &mut Self {
        self.status_sink = Some(sink);
        self
    }

    /// Clear any configured operation-status sink.
    pub fn clear_status_sink(&mut self) -> &mut Self {
        self.status_sink = None;
        self
    }

    /// Configure runtime receipt hash population.
    pub fn receipt_hash_policy(&mut self, policy: ReceiptHashPolicy) -> &mut Self {
        self.receipt_hash_policy = policy;
        self
    }

    /// Configure the runtime-owned effect backend.
    ///
    /// Operations append events only through `Ctx`, which performs the append
    /// through this backend. Without a backend bound, an `append_event` call from
    /// a handler fails closed rather than reaching a store the runtime did not
    /// mediate.
    pub fn effect_backend<B>(&mut self, backend: B) -> &mut Self
    where
        B: EffectBackend + 'static,
    {
        self.effect_backend = Some(Box::new(backend));
        self
    }

    /// Configure the effect backend from an already-boxed trait object.
    ///
    /// Equivalent to [`Self::effect_backend`] for a composition layer that holds
    /// its backend as a trait object.
    pub fn effect_backend_boxed(&mut self, backend: BoxedEffectBackend) -> &mut Self {
        self.effect_backend = Some(backend);
        self
    }

    /// Build a runnable [`Core`].
    ///
    /// # Errors
    /// Returns a missing-descriptor or missing-handler error when the mounted
    /// operation table and handler table do not line up by name.
    pub fn build(self) -> Result<Core, BuildError> {
        for name in self.descriptors.keys() {
            if !self.handlers.contains_key(name) {
                return Err(BuildError::missing_handler(name.clone()));
            }
        }
        for name in self.handlers.keys() {
            if !self.descriptors.contains_key(name) {
                return Err(BuildError::missing_descriptor(name.clone()));
            }
        }

        Ok(Core {
            descriptors: self.descriptors,
            handlers: self.handlers,
            admission_guard: self.admission_guard,
            receipt_sink: self.receipt_sink,
            status_sink: self.status_sink,
            receipt_hash_policy: self.receipt_hash_policy,
            effect_backend: self.effect_backend,
        })
    }
}
