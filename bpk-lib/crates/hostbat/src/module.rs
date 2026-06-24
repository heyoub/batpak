//! The runtime module: the manifest's declared parts plus the implementations
//! they describe, sealed together so they cannot drift.
//!
//! A [`HostModule`] is produced by a [`HostModuleBuilder`]. You register
//! `(descriptor, handler)` pairs, at most one guard, lifecycle hooks, supervised
//! jobs, and the receipt namespaces the module owns; `build` validates the module
//! is internally coherent and **derives** the [`HostModuleManifest`] from exactly
//! those parts. There is no way to author a manifest that disagrees with the
//! impls — the projection is the only constructor.

use std::collections::BTreeMap;

use syncbat::{AdmissionGuard, Handler, OperationDescriptor};

use crate::descriptor::{GuardDescriptor, HookDescriptor, HookPhase, JobDescriptor};
use crate::error::HostError;
use crate::manifest::HostModuleManifest;

type BoxedHandler = Box<dyn Handler + 'static>;
type BoxedGuard = Box<dyn AdmissionGuard + 'static>;
/// Boxed lifecycle hook, named by the host that runs it.
pub(crate) type BoxedHook = Box<dyn LifecycleHook + 'static>;
/// Boxed supervised-job factory, named by the host that spawns it.
pub(crate) type BoxedJob = Box<dyn JobBody + 'static>;

/// A deterministic lifecycle hook run once at host start or shutdown.
///
/// Hooks are fallible: returning `Err(detail)` aborts startup (fail-closed) or
/// surfaces a shutdown failure. The host stamps the owning module + hook name
/// onto the failure.
pub trait LifecycleHook {
    /// Run the hook.
    ///
    /// # Errors
    /// `Err(detail)` reports a stable failure detail; the host stamps the owning
    /// module and hook name onto it.
    fn run(&self) -> Result<(), String>;
}

impl<F> LifecycleHook for F
where
    F: Fn() -> Result<(), String>,
{
    fn run(&self) -> Result<(), String> {
        self()
    }
}

/// A factory for a supervised-job body. Each call produces a fresh unit of work
/// the host's generic supervisor runs over the [`batpak::store::Spawn`] seam.
pub trait JobBody: Send + Sync {
    /// Produce one fresh body to spawn.
    fn make(&self) -> Box<dyn FnOnce() + Send + 'static>;
}

impl<F> JobBody for F
where
    F: Fn() -> Box<dyn FnOnce() + Send + 'static> + Send + Sync,
{
    fn make(&self) -> Box<dyn FnOnce() + Send + 'static> {
        self()
    }
}

/// A built host module: a sealed manifest plus the implementations it attests.
pub struct HostModule {
    manifest: HostModuleManifest,
    handlers: BTreeMap<String, BoxedHandler>,
    guard: Option<BoxedGuard>,
    hooks: Vec<(HookDescriptor, BoxedHook)>,
    jobs: BTreeMap<String, BoxedJob>,
}

/// The owned parts a [`HostModule`] lowers into when a host is built.
pub(crate) struct HostModuleParts {
    pub(crate) manifest: HostModuleManifest,
    pub(crate) handlers: BTreeMap<String, BoxedHandler>,
    pub(crate) guard: Option<BoxedGuard>,
    pub(crate) hooks: Vec<(HookDescriptor, BoxedHook)>,
    pub(crate) jobs: BTreeMap<String, BoxedJob>,
}

impl HostModule {
    /// Start building a module with a stable id and version.
    #[must_use]
    pub fn builder(id: impl Into<String>, version: u32) -> HostModuleBuilder {
        HostModuleBuilder::new(id, version)
    }

    /// The sealed, content-identified manifest.
    #[must_use]
    pub fn manifest(&self) -> &HostModuleManifest {
        &self.manifest
    }

    /// Consume the module into its owned parts for lowering into a host.
    pub(crate) fn into_parts(self) -> HostModuleParts {
        HostModuleParts {
            manifest: self.manifest,
            handlers: self.handlers,
            guard: self.guard,
            hooks: self.hooks,
            jobs: self.jobs,
        }
    }

