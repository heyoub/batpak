//! `dangerous-test-hooks` proof hooks for the Linux backend S1 coupling proof
//! (`coupling_proof.rs`): build representative profiles WITHOUT the live kernel,
//! expose the production [`BackendProfile`] ceiling + the [`ProfileFacts`] the §3
//! floor predicate is checked against. Split from backend_impl.rs to hold it under
//! the non-overridable file-size cap; the whole module is feature-gated so the
//! default public surface is unaffected.

use super::super::support_matrix;
use super::{default_secret_resolver, LinuxBackend, LANDLOCK_ABI_FLOOR};
use crate::contract::backend::Backend;
use crate::contract::capability::Enforcement;
use crate::contract::ids::BackendId;
use crate::contract::plan::BoundaryRequirement;
use crate::contract::support::{BackendProfile, RequirementKind};

impl LinuxBackend {
    /// The minimum landlock ABI this backend floors `Filesystem` to `Enforced` at.
    pub const LANDLOCK_ABI_FLOOR: i64 = LANDLOCK_ABI_FLOOR;

    /// A representative profile with a FORCED cgroup confinement base at the FS ABI
    /// floor — the production-shaped profile that backs Filesystem + Kill +
    /// process_count Enforced. `pids_peak` controls the `ResourceUsage` evidence.
    #[must_use]
    pub fn with_cgroup_for_proof(pids_peak: bool) -> Self {
        Self {
            id: BackendId::new(Self::ID),
            support: support_matrix(),
            landlock_abi: LANDLOCK_ABI_FLOOR,
            launcher_path: None,
            cgroup_base: Some(std::path::PathBuf::from("/sys/fs/cgroup/proof-placeholder")),
            cgroup_pids_peak: pids_peak,
            // The production-shaped proof profile permits unprivileged userns+netns, so its
            // ceiling backs NetworkDenyAll=Enforced and the coupling gate qualifies it.
            netns_available: true,
            secret_resolver: default_secret_resolver(),
        }
    }

    /// A representative profile with a FORCED landlock ABI and NO cgroup base — for
    /// proving the at-floor and below-floor FS ceilings without a live kernel.
    #[must_use]
    pub fn with_abi_for_proof(landlock_abi: i64) -> Self {
        Self {
            id: BackendId::new(Self::ID),
            support: support_matrix(),
            landlock_abi,
            launcher_path: None,
            cgroup_base: None,
            cgroup_pids_peak: false,
            // The ABI-focused proof profiles isolate the Filesystem floor; force netns OFF
            // so NetworkDenyAll does not enter the ceiling and the FS/Kill coupling tests
            // stay focused on exactly their cells.
            netns_available: false,
            secret_resolver: default_secret_resolver(),
        }
    }

    /// The PRODUCTION ceiling ([`BackendProfile`]) this profile advertises — the
    /// per-kind enforcement table the coupling gate scans for `Enforced` cells.
    #[must_use]
    pub fn proof_ceiling(&self) -> BackendProfile {
        self.ceiling()
    }

    /// The TYPED machine facts the §3 [`ProfileFloor`] predicate is checked against
    /// (the live ABI integer + whether the cgroup base backs atomic kill / a peak
    /// witness). Distinct from the ceiling: the floor is stated over these facts.
    #[must_use]
    pub fn proof_facts(&self) -> crate::contract::qualification::ProfileFacts {
        crate::contract::qualification::ProfileFacts {
            landlock_abi: self.landlock_abi,
            has_cgroup_kill: self.cgroup_base.is_some(),
            has_pids_peak: self.cgroup_pids_peak,
            has_unprivileged_userns: self.netns_available,
        }
    }

    /// The backend's live mechanism string for a requirement kind at an enforcement
    /// level — the digest source the coupling gate matches the ledger against. Builds
    /// a representative requirement for the kind so `mechanism(..)` names the
    /// primitive (the mechanism is keyed on `RequirementKind::of(req)`, so any
    /// representative of the kind yields the same primitive string).
    #[must_use]
    pub fn proof_mechanism(&self, kind: RequirementKind, enforcement: Enforcement) -> String {
        self.mechanism(&representative_requirement(kind), enforcement)
    }
}

