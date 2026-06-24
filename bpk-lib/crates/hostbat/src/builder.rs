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
    AdmissionDecision, AdmissionGuard, CoreBuilder, Ctx, OperationDescriptor, ReceiptSink,
};

use crate::descriptor::HookPhase;
use crate::error::HostError;
use crate::host::{Host, HostHook};
use crate::identity::{canonical_digest, HostFingerprint};
use crate::module::{BoxedJob, HostModule, HostModuleParts};

type BoxedGuard = Box<dyn AdmissionGuard + 'static>;
type BoxedReceiptSink = Box<dyn ReceiptSink + 'static>;

/// Domain separator for the host-composition fingerprint.
const HOST_FINGERPRINT_DOMAIN: &str = "hostbat.host.v1";

/// Builder that mounts modules and lowers them into a runnable [`Host`].
pub struct HostBuilder {
    modules: Vec<HostModuleParts>,
    module_ids: BTreeSet<String>,
    operation_owners: BTreeMap<String, String>,
    receipt_namespaces: BTreeMap<String, String>,
    job_owners: BTreeMap<String, String>,
    spawn: Arc<dyn Spawn>,
    receipt_sink: Option<BoxedReceiptSink>,
}

impl Default for HostBuilder {
    fn default() -> Self {
        Self {
            modules: Vec::new(),
            module_ids: BTreeSet::new(),
            operation_owners: BTreeMap::new(),
            receipt_namespaces: BTreeMap::new(),
            job_owners: BTreeMap::new(),
            spawn: Arc::new(ThreadSpawn),
            receipt_sink: None,
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
            if self
                .operation_owners
                .insert(name.clone(), id.clone())
                .is_some()
            {
                return Err(HostError::DuplicateOperation {
                    operation: name,
                    module: id,
                });
            }
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

        let mut core_builder = CoreBuilder::new();
        let mut guard = CompositeGuard::default();
        let supervisor = crate::supervisor::Supervisor::new(self.spawn);
        let mut startup: Vec<HostHook> = Vec::new();
        let mut shutdown: Vec<HostHook> = Vec::new();
        let mut job_factories: BTreeMap<String, BoxedJob> = BTreeMap::new();

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
                core_builder.register_boxed(descriptor.clone(), handler)?;
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

        // Global deterministic hook order: (order, module-id, name). Module ids
        // are unique, so this is a total order with no cross-module ambiguity.
        startup.sort_by(|a, b| a.order_key().cmp(&b.order_key()));
        shutdown.sort_by(|a, b| a.order_key().cmp(&b.order_key()));

        let core = core_builder.build()?;
        Ok(Host::new(
            core,
            supervisor,
            fingerprint,
            startup,
            shutdown,
            job_factories,
        ))
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