    /// Replace the manifest with a hash-tampered copy. **Test-only**, behind the
    /// gauntlet red-fixture cfg: feeds the `ModuleHashMismatch` mount fixture.
    #[cfg(any(test, gauntlet_red_fixture))]
    pub(crate) fn tamper_manifest_for_fixture(&mut self) {
        self.manifest.corrupt_digest_for_fixture();
    }
}

/// Builder that validates a single module and derives its manifest.
pub struct HostModuleBuilder {
    id: String,
    version: u32,
    operations: BTreeMap<String, (OperationDescriptor, BoxedHandler)>,
    receipt_namespaces: Vec<String>,
    guard: Option<(GuardDescriptor, BoxedGuard)>,
    hooks: Vec<(HookDescriptor, BoxedHook)>,
    jobs: BTreeMap<String, (JobDescriptor, BoxedJob)>,
}

impl HostModuleBuilder {
    /// Create an empty module builder.
    #[must_use]
    pub fn new(id: impl Into<String>, version: u32) -> Self {
        Self {
            id: id.into(),
            version,
            operations: BTreeMap::new(),
            receipt_namespaces: Vec::new(),
            guard: None,
            hooks: Vec::new(),
            jobs: BTreeMap::new(),
        }
    }

    /// Register an operation descriptor and its handler together.
    ///
    /// # Errors
    /// [`HostError::ModuleCoherence`] if the descriptor is invalid or its name is
    /// already registered in this module.
    pub fn operation<H>(
        mut self,
        descriptor: OperationDescriptor,
        handler: H,
    ) -> Result<Self, HostError>
    where
        H: Handler + 'static,
    {
        descriptor
            .validate()
            .map_err(|error| HostError::coherence(&self.id, error.to_string()))?;
        let name = descriptor.name().to_owned();
        if self.operations.contains_key(&name) {
            return Err(HostError::coherence(
                &self.id,
                format!("operation {name:?} is declared twice"),
            ));
        }
        self.operations
            .insert(name, (descriptor, Box::new(handler)));
        Ok(self)
    }

    /// Mount the module's single admission guard, attested by `descriptor`.
    ///
    /// # Errors
    /// [`HostError::ModuleCoherence`] if a guard is already set.
    pub fn guard<G>(mut self, descriptor: GuardDescriptor, guard: G) -> Result<Self, HostError>
    where
        G: AdmissionGuard + 'static,
    {
        if self.guard.is_some() {
            return Err(HostError::coherence(
                &self.id,
                "a module mounts at most one guard",
            ));
        }
        self.guard = Some((descriptor, Box::new(guard)));
        Ok(self)
    }

    /// Declare a receipt-extension namespace this module owns.
    ///
    /// # Errors
    /// [`HostError::ModuleCoherence`] if the namespace is declared twice.
    pub fn receipt_namespace(mut self, namespace: impl Into<String>) -> Result<Self, HostError> {
        let namespace = namespace.into();
        if self.receipt_namespaces.contains(&namespace) {
            return Err(HostError::coherence(
                &self.id,
                format!("receipt namespace {namespace:?} is declared twice"),
            ));
        }
        self.receipt_namespaces.push(namespace);
        Ok(self)
    }

    /// Register a lifecycle hook in `phase` with module-local `order`.
    pub fn hook<H>(mut self, phase: HookPhase, name: impl Into<String>, order: u32, hook: H) -> Self
    where
        H: LifecycleHook + 'static,
    {
        self.hooks
            .push((HookDescriptor::new(phase, name, order), Box::new(hook)));
        self
    }

