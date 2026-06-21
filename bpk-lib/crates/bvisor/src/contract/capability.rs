//! [`Capability`] — the guest-invokable admitted authority POLICY, plus
//! [`Enforcement`] (the matrix verdict) and the guarantee-shaped grades.
//!
//! A [`Capability`] is the admitted rule the boundary ENFORCES on what the
//! WORKLOAD may attempt. It carries GRANTS and RESTRICTIONS — a deny-all
//! network policy is a restriction, still a Capability because it is the
//! admitted authority policy the backend must honor. Host lifecycle lives in
//! [`crate::HostControl`], NOT here: the confined workload cannot self-grant a
//! commit, a temp root, or its own launch.
//!
//! GRADES ARE GUARANTEE-SHAPED, NOT MECHANISM-SHAPED. The spec says WHAT
//! guarantee is required; the backend says HOW (pivot_root / Landlock / preopen
//! / Job Object / …) and records it in
//! [`crate::AdmittedRequirement::mechanism`] as evidence. [`Enforcement::Unsupported`]
//! is NEVER a requested value — it is only ever the backend's answer.

use serde::{Deserialize, Serialize};

/// The matrix verdict for one requirement against one machine profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Enforcement {
    /// The backend can guarantee the requirement (strong primitive present).
    Enforced,
    /// The backend can honor the requirement only by mediating each attempt
    /// (e.g. a broker / notifier), not by a structural guarantee.
    Mediated,
    /// The backend cannot honor the requirement at all on this machine. Only
    /// ever a backend ANSWER; never a requested value. Forces `plan()` closed.
    Unsupported,
}

/// Guest-invokable admitted authority policy (grants AND restrictions).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Capability {
    /// Filesystem authority confined to a declared scope.
    Filesystem {
        /// Read / write / read-write grant.
        access: FsAccess,
        /// The declared roots the access is scoped to.
        scope: PathSet,
        /// Whether the scope applies recursively under each root.
        recursive: bool,
        /// The confinement GUARANTEE required (not a mechanism).
        confinement: FsConfinement,
    },
    /// Network authority: deny-all (restriction) or a scoped allow-list (grant).
    Network {
        /// The admitted network policy.
        policy: NetPolicy,
    },
    /// Authority for the workload to spawn its OWN children. The workload's
    /// initial launch is a [`crate::HostControl::LaunchWorkload`], not this.
    ChildSpawn {
        /// Whether the workload may spawn children.
        policy: SpawnPolicy,
    },
    /// Environment authority: empty-by-default; explicit grants only.
    Environment {
        /// The admitted environment policy.
        policy: EnvPolicy,
    },
    /// Which host file descriptors survive into the workload; default is none.
    InheritedFds {
        /// The admitted fd-inheritance policy.
        policy: FdPolicy,
    },
}

/// Filesystem access grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum FsAccess {
    /// Read only.
    Read,
    /// Write only.
    Write,
    /// Read and write.
    ReadWrite,
}

/// GUARANTEE: "reads/writes confined to the declared scope" — not a mechanism.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum FsConfinement {
    /// Access is confined to the declared roots and nothing outside them.
    DeclaredRootsOnly,
}

/// GUARANTEE: deny vs scoped-allow (a policy, not a mechanism).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum NetPolicy {
    /// All network access is denied (a restriction).
    DenyAll,
    /// Only the listed destinations are reachable (a scoped grant).
    AllowList(Vec<NetDest>),
}

/// Whether the workload may spawn child processes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SpawnPolicy {
    /// The workload may not spawn children.
    Deny,
    /// The workload may spawn children.
    Allow,
}

/// Environment-variable policy: empty by default, explicit keys only.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EnvPolicy {
    /// The environment is empty except for the explicitly granted keys.
    EmptyExcept(Vec<String>),
}

/// Host-fd inheritance policy: none by default, explicit fds only.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum FdPolicy {
    /// No host file descriptors survive into the workload.
    None,
    /// Only the listed raw fds survive into the workload.
    Only(Vec<u32>),
}

/// A declared set of filesystem roots a [`Capability::Filesystem`] is scoped to.
///
/// Portable, inert string paths — the contract never touches the filesystem, so
/// these are evidence/scope data, not opened handles.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PathSet {
    /// The declared roots, as portable path strings.
    pub roots: Vec<String>,
}

impl PathSet {
    /// An empty path set (no roots declared).
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }
}

/// A single allow-listed network destination (host + port), inert evidence.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NetDest {
    /// Destination host (name or address), as a portable string.
    pub host: String,
    /// Destination port.
    pub port: u16,
}
