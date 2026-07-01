//! The host builder: mount content-identified modules, validate them against
//! each other, then lower the whole set into one `syncbat` runtime.
//!
//! `mount` performs the cross-module validation a single module cannot do alone
//! (id / operation / receipt-namespace / job-kind collisions) and re-verifies
//! each module's manifest hash against its declared parts (tamper detection).
//! `build` then **lowers** every module into a single [`syncbat::CoreBuilder`] —
//! it does not re-wrap it — composes the per-module guards into one routing
//! guard, attaches the receipt sink, and computes the host-composition
//! fingerprint.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use batpak::store::{Spawn, ThreadSpawn};
use syncbat::{
    AdmissionDecision, AdmissionGuard, CoreBuilder, Ctx, EffectBackend, Handler, HandlerError,
    HandlerResult, OperationDescriptor, OperationEffectRow, ReceiptSink,
};

use crate::composition::CompositionSchemaBuilder;
use crate::descriptor::HookPhase;
use crate::error::HostError;
use crate::event_payload_binding::EventPayloadBinding;
use crate::host::{Host, HostHook, HostParts};
use crate::host_control_backend::{HostControlEffectBackend, HostController};
use crate::identity::{canonical_digest, HostFingerprint};
use crate::interface::compute_interface_fingerprint;
use crate::module::{BoxedJob, HostModule, HostModuleParts};
use crate::schema::{SchemaRegistry, SchemaRole};
use crate::subscription::SubscriptionDescriptor;
use crate::validating_effect_backend::ValidatingEffectBackend;

type BoxedGuard = Box<dyn AdmissionGuard + 'static>;
type BoxedHandler = Box<dyn Handler + 'static>;
type BoxedReceiptSink = Box<dyn ReceiptSink + 'static>;
type BoxedEffectBackend = Box<dyn EffectBackend + 'static>;
type BoxedHostController = Box<dyn HostController + 'static>;

/// Domain separator for the host-composition fingerprint.
const HOST_FINGERPRINT_DOMAIN: &str = "hostbat.host.v1";

/// Builder that mounts modules and lowers them into a runnable [`Host`].
pub struct HostBuilder {
    modules: Vec<HostModuleParts>,
    module_ids: BTreeSet<String>,
    operation_owners: BTreeMap<String, String>,
    operation_effect_rows: BTreeMap<String, OperationEffectRow>,
    receipt_namespaces: BTreeMap<String, String>,
    job_owners: BTreeMap<String, String>,
    subscription_owners: BTreeMap<String, String>,
    event_payload_bindings: BTreeMap<u16, (String, String)>,
    schemas: CompositionSchemaBuilder,
    spawn: Arc<dyn Spawn>,
    receipt_sink: Option<BoxedReceiptSink>,
    effect_backend: Option<BoxedEffectBackend>,
    host_control: Option<BoxedHostController>,
}

impl Default for HostBuilder {
    fn default() -> Self {
        Self {
            modules: Vec::new(),
            module_ids: BTreeSet::new(),
            operation_owners: BTreeMap::new(),
            operation_effect_rows: BTreeMap::new(),
            receipt_namespaces: BTreeMap::new(),
            job_owners: BTreeMap::new(),
            subscription_owners: BTreeMap::new(),
            event_payload_bindings: BTreeMap::new(),
            schemas: CompositionSchemaBuilder::default(),
            spawn: Arc::new(ThreadSpawn),
            receipt_sink: None,
            effect_backend: None,
            host_control: None,
        }
    }
}

impl HostBuilder {
    /// Create an empty host builder backed by the production [`ThreadSpawn`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Use a custom [`Spawn`] backend for the supervisor (e.g. a deterministic
    /// test scheduler). Replaces the default [`ThreadSpawn`].
    #[must_use]
    pub fn spawn_with(mut self, spawn: Arc<dyn Spawn>) -> Self {
        self.spawn = spawn;
        self
    }

    /// Attach the receipt sink the composed runtime records into.
    #[must_use]
    pub fn receipt_sink<S>(mut self, sink: S) -> Self
    where
        S: ReceiptSink + 'static,
    {
        self.receipt_sink = Some(Box::new(sink));
        self
    }