/// Build a representative [`BoundaryRequirement`] for a [`RequirementKind`] so the
/// backend's `mechanism(..)` can name the primitive. Used ONLY by the coupling
/// proof hook; every cell whose mechanism the ledger commits is covered.
fn representative_requirement(kind: RequirementKind) -> BoundaryRequirement {
    use crate::contract::capability::{
        Capability, EnvPolicy, FdPolicy, FsAccess, FsConfinement, NetDest, NetPolicy, PathSet,
        SpawnPolicy,
    };
    use crate::contract::host_control::{
        CommitDurability, HostControl, KillGuarantee, KillTarget, PathView, StdStreams,
    };
    match kind {
        RequirementKind::Filesystem => BoundaryRequirement::Capability(Capability::Filesystem {
            access: FsAccess::Read,
            scope: PathSet::empty(),
            recursive: true,
            confinement: FsConfinement::DeclaredRootsOnly,
        }),
        RequirementKind::NetworkDenyAll => BoundaryRequirement::Capability(Capability::Network {
            policy: NetPolicy::DenyAll,
        }),
        RequirementKind::NetworkAllowList => BoundaryRequirement::Capability(Capability::Network {
            policy: NetPolicy::AllowList(vec![NetDest {
                host: "example".to_string(),
                port: 443,
            }]),
        }),
        RequirementKind::ChildSpawnDenyNewTasks => {
            BoundaryRequirement::Capability(Capability::ChildSpawn {
                policy: SpawnPolicy::DenyNewTasks,
            })
        }
        RequirementKind::ChildSpawnAllowThreads => {
            BoundaryRequirement::Capability(Capability::ChildSpawn {
                policy: SpawnPolicy::AllowThreadsWithinBoundary,
            })
        }
        RequirementKind::ChildSpawnAllowDescendants => {
            BoundaryRequirement::Capability(Capability::ChildSpawn {
                policy: SpawnPolicy::AllowDescendantsWithinBoundary,
            })
        }
        RequirementKind::Environment => BoundaryRequirement::Capability(Capability::Environment {
            policy: EnvPolicy::Exact(Vec::new()),
        }),
        RequirementKind::InheritedFdsNone => {
            BoundaryRequirement::Capability(Capability::InheritedFds {
                policy: FdPolicy::None,
            })
        }
        RequirementKind::InheritedFdsOnly => {
            BoundaryRequirement::Capability(Capability::InheritedFds {
                policy: FdPolicy::Only(vec![3]),
            })
        }
        RequirementKind::LaunchWorkload => {
            BoundaryRequirement::HostControl(HostControl::LaunchWorkload)
        }
        RequirementKind::CaptureStreams => {
            BoundaryRequirement::HostControl(HostControl::CaptureStreams {
                streams: StdStreams::capture_out_err(),
            })
        }
        RequirementKind::TempRoot => BoundaryRequirement::HostControl(HostControl::TempRoot {
            visibility: PathView::PrivateToBoundary,
        }),
        RequirementKind::ExposePath => BoundaryRequirement::HostControl(HostControl::ExposePath {
            source: String::new(),
            dest: String::new(),
            access: FsAccess::Read,
            view: PathView::PrivateToBoundary,
        }),
        RequirementKind::CommitArtifact => {
            BoundaryRequirement::HostControl(HostControl::CommitArtifact {
                durability: CommitDurability::Atomic,
            })
        }
        RequirementKind::DiscardArtifact => {
            BoundaryRequirement::HostControl(HostControl::DiscardArtifact)
        }
        RequirementKind::Kill => BoundaryRequirement::HostControl(HostControl::Kill {
            target: KillTarget::RunTree,
            guarantee: KillGuarantee::Atomic,
        }),
        RequirementKind::ListOutputs => BoundaryRequirement::HostControl(HostControl::ListOutputs),
    }
}
