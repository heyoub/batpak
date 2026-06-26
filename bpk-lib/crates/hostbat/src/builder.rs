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
use crate::host::{Host, HostHook, HostParts};
use crate::identity::{canonical_digest, HostFingerprint};
use crate::interface::compute_interface_fingerprint;
use crate::module::{BoxedJob, HostModule, HostModuleParts};
use crate::schema::{SchemaRegistry, SchemaRole};

type BoxedGuard = Box<dyn AdmissionGuard + 'static>;
type BoxedHandler = Box<dyn Handler + 'static>;
type BoxedReceiptSink = Box<dyn ReceiptSink + 'static>;
type BoxedEffectBackend = Box<dyn EffectBackend + 'static>;

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
    schemas: CompositionSchemaBuilder,
    spawn: Arc<dyn Spawn>,
    receipt_sink: Option<BoxedReceiptSink>,
    effect_backend: Option<BoxedEffectBackend>,
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
            schemas: CompositionSchemaBuilder::default(),
            spawn: Arc::new(ThreadSpawn),
            receipt_sink: None,
            effect_backend: None,
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
        let interface_fingerprint =
            compute_interface_fingerprint(&self.modules, &composition_schemas)?;
        let schema_registry = SchemaRegistry::from_descriptors(
            composition_schemas
                .schemas()
                .map(|entry| entry.descriptor().clone()),
        );

        let mut core_builder = CoreBuilder::new();
        let mut guard = CompositeGuard::default();
        let supervisor = crate::supervisor::Supervisor::new(self.spawn);
        let mut startup: Vec<HostHook> = Vec::new();
        let mut shutdown: Vec<HostHook> = Vec::new();
        let mut job_factories: BTreeMap<String, BoxedJob> = BTreeMap::new();
        let mut operation_descriptors: Vec<OperationDescriptor> = Vec::new();

        for parts in self.modules {
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
        }

        if !guard.is_empty() {
            core_builder.admission_guard(guard);
        }
        if let Some(sink) = self.receipt_sink {
            core_builder.receipt_sink_boxed(sink);
        }
        if let Some(backend) = self.effect_backend {
            core_builder.effect_backend_boxed(backend);
        }

        // Global deterministic hook order: (order, module-id, name). Module ids
        // are unique, so this is a total order with no cross-module ambiguity.
        startup.sort_by(|a, b| a.order_key().cmp(&b.order_key()));
        shutdown.sort_by(|a, b| a.order_key().cmp(&b.order_key()));
        operation_descriptors.sort_by(|a, b| a.name().cmp(b.name()));

        let core = core_builder.build()?;
        Ok(Host::new(HostParts {
            core,
            supervisor,
            fingerprint,
            interface_fingerprint,
            operations: operation_descriptors,
            composition_schemas,
            schema_registry,
            startup,
            shutdown,
            job_factories,
        }))
    }
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
