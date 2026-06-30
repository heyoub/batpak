//! Builder for the synchronous runtime composition root.

use std::collections::{BTreeMap, BTreeSet};
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
    /// When true, the caller explicitly accepted a sinkless build: no receipt
    /// sink is required and the built core records no receipts. Without this
    /// opt-out, [`Self::build`] fails closed rather than silently dropping
    /// receipts.
    receipts_opted_out: bool,
    status_sink: Option<BoxedStatusSink>,
    receipt_hash_policy: ReceiptHashPolicy,
    effect_backend: Option<BoxedEffectBackend>,
    /// Capability tokens this Core is granted. At checkout the runtime fails
    /// closed when a dispatched operation declares a required capability token
    /// that is not in this set (effect-axis tokens are ambient and excluded).
    granted_capabilities: BTreeSet<String>,
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

    /// Explicitly build a core that records no receipts.
    ///
    /// [`Self::build`] fails closed with [`BuildError::MissingReceiptSink`] when
    /// no receipt sink is configured, because a sinkless core silently drops
    /// every runtime receipt. Call this to state on purpose that this core
    /// persists no receipts. A receipt sink configured with
    /// [`Self::receipt_sink`] takes precedence and still persists receipts.
    pub fn without_receipts(&mut self) -> &mut Self {
        self.receipts_opted_out = true;
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

    /// Grant one runtime capability token to the built [`Core`].
    ///
    /// At checkout the runtime fails closed when a dispatched operation declares
    /// a required capability token it was not granted. Tokens are declared with
    /// `OperationEffectRow::requires_capability` or the `#[operation]` macro's
    /// `requires_capabilities`. Effect-axis tokens auto-declared by the effect
    /// builders (event read/append, projection query, receipt emit, host
    /// control) are ambient — they are mediated by the observed-effect subset
    /// check and never need an explicit grant. A Core granted nothing therefore
    /// runs only operations that declare no extra capability tokens.
    pub fn grant_capability(&mut self, capability: impl Into<String>) -> &mut Self {
        self.granted_capabilities.insert(capability.into());
        self
    }

    /// Grant several runtime capability tokens to the built [`Core`].
    ///
    /// Convenience over repeated [`Self::grant_capability`] calls; see it for
    /// the gate semantics.
    pub fn grant_capabilities<I, S>(&mut self, capabilities: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.granted_capabilities
            .extend(capabilities.into_iter().map(Into::into));
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
    /// operation table and handler table do not line up by name, or
    /// [`BuildError::MissingReceiptSink`] when no receipt sink is configured and
    /// the caller did not opt out with [`Self::without_receipts`] (a sinkless
    /// core silently drops every runtime receipt, so the build fails closed).
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
        if self.receipt_sink.is_none() && !self.receipts_opted_out {
            return Err(BuildError::MissingReceiptSink);
        }

        Ok(Core {
            descriptors: self.descriptors,
            handlers: self.handlers,
            admission_guard: self.admission_guard,
            receipt_sink: self.receipt_sink,
            status_sink: self.status_sink,
            receipt_hash_policy: self.receipt_hash_policy,
            effect_backend: self.effect_backend,
            granted_capabilities: self.granted_capabilities,
        })
    }
}

#[cfg(test)]
mod builder_mutation_tests {
    //! Pins the boxed-registration and sink/guard/backend setters: each setter
    //! mutates builder state that `build()` lifts into `Core`'s `pub(crate)`
    //! fields, so the leaked-`Default` setter mutants (which drop the mutation)
    //! and the `register_boxed -> Ok(Default)` mutant are observable here.

    use std::sync::Arc;

    use crate::admission::{AdmissionDecision, AdmissionGuard};
    use crate::core::Ctx;
    use crate::effect_backend::{EffectBackend, EffectError};
    use crate::handler::{Handler, HandlerResult};
    use crate::operation::{EffectClass, OperationDescriptor};
    use crate::operation_status::OperationStatusFactV1;
    use crate::operation_status_sink::{OperationStatusSink, OperationStatusSinkError};
    use crate::receipt::{ReceiptEnvelope, ReceiptSink, ReceiptSinkError, RecordedReceipt};

    use super::CoreBuilder;

    const PING: OperationDescriptor = OperationDescriptor::new(
        "ping",
        EffectClass::Inspect,
        "schema.ping.input.v1",
        "schema.ping.output.v1",
        "receipt.ping.v1",
    );

    struct Echo;

    impl Handler for Echo {
        fn handle(&mut self, input: &[u8], _cx: &mut Ctx<'_>) -> HandlerResult {
            Ok(input.to_vec())
        }
    }