    /// Attach the runtime-owned effect backend operations append through.
    ///
    /// Operations append events only via `Ctx`, which performs the append
    /// through this backend; without it bound, an `append_event` call fails
    /// closed instead of reaching a store the runtime did not mediate.
    #[must_use]
    pub fn effect_backend<B>(mut self, backend: B) -> Self
    where
        B: EffectBackend + 'static,
    {
        self.effect_backend = Some(Box::new(backend));
        self
    }

    /// Attach the controller that performs a `Control` operation's declared host
    /// controls.
    ///
    /// `Control` operations reach host authority only via `Ctx`, which performs
    /// the identified control through this controller; without it bound, a
    /// `use_host_control` call fails closed instead of touching the host. The
    /// composed host-control backend layers OUTER over any [`effect_backend`],
    /// so the store axes still flow through that inner backend.
    ///
    /// [`effect_backend`]: Self::effect_backend
    #[must_use]
    pub fn host_control<C>(mut self, controller: C) -> Self
    where
        C: HostController + 'static,
    {
        self.host_control = Some(Box::new(controller));
        self
    }

    /// Mount a module, validating it against everything already mounted.
    ///
    /// # Errors
    /// A [`HostError`] collision variant if the module's id, any operation,
    /// receipt namespace, or job kind clashes with a mounted module; or
    /// [`HostError::ModuleHashMismatch`] if the module's manifest does not match
    /// its declared parts.
    pub fn mount(mut self, module: HostModule) -> Result<Self, HostError> {
        if !module.manifest().verify_hash()? {
            return Err(HostError::ModuleHashMismatch {
                module: module.manifest().id().to_owned(),
            });
        }

        let parts = module.into_parts();
        let id = parts.manifest.id().to_owned();

        if !self.module_ids.insert(id.clone()) {
            return Err(HostError::DuplicateModuleId { id });
        }
        for descriptor in parts.manifest.operations() {
            let name = descriptor.name().to_owned();
            if let Some(existing_row) = self.operation_effect_rows.get(&name) {
                if existing_row != descriptor.effect_row() {
                    return Err(HostError::EffectConflict {
                        operation: name,
                        module: id,
                    });
                }
                return Err(HostError::DuplicateOperation {
                    operation: name,
                    module: id,
                });
            }
            self.operation_owners.insert(name.clone(), id.clone());
            self.operation_effect_rows
                .insert(name, descriptor.effect_row().clone());
        }
        for namespace in parts.manifest.receipt_namespaces() {
            if self
                .receipt_namespaces
                .insert(namespace.to_owned(), id.clone())
                .is_some()
            {
                return Err(HostError::DuplicateReceiptNamespace {
                    namespace: namespace.to_owned(),
                    module: id,
                });
            }
        }
        for descriptor in parts.manifest.jobs() {
            if self
                .job_owners
                .insert(descriptor.kind.clone(), id.clone())
                .is_some()
            {
                return Err(HostError::DuplicateJobKind {
                    kind: descriptor.kind.clone(),
                    module: id,
                });
            }
        }
        for descriptor in parts.manifest.subscriptions() {
            let subscription_id = descriptor.id().as_str().to_owned();
            if self
                .subscription_owners
                .insert(subscription_id.clone(), id.clone())
                .is_some()
            {
                return Err(HostError::DuplicateSubscriptionId {
                    id: subscription_id,
                    module: id,
                });
            }
        }
        for binding in parts.manifest.event_payload_bindings() {
            let kind = binding.kind_raw();
            let schema_ref = binding.payload_schema_ref().to_owned();
            if let Some((first_module, first_schema_ref)) = self.event_payload_bindings.get(&kind) {
                if first_schema_ref == &schema_ref {
                    return Err(HostError::DuplicateEventPayloadBinding {
                        kind,
                        module: id.clone(),
                    });
                }
                return Err(HostError::EventPayloadBindingConflict {
                    kind,
                    first_module: first_module.clone(),
                    first_schema_ref: first_schema_ref.clone(),
                    second_module: id.clone(),
                    second_schema_ref: schema_ref,
                });
            }
            self.event_payload_bindings
                .insert(kind, (id.clone(), schema_ref));
        }
        // Aggregate this module's schemas into the composition, failing closed on
        // a cross-module identity collision with a differing canonical encoding.
        for descriptor in parts.manifest.schemas() {
            self.schemas.add(&id, descriptor)?;
        }

        self.modules.push(parts);
        Ok(self)
    }