    /// Register a supervised-job factory for `kind`.
    ///
    /// # Errors
    /// [`HostError::ModuleCoherence`] if the job kind is declared twice.
    pub fn job<J>(mut self, kind: impl Into<String>, body: J) -> Result<Self, HostError>
    where
        J: JobBody + 'static,
    {
        let kind = kind.into();
        if self.jobs.contains_key(&kind) {
            return Err(HostError::coherence(
                &self.id,
                format!("supervised-job kind {kind:?} is declared twice"),
            ));
        }
        self.jobs
            .insert(kind.clone(), (JobDescriptor::new(kind), Box::new(body)));
        Ok(self)
    }

    /// Validate the module and derive its sealed manifest.
    ///
    /// # Errors
    /// [`HostError::ModuleCoherence`] if the id is malformed, the module declares
    /// nothing, or two hooks in one phase share an order; or
    /// [`HostError::CanonicalEncoding`] if sealing fails.
    pub fn build(self) -> Result<HostModule, HostError> {
        validate_module_id(&self.id)?;
        if self.operations.is_empty()
            && self.hooks.is_empty()
            && self.jobs.is_empty()
            && self.guard.is_none()
        {
            return Err(HostError::coherence(
                &self.id,
                "module declares no operations, hooks, jobs, or guard",
            ));
        }

        // The BTreeMaps already hold operations and jobs in canonical key order.
        let mut handlers = BTreeMap::new();
        let mut operation_descriptors = Vec::with_capacity(self.operations.len());
        for (name, (descriptor, handler)) in self.operations {
            operation_descriptors.push(descriptor);
            handlers.insert(name, handler);
        }

        let mut job_descriptors = Vec::with_capacity(self.jobs.len());
        let mut jobs = BTreeMap::new();
        for (kind, (descriptor, body)) in self.jobs {
            job_descriptors.push(descriptor);
            jobs.insert(kind, body);
        }

        let (guard_descriptor, guard_impl) = match self.guard {
            Some((descriptor, guard)) => (Some(descriptor), Some(guard)),
            None => (None, None),
        };

        let mut hooks = self.hooks;
        hooks.sort_by(|(a, _), (b, _)| a.order_key().cmp(&b.order_key()));
        reject_hook_order_collisions(&self.id, &hooks)?;
        let hook_descriptors: Vec<HookDescriptor> = hooks
            .iter()
            .map(|(descriptor, _)| descriptor.clone())
            .collect();

        let mut receipt_namespaces = self.receipt_namespaces;
        receipt_namespaces.sort();

        let manifest = HostModuleManifest::seal(
            self.id,
            self.version,
            operation_descriptors,
            receipt_namespaces,
            guard_descriptor,
            hook_descriptors,
            job_descriptors,
        )?;

        Ok(HostModule {
            manifest,
            handlers,
            guard: guard_impl,
            hooks,
            jobs,
        })
    }
}

/// Reject two hooks in the same phase sharing an order (ambiguous ordering). The
/// slice is pre-sorted by `(phase, order, name)`, so a collision is an adjacent
/// pair agreeing on `(phase, order)`.
fn reject_hook_order_collisions(
    module: &str,
    hooks: &[(HookDescriptor, BoxedHook)],
) -> Result<(), HostError> {
    for window in hooks.windows(2) {
        let [(a, _), (b, _)] = window else { continue };
        if a.phase == b.phase && a.order == b.order {
            return Err(HostError::hook_order_collision(module, a.phase, a.order));
        }
    }
    Ok(())
}

/// Validate a module id: non-empty, ≤128 bytes, ASCII `[a-z0-9._-]+`, no leading,
/// trailing, or doubled `.`.
fn validate_module_id(id: &str) -> Result<(), HostError> {
    let reject = |detail: &str| {
        Err(HostError::coherence(
            id,
            format!("invalid module id: {detail}"),
        ))
    };
    if id.is_empty() {
        return reject("empty");
    }
    if id.len() > 128 {
        return reject("longer than 128 bytes");
    }
    if id.starts_with('.') || id.ends_with('.') {
        return reject("leading or trailing '.'");
    }
    if id.contains("..") {
        return reject("doubled '.'");
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
    {
        return reject("characters outside [a-z0-9._-]");
    }
    Ok(())
}