    fn boxed_handler() -> Box<dyn Handler + 'static> {
        Box::new(Echo)
    }

    struct NoopReceiptSink;

    impl ReceiptSink for NoopReceiptSink {
        fn record_receipt(
            &self,
            envelope: &ReceiptEnvelope,
        ) -> Result<RecordedReceipt, ReceiptSinkError> {
            Ok(RecordedReceipt::new(envelope.clone()))
        }
    }

    struct NoopStatusSink;

    impl OperationStatusSink for NoopStatusSink {
        fn record_fact(
            &self,
            _fact: &OperationStatusFactV1,
        ) -> Result<(), OperationStatusSinkError> {
            Ok(())
        }
    }

    struct NoopBackend;

    impl EffectBackend for NoopBackend {
        fn append_event(
            &mut self,
            _kind: batpak::event::EventKind,
            _payload: &[u8],
        ) -> Result<(), EffectError> {
            Ok(())
        }
    }

    struct DenyGuard;

    impl AdmissionGuard for DenyGuard {
        fn admit(
            &self,
            _descriptor: &OperationDescriptor,
            _input: &[u8],
            _cx: &mut Ctx<'_>,
        ) -> AdmissionDecision {
            AdmissionDecision::deny("denied", "blocked in test")
        }
    }

    #[test]
    fn register_boxed_inserts_descriptor_and_handler_and_detects_duplicates() {
        let mut builder = CoreBuilder::new();
        builder
            .register_boxed(PING, boxed_handler())
            .expect("first register_boxed should succeed");
        // The second registration only errors if the first insert actually took
        // hold; the `Ok(Default)` mutant never inserts, so it cannot detect the
        // duplicate and returns Ok.
        let duplicate = builder.register_boxed(PING, boxed_handler());
        assert!(
            duplicate.is_err(),
            "registering the same name twice must be a duplicate error"
        );
        builder.without_receipts();
        let core = builder.build().expect("build should succeed");
        assert!(
            core.contains_operation("ping"),
            "register_boxed must leave the descriptor in the built Core"
        );
        // Descriptor presence alone is insufficient: a regression that kept the
        // descriptor but dropped the boxed handler would still pass the check
        // above while breaking dispatch. Assert the handler map retained it too.
        assert!(
            core.handlers.contains_key("ping"),
            "register_boxed must also retain the boxed handler in the built Core"
        );
    }

    #[test]
    fn admission_guard_is_set_then_cleared() {
        let mut with_guard = CoreBuilder::new();
        with_guard
            .register_boxed(PING, boxed_handler())
            .expect("register");
        with_guard.admission_guard(DenyGuard);
        with_guard.without_receipts();
        let core = with_guard.build().expect("build");
        assert!(
            core.admission_guard.is_some(),
            "admission_guard setter must bind a guard"
        );

        let mut cleared = CoreBuilder::new();
        cleared
            .register_boxed(PING, boxed_handler())
            .expect("register");
        cleared.admission_guard(DenyGuard);
        cleared.clear_admission_guard();
        cleared.without_receipts();
        let core = cleared.build().expect("build");
        assert!(
            core.admission_guard.is_none(),
            "clear_admission_guard must drop the bound guard"
        );
    }

    #[test]
    fn receipt_sink_boxed_binds_the_sink() {
        let mut builder = CoreBuilder::new();
        builder
            .register_boxed(PING, boxed_handler())
            .expect("register");
        builder.receipt_sink_boxed(Box::new(NoopReceiptSink));
        let core = builder.build().expect("build");
        assert!(
            core.receipt_sink.is_some(),
            "receipt_sink_boxed must bind the sink"
        );
    }

    #[test]
    fn status_sink_boxed_is_set_then_cleared() {
        let mut builder = CoreBuilder::new();
        builder
            .register_boxed(PING, boxed_handler())
            .expect("register");
        builder.status_sink_boxed(Arc::new(NoopStatusSink));
        builder.without_receipts();
        let core = builder.build().expect("build");
        assert!(
            core.status_sink.is_some(),
            "status_sink_boxed must bind the sink"
        );

        let mut cleared = CoreBuilder::new();
        cleared
            .register_boxed(PING, boxed_handler())
            .expect("register");
        cleared.status_sink_boxed(Arc::new(NoopStatusSink));
        cleared.clear_status_sink();
        cleared.without_receipts();
        let core = cleared.build().expect("build");
        assert!(
            core.status_sink.is_none(),
            "clear_status_sink must drop the bound sink"
        );
    }

    #[test]
    fn effect_backend_boxed_binds_the_backend() {
        let mut builder = CoreBuilder::new();
        builder
            .register_boxed(PING, boxed_handler())
            .expect("register");
        builder.effect_backend_boxed(Box::new(NoopBackend));
        builder.without_receipts();
        let core = builder.build().expect("build");
        assert!(
            core.effect_backend.is_some(),
            "effect_backend_boxed must bind the backend"
        );
    }
}