    /// Lower the mounted modules into a runnable [`Host`].
    ///
    /// # Errors
    /// [`HostError::EmptyHost`] if nothing is mounted; [`HostError::ModuleCoherence`]
    /// if a declared operation has no handler; [`HostError::Build`] if the lowered
    /// `syncbat` runtime does not validate; or [`HostError::CanonicalEncoding`] if
    /// the fingerprint cannot be sealed.
    pub fn build(self) -> Result<Host, HostError> {
        if self.modules.is_empty() {
            return Err(HostError::EmptyHost);
        }

        let fingerprint = compute_fingerprint(&self.modules)?;
        let composition_schemas = self.schemas.seal()?;
        validate_subscription_payload_schemas(&self.modules, &composition_schemas)?;
        validate_event_payload_bindings(&self.modules, &composition_schemas)?;
        let interface_fingerprint =
            compute_interface_fingerprint(&self.modules, &composition_schemas)?;
        let schema_registry = SchemaRegistry::from_descriptors(
            composition_schemas
                .schemas()
                .map(|entry| entry.descriptor().clone()),
        );

        let lowered = lower_modules(self.modules, &schema_registry)?;
        let mut core_builder = lowered.core_builder;
        if !lowered.guard.is_empty() {
            core_builder.admission_guard(lowered.guard);
        }
        if let Some(sink) = self.receipt_sink {
            core_builder.receipt_sink_boxed(sink);
        } else {
            // The core builder fails closed without a receipt sink. A host that
            // was assembled without one explicitly records no receipts; opt out
            // here so the absence is a stated choice rather than a silent drop.
            core_builder.without_receipts();
        }
        // The store-effect backend, schema-validated at the append boundary. The
        // host-control layer wraps this OUTER so store axes still flow through it.
        let inner_backend: Option<BoxedEffectBackend> = self.effect_backend.map(|backend| {
            Box::new(ValidatingEffectBackend::new(
                backend,
                collect_event_payload_binding_map(&lowered.event_payload_bindings),
                schema_registry.clone(),
            )) as BoxedEffectBackend
        });
        if let Some(controller) = self.host_control {
            let host_control_backend = HostControlEffectBackend::new(inner_backend, controller);
            core_builder.effect_backend_boxed(Box::new(host_control_backend));
        } else if let Some(inner) = inner_backend {
            core_builder.effect_backend_boxed(inner);
        }

        let mut startup = lowered.startup;
        let mut shutdown = lowered.shutdown;
        let mut operation_descriptors = lowered.operation_descriptors;
        let mut subscriptions = lowered.subscriptions;
        let mut event_payload_bindings = lowered.event_payload_bindings;

        // Global deterministic hook order: (order, module-id, name). Module ids
        // are unique, so this is a total order with no cross-module ambiguity.
        startup.sort_by(|a, b| a.order_key().cmp(&b.order_key()));
        shutdown.sort_by(|a, b| a.order_key().cmp(&b.order_key()));
        operation_descriptors.sort_by(|a, b| a.name().cmp(b.name()));
        subscriptions.sort_by(|(a_id, a), (b_id, b)| {
            a_id.cmp(b_id)
                .then_with(|| a.id().as_str().cmp(b.id().as_str()))
        });
        event_payload_bindings.sort_by(|(a_id, a), (b_id, b)| {
            a_id.cmp(b_id).then_with(|| a.kind_raw().cmp(&b.kind_raw()))
        });

        let core = core_builder.build()?;
        Ok(Host::new(HostParts {
            core,
            supervisor: crate::supervisor::Supervisor::new(self.spawn),
            fingerprint,
            interface_fingerprint,
            operations: operation_descriptors,
            subscriptions,
            event_payload_bindings,
            composition_schemas,
            schema_registry,
            startup,
            shutdown,
            job_factories: lowered.job_factories,
        }))
    }
}

