//! Declarative, content-hashable descriptors for the non-operation axes a host
//! module mounts: its admission guard, its lifecycle hooks, and its supervised
//! jobs.
//!
//! These carry **declarations, never implementations** — a [`GuardDescriptor`]
//! attests *that* a module guards its operations (by a stable code), not the
//! guard's behavior; the runtime impl lives beside it on the module. Because the
//! manifest is derived from exactly the registered parts (see
//! [`crate::module::HostModule`]), these descriptors and the impls they describe
//! cannot drift.

use serde::Serialize;

/// The lifecycle phase a hook runs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HookPhase {
    /// Runs once, in deterministic order, when the host starts — before any
    /// operation is served.
    Startup,
    /// Runs once, in deterministic order, when the host shuts down — after the
    /// supervisor has joined.
    Shutdown,
}

impl std::fmt::Display for HookPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Startup => f.write_str("startup"),
            Self::Shutdown => f.write_str("shutdown"),
        }
    }
}

/// Declarative attestation that a module mounts an admission guard over its
/// operations. The `code` is a stable identifier of the guard policy folded into
/// module identity; the guard *behavior* is the impl mounted beside it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct GuardDescriptor {
    /// Stable guard-policy code (e.g. `"bvisor.boundary.admission.v1"`).
    pub code: String,
}

impl GuardDescriptor {
    /// Construct a guard descriptor from a stable policy code.
    pub fn new(code: impl Into<String>) -> Self {
        Self { code: code.into() }
    }
}

/// Declarative descriptor of one lifecycle hook: its phase, its stable name, and
/// its module-local order within that phase.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HookDescriptor {
    /// The phase this hook runs in.
    pub phase: HookPhase,
    /// Stable hook name (unique within its phase, per module).
    pub name: String,
    /// Module-local order within the phase; smaller runs first. The global
    /// schedule breaks ties across modules by module id, so independently
    /// authored modules never need to coordinate order numbers.
    pub order: u32,
}

impl HookDescriptor {
    /// Construct a hook descriptor.
    pub fn new(phase: HookPhase, name: impl Into<String>, order: u32) -> Self {
        Self {
            phase,
            name: name.into(),
            order,
        }
    }

    /// The canonical within-module ordering key for a hook: `(phase, order,
    /// name)`. Two hooks in the same phase sharing an `order` collide on the
    /// `(phase, order)` prefix — a within-module incoherence the module builder
    /// rejects.
    #[must_use]
    pub(crate) fn order_key(&self) -> (HookPhase, u32, &str) {
        (self.phase, self.order, self.name.as_str())
    }
}

/// Declarative descriptor of one supervised-job kind a module contributes to the
/// host's single generic supervisor.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct JobDescriptor {
    /// Stable job kind (globally unique across the composed host).
    pub kind: String,
}

impl JobDescriptor {
    /// Construct a job descriptor.
    pub fn new(kind: impl Into<String>) -> Self {
        Self { kind: kind.into() }
    }
}
