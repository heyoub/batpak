//! The composed, runnable host.
//!
//! A [`Host`] is what [`crate::HostBuilder::build`] produces: one `syncbat`
//! [`Core`] (operations + composed guard + receipt sink), a content-composition
//! [`HostFingerprint`], a deterministic startup/shutdown hook schedule, and a
//! generic [`Supervisor`] over the [`batpak::store::Spawn`] seam. The host
//! delegates operation dispatch to the `Core` (it does not reimplement it) and
//! owns only the lifecycle and identity the `Core` has no concept of.
//!
//! [`Core`]: syncbat::Core

use std::collections::BTreeMap;

use syncbat::{CheckoutResult, Core, RuntimeError};

use crate::descriptor::{HookDescriptor, HookPhase};
use crate::error::{HookFailure, HostRuntimeError};
use crate::identity::HostFingerprint;
use crate::module::{BoxedHook, BoxedJob};
use crate::supervisor::Supervisor;

/// One lifecycle hook bound to the module that owns it, ready to run in the
/// host's global deterministic order.
pub struct HostHook {
    module: String,
    descriptor: HookDescriptor,
    hook: BoxedHook,
}

impl HostHook {
    pub(crate) fn new(module: String, descriptor: HookDescriptor, hook: BoxedHook) -> Self {
        Self {
            module,
            descriptor,
            hook,
        }
    }

    pub(crate) fn phase(&self) -> HookPhase {
        self.descriptor.phase
    }

    /// Total-order key across modules: `(order, module-id, name)`. Module ids are
    /// unique, so this orders every hook in a phase unambiguously.
    pub(crate) fn order_key(&self) -> (u32, &str, &str) {
        (
            self.descriptor.order,
            self.module.as_str(),
            self.descriptor.name.as_str(),
        )
    }

    fn run(&self) -> Result<(), HookFailure> {
        self.hook.run().map_err(|detail| HookFailure {
            phase: self.descriptor.phase,
            module: self.module.clone(),
            hook: self.descriptor.name.clone(),
            detail,
        })
    }
}

/// A composed, runnable module host.
pub struct Host {
    core: Core,
    supervisor: Supervisor,
    fingerprint: HostFingerprint,
    startup: Vec<HostHook>,
    shutdown: Vec<HostHook>,
    job_factories: BTreeMap<String, BoxedJob>,
    started: bool,
}

impl Host {
    pub(crate) fn new(
        core: Core,
        supervisor: Supervisor,
        fingerprint: HostFingerprint,
        startup: Vec<HostHook>,
        shutdown: Vec<HostHook>,
        job_factories: BTreeMap<String, BoxedJob>,
    ) -> Self {
        Self {
            core,
            supervisor,
            fingerprint,
            startup,
            shutdown,
            job_factories,
            started: false,
        }
    }

    /// The host-composition fingerprint `H_host`.
    #[must_use]
    pub fn fingerprint(&self) -> HostFingerprint {
        self.fingerprint
    }

    /// Whether [`start`](Self::start) has completed successfully.
    #[must_use]
    pub fn is_started(&self) -> bool {
        self.started
    }

    /// Run the startup hooks in deterministic order. Fail-closed: the first hook
    /// failure aborts startup and leaves the host not-started.
    ///
    /// # Errors
    /// [`HostRuntimeError::StartupHook`] on the first failing hook.
    pub fn start(&mut self) -> Result<(), HostRuntimeError> {
        for hook in &self.startup {
            hook.run().map_err(HostRuntimeError::StartupHook)?;
        }
        self.started = true;
        Ok(())
    }

    /// Invoke an operation, delegating dispatch to the composed `syncbat` runtime
    /// (which runs the composed admission guard and records the receipt).
    ///
    /// # Errors
    /// The [`RuntimeError`] the underlying [`Core::invoke`] returns (unknown
    /// operation, guard denial, handler failure, receipt-sink failure).
    pub fn invoke(
        &mut self,
        operation: impl AsRef<str>,
        input: Vec<u8>,
    ) -> Result<CheckoutResult, RuntimeError> {
        self.core.invoke(operation, input)
    }

    /// Spawn a supervised job by its declared kind. The owning module's factory
    /// produces a fresh body, which the supervisor runs over the [`Spawn`] seam.
    ///
    /// [`Spawn`]: batpak::store::Spawn
    ///
    /// # Errors
    /// [`HostRuntimeError::UnknownJobKind`] if no module declares the kind, or
    /// [`HostRuntimeError::Spawn`] if the body could not be started.
    pub fn spawn_job(&mut self, kind: impl AsRef<str>) -> Result<(), HostRuntimeError> {
        let kind = kind.as_ref();
        let factory =
            self.job_factories
                .get(kind)
                .ok_or_else(|| HostRuntimeError::UnknownJobKind {
                    kind: kind.to_owned(),
                })?;
        let body = factory.make();
        self.supervisor.spawn(kind.to_owned(), None, body)?;
        Ok(())
    }

    /// Read-only access to the generic supervisor (job statuses, counts).
    #[must_use]
    pub fn supervisor(&self) -> &Supervisor {
        &self.supervisor
    }

    /// Run the shutdown hooks in deterministic order, then join every supervised
    /// job (blocking). Shutdown attempts every hook even if one fails and returns
    /// the first failure encountered; the supervisor is always drained.
    ///
    /// # Errors
    /// [`HostRuntimeError::ShutdownHook`] carrying the first failing shutdown
    /// hook, if any.
    pub fn shutdown(&mut self) -> Result<(), HostRuntimeError> {
        let mut first_failure = None;
        for hook in &self.shutdown {
            if let Err(failure) = hook.run() {
                if first_failure.is_none() {
                    first_failure = Some(failure);
                }
            }
        }
        // Drain the supervisor regardless of hook outcomes — shutdown hooks have
        // signalled jobs to wind down; now block until they finish.
        let _outcomes = self.supervisor.join_all();
        self.started = false;
        match first_failure {
            Some(failure) => Err(HostRuntimeError::ShutdownHook(failure)),
            None => Ok(()),
        }
    }
}