struct LoweredModules {
    core_builder: CoreBuilder,
    guard: CompositeGuard,
    startup: Vec<HostHook>,
    shutdown: Vec<HostHook>,
    job_factories: BTreeMap<String, BoxedJob>,
    operation_descriptors: Vec<OperationDescriptor>,
    subscriptions: Vec<(String, SubscriptionDescriptor)>,
    event_payload_bindings: Vec<(String, EventPayloadBinding)>,
}

fn lower_modules(
    modules: Vec<HostModuleParts>,
    schema_registry: &SchemaRegistry,
) -> Result<LoweredModules, HostError> {
    let mut core_builder = CoreBuilder::new();
    let mut guard = CompositeGuard::default();
    let mut startup: Vec<HostHook> = Vec::new();
    let mut shutdown: Vec<HostHook> = Vec::new();
    let mut job_factories: BTreeMap<String, BoxedJob> = BTreeMap::new();
    let mut operation_descriptors: Vec<OperationDescriptor> = Vec::new();
    let mut subscriptions: Vec<(String, SubscriptionDescriptor)> = Vec::new();
    let mut event_payload_bindings: Vec<(String, EventPayloadBinding)> = Vec::new();

    for parts in modules {
        let HostModuleParts {
            manifest,
            mut handlers,
            guard: module_guard,
            hooks,
            jobs,
        } = parts;
        let module_id = manifest.id().to_owned();

        let mut operation_names = Vec::new();
        for descriptor in manifest.operations() {
            let name = descriptor.name().to_owned();
            let handler = handlers.remove(&name).ok_or_else(|| {
                HostError::coherence(&module_id, format!("operation {name:?} has no handler"))
            })?;
            let handler =
                SchemaValidatingHandler::new(descriptor, schema_registry.clone(), handler);
            core_builder.register_boxed(descriptor.clone(), Box::new(handler))?;
            operation_descriptors.push(descriptor.clone());
            operation_names.push(name);
        }
        if let Some(module_guard) = module_guard {
            guard.add(operation_names, module_guard);
        }
        for (descriptor, hook) in hooks {
            let entry = HostHook::new(module_id.clone(), descriptor, hook);
            match entry.phase() {
                HookPhase::Startup => startup.push(entry),
                HookPhase::Shutdown => shutdown.push(entry),
            }
        }
        for (kind, body) in jobs {
            job_factories.insert(kind, body);
        }
        for descriptor in manifest.subscriptions() {
            subscriptions.push((module_id.clone(), descriptor.clone()));
        }
        for binding in manifest.event_payload_bindings() {
            event_payload_bindings.push((module_id.clone(), binding.clone()));
        }
    }

    Ok(LoweredModules {
        core_builder,
        guard,
        startup,
        shutdown,
        job_factories,
        operation_descriptors,
        subscriptions,
        event_payload_bindings,
    })
}

fn collect_event_payload_binding_map(
    bindings: &[(String, EventPayloadBinding)],
) -> BTreeMap<u16, String> {
    bindings
        .iter()
        .map(|(_module, binding)| (binding.kind_raw(), binding.payload_schema_ref().to_owned()))
        .collect()
}

fn validate_event_payload_bindings(
    modules: &[HostModuleParts],
    composition_schemas: &crate::composition::HostCompositionManifest,
) -> Result<(), HostError> {
    for parts in modules {
        let module_id = parts.manifest.id();
        for binding in parts.manifest.event_payload_bindings() {
            let reference = binding.payload_schema_ref();
            let matches = composition_schemas
                .schemas()
                .filter_map(|entry| {
                    let schema = entry.descriptor();
                    if schema.id().as_str() == reference
                        && schema.role() == SchemaRole::EventPayload
                    {
                        Some(schema)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [_descriptor] => {}
                _ => {
                    return Err(HostError::EventPayloadBindingSchemaMissing {
                        module: module_id.to_owned(),
                        kind: binding.kind_raw(),
                        reference: reference.to_owned(),
                    });
                }
            }
        }
    }
    Ok(())
}

fn validate_subscription_payload_schemas(
    modules: &[HostModuleParts],
    composition_schemas: &crate::composition::HostCompositionManifest,
) -> Result<(), HostError> {
    for parts in modules {
        let module_id = parts.manifest.id();
        for descriptor in parts.manifest.subscriptions() {
            let role = descriptor.required_payload_role();
            let reference = descriptor.payload_schema_ref();
            let matches = composition_schemas
                .schemas()
                .filter_map(|entry| {
                    let schema = entry.descriptor();
                    if schema.id().as_str() == reference && schema.role() == role {
                        Some(schema)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [_descriptor] => {}
                _ => {
                    return Err(HostError::SubscriptionPayloadSchemaMissing {
                        module: module_id.to_owned(),
                        subscription: descriptor.id().as_str().to_owned(),
                        reference: reference.to_owned(),
                        role: role.as_str().to_owned(),
                    });
                }
            }
        }
    }
    Ok(())
}

struct SchemaValidatingHandler {
    operation: String,
    input_schema_ref: String,
    output_schema_ref: String,
    registry: SchemaRegistry,
    inner: BoxedHandler,
}

impl SchemaValidatingHandler {
    fn new(
        descriptor: &OperationDescriptor,
        registry: SchemaRegistry,
        inner: BoxedHandler,
    ) -> Self {
        Self {
            operation: descriptor.name().to_owned(),
            input_schema_ref: descriptor.input_schema_ref().to_owned(),
            output_schema_ref: descriptor.output_schema_ref().to_owned(),
            registry,
            inner,
        }
    }
}

impl Handler for SchemaValidatingHandler {
    fn handle(&mut self, input: &[u8], cx: &mut Ctx<'_>) -> HandlerResult {
        self.registry
            .validate(&self.input_schema_ref, SchemaRole::OperationInput, input)
            .map_err(|error| {
                HandlerError::invalid_input(format!(
                    "operation {} input schema validation failed: {error}",
                    self.operation
                ))
            })?;
        let output = self.inner.handle(input, cx)?;
        self.registry
            .validate(
                &self.output_schema_ref,
                SchemaRole::OperationOutput,
                &output,
            )
            .map_err(|error| {
                HandlerError::failed(format!(
                    "operation {} output schema validation failed: {error}",
                    self.operation
                ))
            })?;
        Ok(output)
    }
}

fn compute_fingerprint(modules: &[HostModuleParts]) -> Result<HostFingerprint, HostError> {
    let mut entries: Vec<FingerprintEntry> = modules
        .iter()
        .map(|parts| FingerprintEntry {
            module_id: parts.manifest.id().to_owned(),
            module_digest: *parts.manifest.digest().bytes(),
        })
        .collect();
    entries.sort_by(|a, b| a.module_id.cmp(&b.module_id));
    let view = FingerprintView {
        domain: HOST_FINGERPRINT_DOMAIN,
        modules: entries,
    };
    canonical_digest(&view).map(HostFingerprint)
}

#[derive(serde::Serialize)]
struct FingerprintEntry {
    module_id: String,
    module_digest: [u8; 32],
}

#[derive(serde::Serialize)]
struct FingerprintView {
    domain: &'static str,
    modules: Vec<FingerprintEntry>,
}

/// Routes each operation to the guard of the module that declared it. Operations
/// from modules with no guard are admitted.
#[derive(Default)]
struct CompositeGuard {
    route: BTreeMap<String, usize>,
    guards: Vec<BoxedGuard>,
}

impl CompositeGuard {
    fn add(&mut self, operations: Vec<String>, guard: BoxedGuard) {
        let index = self.guards.len();
        self.guards.push(guard);
        for operation in operations {
            self.route.insert(operation, index);
        }
    }

    fn is_empty(&self) -> bool {
        self.guards.is_empty()
    }
}

impl AdmissionGuard for CompositeGuard {
    fn admit(
        &self,
        descriptor: &OperationDescriptor,
        input: &[u8],
        cx: &mut Ctx<'_>,
    ) -> AdmissionDecision {
        match self.route.get(descriptor.name()) {
            Some(&index) => self.guards[index].admit(descriptor, input, cx),
            None => AdmissionDecision::Admit,
        }
    }
}
