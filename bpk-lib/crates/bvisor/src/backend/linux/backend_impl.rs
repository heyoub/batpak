//! [`LinuxBackend`] — REAL landlock filesystem confinement + cgroup v2 resource
//! confinement, enforced THROUGH the host-side launcher harness (backend→launcher
//! rewire 7b; cgroup steps 8a–8b-ii-b2).
//!
//! SCOPE: `execute()` builds a [`LinuxLaunchPlanV1`] from the admitted plan (descriptor
//! table over pre-opened authority handles + a scrub/landlock-apply/exec lowering
//! schedule), creates a per-run cgroup leaf (when a cgroup base was probed), resolves the
//! confinement launcher binary, and runs it via [`super::launch::run_launcher`]. The
//! LAUNCHER applies the landlock ruleset in its single-threaded child window
//! (`restrict_self`, after the fd scrub, before `fexecve`) AND births the child INSIDE
//! the cgroup leaf via `clone3(CLONE_INTO_CGROUP)`, so both FS and resource confinement
//! are in force the instant the workload image runs. The backend NO LONGER
//! spawns/confines itself — its only raw surface is the landlock ABI probe (for
//! `profile()`); the harness owns the memfd/spawn `unsafe` and the cgroup manager is pure
//! SAFE `std::fs`. The orchestration here is SAFE.
//!
//! HONESTY (the cardinal rule): `profile()` backs ONLY what this build genuinely
//! delivers, gated on the live probes —
//!   - `Filesystem` (landlock) — Enforced at/above the ABI floor, else Unsupported;
//!   - `LaunchWorkload` + `CaptureStreams` — always (process spawn + pipe capture);
//!   - `Kill{RunTree,Atomic}` (cgroup `cgroup.kill`) — Enforced ONLY when a cgroup base
//!     with atomic kill was probed, else ABSENT ⇒ Unsupported;
//!   - Budget `process_count` (cgroup `pids.max`, witnessed from `pids.peak`) — Enforced
//!     ONLY when a cgroup base was probed; every OTHER budget dimension stays `Mediated`
//!     (observed-not-capped — no cap installed, so claiming Enforced would over-claim).
//!
//!   - `Environment` (explicit envp) — Enforced: the admitted `Environment::Exact`
//!     policy is lowered to the launcher's explicit env (literals + parent-resolved
//!     secret leases), served verbatim to `fexecve` with no ambient inheritance.
//!   - `InheritedFds::None` (fd-scrub) — Enforced: the admitted `FdPolicy::None` drives
//!     the launcher's child-side fd-scrub, which closes every undeclared inherited fd
//!     before `fexecve` (only the declared descriptor-table authority + stdio survive).
//!     `InheritedFds::Only` stays absent (Unsupported) — the scrub realizes only `None`.
//!
//! EVERYTHING ELSE (`InheritedFds::Only`, `ChildSpawn`, `NetworkDenyAll`, `TempRoot`, …) is ABSENT from the
//! ceiling ⇒ `Unsupported` ⇒ `plan()` fails closed. The family `support_matrix()` keeps
//! the §4 aspiration; the machine ceiling reflects reality. Claiming more than
//! `execute()` delivers is the exact lie the gauntlet must catch — so we do not.
//!
//! ## What the launcher path observes vs. the OLD self-spawn path
//! The launcher reports its honest setup transcript (terminal + phase resolutions +
//! `confinement_installed`) on its control fd, AND the host CAPTURES the workload's
//! stdout/stderr (step 7b): the launcher is stdio-silent on every workload-running path,
//! so the workload's inherited stdio flows through the launcher's piped fd 1/2 and the
//! host reads it back — backing `CaptureStreams=Enforced` + the `stream_captured` fact.
//! A landlock DENIAL is still proven by the INDEPENDENT on-disk oracle in the G-grid
//! (not parsed from stderr); the report's honest confinement evidence is the launcher's
//! `confinement_installed` mechanism attestation. The terminal maps to the
//! [`Outcome`] via [`super::launch::LaunchObservation::outcome`]
//! (ExecSucceeded→Completed, SetupRefused→Unsupported, SetupFaulted→SupervisorFault).

use crate::backend::linux::cgroup_run::{cgroup_for_run, finish};
use crate::backend::linux::launch;
use crate::backend::linux::{cgroup, plan_build, sys};
use crate::contract::backend::Backend;
use crate::contract::capability::{Enforcement, EvidenceClaim, FsAccess, PathSet, SupportVerdict};
use crate::contract::ids::BackendId;
use crate::contract::plan::{BoundaryPlan, BoundaryRequirement, Workload};
use crate::contract::report::{BoundaryReportBody, ObservedFact, Outcome};
use crate::contract::secret::{MapSecretResolver, SecretResolver};
use crate::contract::support::{
    BackendProfile, BackendProfileSnapshot, RequirementKind, SupportMatrix,
};
use std::collections::BTreeMap;

/// The minimum landlock ABI this backend requires to floor `Filesystem` to
/// `Enforced`. ABI v1 already enforces path-beneath read/write/execute access —
/// the foundation of declared-roots-only confinement — so v1 is the honest floor.
/// Below it (ABI 0 = landlock unavailable) `Filesystem` floors to `Unsupported`.
const LANDLOCK_ABI_FLOOR: i64 = 1;

// The frozen launcher-wire constants (served primitive ids + phase codes) and the
// descriptor-table slot fd numbers live in `plan_build`, which assembles the launch
// plan from them. `backend_impl` only orchestrates; it does not name them directly.

/// The Linux boundary backend: REAL landlock filesystem confinement.
pub struct LinuxBackend {
    id: BackendId,
    support: SupportMatrix,
    /// The live landlock ABI integer, probed once at construction from the kernel.
    landlock_abi: i64,
    /// An explicit launcher-binary path INJECTED at construction (constructor
    /// injection, NOT a process-env mutation — `std::env::set_var` is banned as
    /// thread-unsafe, BANNED-003). When `Some`, `execute()` runs exactly this
    /// launcher; when `None`, it resolves via the documented `BVISOR_LAUNCHER_BIN`
    /// env / co-located-binary fallback. A test deps binary has no co-located
    /// launcher, so the integration tests inject the compile-time launcher path here
    /// instead of racing the process environment.
    launcher_path: Option<std::path::PathBuf>,
    /// The cgroup v2 confinement base, PROBED ONCE at construction (the nearest
    /// writable `pids`-delegating ancestor that ALSO exposes atomic `cgroup.kill`).
    /// `Some` ⇒ `execute()` runs the workload in a real cgroup leaf (placed at birth
    /// via the launcher's `CLONE_INTO_CGROUP`) whose run tree the host can atomically
    /// `cgroup.kill`, so the ceiling backs `Kill{RunTree,Atomic}=Enforced`. `None` ⇒
    /// no cgroup confinement, so `Kill` is absent from the ceiling ⇒ Unsupported ⇒
    /// `plan()` fails closed for a kill spec (no unbacked guarantee). `pub(super)` so the
    /// `cgroup_run` lifecycle module can read it.
    pub(super) cgroup_base: Option<std::path::PathBuf>,
    /// Whether `pids.peak` (the process-count usage WITNESS, cgroup v2 ≥ 6.1) is present
    /// under the cgroup base, PROBED at construction. DISTINCT from the `pids.max` Hard
    /// cap (which `cgroup_base` already backs): a kernel can cap pids without exposing a
    /// peak. The profile advertises the `ResourceUsage` evidence claim ONLY when this is
    /// true, so a plan requiring that evidence never admits on a kernel that cannot
    /// witness it. Always `false` when `cgroup_base` is `None`.
    cgroup_pids_peak: bool,
    /// Whether this host permits UNPRIVILEGED user + network namespace creation (the
    /// `NetworkDenyAll` empty-netns floor, S9 / D3), PROBED ONCE at construction via
    /// [`launch::unprivileged_userns_available`]. `true` ⇒ `execute()` engages an empty
    /// netns for an admitted `NetworkDenyAll`, so the ceiling backs
    /// `NetworkDenyAll=Enforced`. `false` ⇒ the cell is absent from the ceiling ⇒
    /// Unsupported ⇒ `plan()` fails closed (no unbacked netns guarantee — the FAIL_CLOSED
    /// floor branch).
    netns_available: bool,
    /// Whether this host supports installing a seccomp BPF FILTER (the
    /// `ChildSpawn::DenyNewTasks` floor, S10), PROBED ONCE at construction via
    /// [`seccomp::seccomp_filter_available`]. `true` ⇒ `execute()` installs a default-allow
    /// seccomp denylist (deny the task-creation family) for an admitted
    /// `ChildSpawn::DenyNewTasks`, so the ceiling backs `ChildSpawnDenyNewTasks=Enforced`.
    /// `false` ⇒ the cell is absent from the ceiling ⇒ Unsupported ⇒ `plan()` fails closed
    /// (the FAIL_CLOSED floor branch — never a silent unfiltered run).
    seccomp_available: bool,
    /// The host's secret-lease resolver (proof-spine §5 D2). `execute()` resolves
    /// every [`crate::EnvSource::SecretLease`] in the admitted `Environment::Exact`
    /// table through this, in the PARENT, immediately before launch — the resolved
    /// VALUE goes only into the child's envp, never the durable plan/report. Defaults
    /// to an EMPTY [`MapSecretResolver`] (every lease fails closed); production injects
    /// a real store-backed resolver via [`Self::with_secret_resolver`]. `Arc<dyn ..>`
    /// so the backend stays shareable behind `Arc<dyn Backend>`.
    pub(super) secret_resolver: std::sync::Arc<dyn SecretResolver + Send + Sync>,
}

/// The default secret resolver: an EMPTY map, so any `SecretLease` in an admitted
/// environment fails closed (the launch is refused, the target never runs) unless the
/// host injects a real resolver. Fail-closed by construction.
pub(super) fn default_secret_resolver() -> std::sync::Arc<dyn SecretResolver + Send + Sync> {
    std::sync::Arc::new(MapSecretResolver::new())
}

/// Probe the cgroup v2 confinement capabilities ONCE at construction. Returns the base —
/// the nearest writable `pids`-delegating ancestor that ALSO exposes atomic `cgroup.kill`
/// (where a leaf can be created and a child placed via `CLONE_INTO_CGROUP`) — paired with
/// whether `pids.peak` (the usage witness) is present. `(None, false)` ⇒ no cgroup
/// confinement. Atomic kill is REQUIRED for a usable base (the Kill capability); the peak
/// witness is OPTIONAL (gates only the `ResourceUsage` evidence claim, never the cap).
fn probe_cgroup() -> (Option<std::path::PathBuf>, bool) {
    let Some(base) = cgroup::probe_controller_base(&["pids"]) else {
        return (None, false);
    };
    let caps = cgroup::probe_leaf_caps(&base);
    if caps.atomic_kill {
        (Some(base), caps.pids_peak)
    } else {
        (None, false)
    }
}

impl LinuxBackend {
    /// The stable id of the Linux backend.
    pub const ID: &'static str = "linux";

    /// Construct the Linux backend, probing the live landlock ABI from the kernel
    /// (the raw probe is the sanctioned `super::sys` basement).
    #[must_use]
    pub fn new() -> Self {
        let (cgroup_base, cgroup_pids_peak) = probe_cgroup();
        Self {
            id: BackendId::new(Self::ID),
            support: super::support_matrix(),
            landlock_abi: sys::probe_landlock_abi(),
            launcher_path: None,
            cgroup_base,
            cgroup_pids_peak,
            netns_available: launch::unprivileged_userns_available(),
            seccomp_available: super::seccomp::seccomp_filter_available(),
            secret_resolver: default_secret_resolver(),
        }
    }

    /// Replace the host secret-lease resolver (proof-spine §5 D2). Production injects a
    /// real store-backed resolver here so `execute()` can resolve `SecretLease` env
    /// entries JIT in the parent; the default is the empty fail-closed resolver.
    #[must_use]
    pub fn with_secret_resolver(
        mut self,
        resolver: std::sync::Arc<dyn SecretResolver + Send + Sync>,
    ) -> Self {
        self.secret_resolver = resolver;
        self
    }

    /// Construct the Linux backend with an EXPLICIT launcher-binary path (constructor
    /// injection — the thread-safe alternative to mutating `BVISOR_LAUNCHER_BIN` via the
    /// banned `std::env::set_var`). `execute()` then runs exactly this launcher. Used by
    /// the integration tests, whose deps binary has no co-located launcher to resolve.
    #[must_use]
    pub fn with_launcher_path(launcher_path: std::path::PathBuf) -> Self {
        let (cgroup_base, cgroup_pids_peak) = probe_cgroup();
        Self {
            id: BackendId::new(Self::ID),
            support: super::support_matrix(),
            landlock_abi: sys::probe_landlock_abi(),
            launcher_path: Some(launcher_path),
            cgroup_base,
            cgroup_pids_peak,
            netns_available: launch::unprivileged_userns_available(),
            seccomp_available: super::seccomp::seccomp_filter_available(),
            secret_resolver: default_secret_resolver(),
        }
    }

    /// Construct a backend with a FORCED landlock ABI, for proving the below-floor
    /// fail-closed path on a host whose live ABI is above the floor. Test-only.
    /// `cgroup_base` is `None` so these FS-focused tests see `Kill` Unsupported.
    #[cfg(test)]
    fn with_abi_for_test(landlock_abi: i64) -> Self {
        Self {
            id: BackendId::new(Self::ID),
            support: super::support_matrix(),
            landlock_abi,
            launcher_path: None,
            cgroup_base: None,
            cgroup_pids_peak: false,
            // FS-focused tests force netns OFF so NetworkDenyAll stays Unsupported in the
            // ceiling (the `unimplemented_kinds_fail_closed` test asserts exactly this).
            netns_available: false,
            seccomp_available: false,
            secret_resolver: default_secret_resolver(),
        }
    }

    /// Construct a backend with a FORCED cgroup confinement base (at the FS ABI floor),
    /// for proving the `Kill`/`process_count`-Enforced ceiling + the fail-closed path
    /// WITHOUT a real cgroup. The base path is a non-creatable placeholder, so the ceiling
    /// (which never touches disk) sees cgroup backing while any actual leaf creation FAILS
    /// — exactly the input the fail-closed `cgroup_for_run` test needs. `pids_peak`
    /// controls whether the `ResourceUsage` evidence claim is advertised. Test-only.
    #[cfg(test)]
    fn with_cgroup_for_test(pids_peak: bool) -> Self {
        Self {
            id: BackendId::new(Self::ID),
            support: super::support_matrix(),
            landlock_abi: LANDLOCK_ABI_FLOOR,
            launcher_path: None,
            cgroup_base: Some(std::path::PathBuf::from("/sys/fs/cgroup/test-placeholder")),
            cgroup_pids_peak: pids_peak,
            // The cgroup-focused tests do not exercise the netns cell; keep it OFF so they
            // assert exactly the Kill/process_count ceiling without a NetworkDenyAll cell.
            netns_available: false,
            seccomp_available: false,
            secret_resolver: default_secret_resolver(),
        }
    }

    /// Whether the live ABI meets the floor required to enforce FS confinement.
    fn filesystem_enforced(&self) -> bool {
        self.landlock_abi >= LANDLOCK_ABI_FLOOR
    }

    /// Whether this host can structurally enforce `NetworkDenyAll` via an empty network
    /// namespace (S9 / D3): it permits UNPRIVILEGED user + network namespace creation
    /// (the S8 rendezvous makes the child root-in-userns; `CLONE_NEWNET` then births it
    /// into an empty netns). `false` ⇒ the cell is absent from the ceiling (FAIL_CLOSED).
    fn network_deny_all_enforced(&self) -> bool {
        self.netns_available
    }

    /// Whether this host can enforce `ChildSpawn::DenyNewTasks` via a seccomp DENYLIST
    /// (S10): it supports installing a seccomp BPF filter (`CONFIG_SECCOMP_FILTER`). The
    /// launcher installs a default-allow denylist refusing clone/clone3/fork/vfork at the
    /// syscall-number level. `false` ⇒ the cell is absent from the ceiling (FAIL_CLOSED —
    /// never a silent unfiltered run).
    fn child_spawn_deny_enforced(&self) -> bool {
        self.seccomp_available
    }

    /// Whether this host can enforce `ChildSpawn::AllowDescendantsWithinBoundary` via the
    /// CGROUP boundary (S10): a cgroup base with atomic `cgroup.kill` was probed, so a
    /// descendant inherits the run cgroup (killable via `cgroup.kill`, counted by
    /// `pids.max`, namespace-trapped) — NOT seccomp. `false` ⇒ the cell is absent from the
    /// ceiling (FAIL_CLOSED — no unbacked descendant-boundary guarantee).
    fn child_spawn_descendants_enforced(&self) -> bool {
        self.cgroup_base.is_some()
    }

    /// The honest machine ceiling: `Filesystem` Enforced ONLY when the ABI floor
    /// is met (else absent ⇒ Unsupported), plus `LaunchWorkload`+`CaptureStreams`
    /// (process spawn + pipe capture, which this chunk genuinely performs).
    /// Nothing else is listed — every other kind floors to `Unsupported`, so
    /// `plan()` fails closed for capabilities this chunk does not back.
    fn ceiling(&self) -> BackendProfile {
        let mut ceiling = BTreeMap::new();
        if self.filesystem_enforced() {
            ceiling.insert(
                RequirementKind::Filesystem,
                SupportVerdict::new(
                    Enforcement::Enforced,
                    [
                        EvidenceClaim::AllowedActions,
                        EvidenceClaim::DeniedAttempts,
                        EvidenceClaim::FilesystemDelta,
                        EvidenceClaim::MechanismAttestation,
                    ]
                    .into_iter()
                    .collect(),
                ),
            );
        }
        ceiling.insert(
            RequirementKind::LaunchWorkload,
            SupportVerdict::new(
                Enforcement::Enforced,
                [EvidenceClaim::TerminalOutcome, EvidenceClaim::ProcessTree]
                    .into_iter()
                    .collect(),
            ),
        );
        ceiling.insert(
            RequirementKind::CaptureStreams,
            SupportVerdict::new(
                Enforcement::Enforced,
                [EvidenceClaim::CapturedStreams].into_iter().collect(),
            ),
        );
        // Environment::Exact is ENFORCED (proof-spine S4 completion): the admitted
        // policy is LOWERED to the launcher's explicit envp (literals + parent-resolved
        // secret leases) and the launcher serves EXACTLY that env to fexecve with NO
        // ambient inheritance — proven end-to-end by the dual-channel + fail-closed
        // oracle (`tests/env_exact_linux.rs`) and coupled to the Proven ledger row. The
        // mechanism is `linux:explicit_env:Enforced`; the evidence is a mechanism
        // attestation (the host independently confirms the child's /proc/<pid>/environ
        // equals the admitted table).
        ceiling.insert(
            RequirementKind::Environment,
            SupportVerdict::new(
                Enforcement::Enforced,
                [EvidenceClaim::MechanismAttestation].into_iter().collect(),
            ),
        );
        // InheritedFds::None is ENFORCED (proof-spine S5 completion): the admitted
        // FdPolicy::None DRIVES the launcher's child-side fd-scrub, which closes EVERY
        // undeclared inherited fd (the allowlist complement) before fexecve — only the
        // declared descriptor-table authority + stdio survive, so a leaked host handle
        // cannot reach the confined workload. Proven end-to-end by the dual-channel +
        // fail-closed oracle (`tests/inherited_fds_none_linux.rs`) and coupled to the
        // Proven ledger row; the mechanism is `linux:fd_scrub:Enforced`, the evidence a
        // mechanism attestation (the host independently confirms a non-CLOEXEC sentinel
        // fd is ABSENT from the child's /proc/<pid>/fd). InheritedFds::Only STAYS absent
        // from the ceiling (Unsupported ⇒ fails closed) — the scrub realizes only `None`.
        ceiling.insert(
            RequirementKind::InheritedFdsNone,
            SupportVerdict::new(
                Enforcement::Enforced,
                [EvidenceClaim::MechanismAttestation].into_iter().collect(),
            ),
        );
        // NetworkDenyAll is ENFORCED (proof-spine S9 / D3) ONLY when this host permits
        // UNPRIVILEGED user + network namespace creation: the admitted NetPolicy::DenyAll
        // engages a NEW, EMPTY network namespace (CLONE_NEWNET, alongside the S8 userns
        // rendezvous) — no external interface, so the workload is STRUCTURALLY unable to
        // reach any network (the S5 fd-scrub already closed any inherited routable socket).
        // Proven end-to-end by the dual-channel + fail-closed oracle
        // (`tests/network_deny_all_linux.rs`: host reads the child's /proc/<pid>/net/dev and
        // sees ONLY `lo` + the workload self-reports it cannot reach the network) and coupled
        // to the Proven ledger row; the mechanism is `linux:empty_netns:Enforced`. Without
        // unprivileged userns+netns the cell is ABSENT (Unsupported ⇒ plan() fails closed —
        // the FAIL_CLOSED floor branch, never a silent pass). NetworkAllowList STAYS absent
        // (no broker in v1).
        if self.network_deny_all_enforced() {
            ceiling.insert(
                RequirementKind::NetworkDenyAll,
                SupportVerdict::new(
                    Enforcement::Enforced,
                    [
                        EvidenceClaim::DeniedAttempts,
                        EvidenceClaim::MechanismAttestation,
                    ]
                    .into_iter()
                    .collect(),
                ),
            );
        }
        // The two S10 ChildSpawn cells (DenyNewTasks via seccomp + AllowDescendants via cgroup)
        // are inserted by the helper below to hold `ceiling` under the complexity budget.
        self.insert_child_spawn_ceiling(&mut ceiling);
        // Kill{RunTree,Atomic} is Enforced ONLY when a cgroup confinement base with
        // atomic `cgroup.kill` was probed: the workload runs in a cgroup leaf (placed
        // at birth by the launcher's CLONE_INTO_CGROUP), so the host can SIGKILL the
        // ENTIRE run tree atomically with no escape window. With no cgroup base, Kill is
        // absent ⇒ Unsupported ⇒ plan() fails closed (no unbacked kill guarantee).
        if self.cgroup_base.is_some() {
            ceiling.insert(
                RequirementKind::Kill,
                SupportVerdict::new(
                    Enforcement::Enforced,
                    [EvidenceClaim::MechanismAttestation].into_iter().collect(),
                ),
            );
        }
        BackendProfile::from_ceiling(ceiling)
    }

    /// Insert the two S10 `ChildSpawn` ceiling cells (split from [`Self::ceiling`] to hold it
    /// under the complexity budget):
    ///
    /// - `ChildSpawn::DenyNewTasks` is ENFORCED ONLY when this host supports seccomp FILTER
    ///   mode: the launcher installs a default-allow seccomp DENYLIST refusing
    ///   clone/clone3/fork/vfork at the syscall-number level (LAST, after landlock, before
    ///   fexecve; EPERM so the workload's fork fails observably). ONE composed layer — the broad
    ///   confinement is landlock/cgroup/netns/fd-scrub. Proven by the dual-channel + fail-closed
    ///   oracle (`tests/child_spawn_linux.rs`: host reads /proc/<pid>/status Seccomp:2 + the
    ///   workload's fork is refused); the mechanism is `linux:seccomp_deny_tasks:Enforced`.
    /// - `ChildSpawn::AllowDescendantsWithinBoundary` is ENFORCED ONLY when a cgroup base was
    ///   probed: a descendant INHERITS the run cgroup (killable via cgroup.kill, counted by
    ///   pids.max, namespace-trapped — the S1 mechanisms; NOT seccomp). Proven by the same oracle
    ///   (host confirms cgroup.procs + cgroup.kill drains the tree); the mechanism is
    ///   `linux:cgroup_descendant_boundary:Enforced`.
    /// - `ChildSpawn::AllowThreads` STAYS absent (the open clone3-pointer/classic-BPF problem,
    ///   S6 — FailClosed, never faked).
    fn insert_child_spawn_ceiling(&self, ceiling: &mut BTreeMap<RequirementKind, SupportVerdict>) {
        if self.child_spawn_deny_enforced() {
            ceiling.insert(
                RequirementKind::ChildSpawnDenyNewTasks,
                SupportVerdict::new(
                    Enforcement::Enforced,
                    [
                        EvidenceClaim::DeniedAttempts,
                        EvidenceClaim::ProcessTree,
                        EvidenceClaim::MechanismAttestation,
                    ]
                    .into_iter()
                    .collect(),
                ),
            );
        }
        if self.child_spawn_descendants_enforced() {
            ceiling.insert(
                RequirementKind::ChildSpawnAllowDescendants,
                SupportVerdict::new(
                    Enforcement::Enforced,
                    [
                        EvidenceClaim::ProcessTree,
                        EvidenceClaim::MechanismAttestation,
                    ]
                    .into_iter()
                    .collect(),
                ),
            );
        }
    }
}

#[cfg(feature = "dangerous-test-hooks")]
#[path = "backend_impl_proof.rs"]
mod proof_hooks;

// The Environment::Exact lowering helpers (admitted-policy extraction + spec→envp
// lowering with parent-side secret-lease resolution), split out to hold this file under
// the non-overridable size cap. SAFE std; the OS work lives in the launcher.
#[path = "backend_impl_env.rs"]
mod env_lowering;
use env_lowering::lower_environment;

// The InheritedFds::None lowering gate (admitted FdPolicy → the launcher fd-scrub),
// split out to hold this file under the size cap. SAFE std; the scrub is the launcher's.
#[path = "backend_impl_fds.rs"]
mod fds_lowering;
use fds_lowering::lower_inherited_fds;

// The NetworkDenyAll lowering gate (admitted NetPolicy → the launcher empty-netns
// engagement), split out to hold this file under the size cap. SAFE std; the netns +
// userns rendezvous are the launcher's.
#[path = "backend_impl_net.rs"]
mod net_lowering;
use net_lowering::{lower_network, NetLowering};

// The ChildSpawn child-task lowering gate (admitted SpawnPolicy → the launcher seccomp
// denylist / cgroup boundary, proof-spine S10), split out to hold this file under the size
// cap. SAFE std; the seccomp install is the launcher's.
#[path = "backend_impl_childspawn.rs"]
mod child_spawn_lowering;
use child_spawn_lowering::{lower_child_spawn, ChildTaskLowering};

// The honest budget profile derivation, split out to hold this file under the size cap.
#[path = "backend_impl_budget.rs"]
mod budget_profile;
use budget_profile::observed_budget_profile;

impl Default for LinuxBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for LinuxBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn support(&self) -> &SupportMatrix {
        &self.support
    }

    fn probe(&self) -> BackendProfileSnapshot {
        // REAL probe facts: the live landlock ABI integer and whether the FS floor
        // is met. Deterministic given the kernel, so replay re-derives identically.
        let mut probed = BTreeMap::new();
        probed.insert("landlock_abi".to_string(), self.landlock_abi.to_string());
        probed.insert(
            "filesystem_confinement".to_string(),
            if self.filesystem_enforced() {
                "landlock".to_string()
            } else {
                "unsupported-below-abi-floor".to_string()
            },
        );
        BackendProfileSnapshot {
            backend: self.id.clone(),
            probed,
            budget: observed_budget_profile(self.cgroup_base.is_some(), self.cgroup_pids_peak),
        }
    }

    fn profile(&self, _snap: &BackendProfileSnapshot) -> BackendProfile {
        // The ceiling is derived from the live ABI: FS Enforced only above the
        // floor; otherwise FS is absent ⇒ Unsupported ⇒ plan() fails closed.
        self.ceiling()
    }

    fn classify(&self, req: &BoundaryRequirement, profile: &BackendProfile) -> SupportVerdict {
        self.support.classify(req, profile)
    }

    fn mechanism(&self, requirement: &BoundaryRequirement, enforcement: Enforcement) -> String {
        // This backend authors only the mechanisms it actually performs this
        // chunk; everything else is honestly named as unimplemented-this-chunk.
        // Exhaustive ON PURPOSE (no bare wildcard over known variants): this chunk
        // backs exactly Filesystem/LaunchWorkload/CaptureStreams; every other kind
        // is honestly named unimplemented-this-chunk. A future variant must declare
        // its mechanism here rather than silently inheriting the unimplemented tag.
        let primitive = match RequirementKind::of(requirement) {
            RequirementKind::Filesystem => "landlock",
            RequirementKind::LaunchWorkload => "process_spawn",
            RequirementKind::CaptureStreams => "pipe_capture",
            // Kill rides cgroup v2 `cgroup.kill` (atomic run-tree teardown) — backed
            // ONLY when a cgroup base was probed (the ceiling gates the actual claim;
            // this names the mechanism this backend uses for Kill).
            RequirementKind::Kill => "cgroup_kill",
            // Environment::Exact rides the launcher's explicit envp (the admitted table
            // is lowered to the env served to fexecve; nothing inherited).
            RequirementKind::Environment => "explicit_env",
            // InheritedFds::None rides the launcher's child-side fd-scrub: the admitted
            // FdPolicy::None drives the scrub close-list (every undeclared inherited fd
            // is closed before fexecve). InheritedFds::Only is NOT realized (below).
            RequirementKind::InheritedFdsNone => "fd_scrub",
            // NetworkDenyAll rides an EMPTY network namespace (CLONE_NEWNET + the userns
            // rendezvous): no external interface, so the workload is structurally unable to
            // reach any network. Backed ONLY when the host permits unprivileged userns+netns
            // (the ceiling gates the actual claim; this names the mechanism).
            RequirementKind::NetworkDenyAll => "empty_netns",
            // ChildSpawn::DenyNewTasks rides a seccomp DENYLIST refusing the task-creation
            // family (clone/clone3/fork/vfork) at the syscall-number level — backed ONLY when
            // seccomp filter mode is supported (the ceiling gates the actual claim).
            RequirementKind::ChildSpawnDenyNewTasks => "seccomp_deny_tasks",
            // ChildSpawn::AllowDescendantsWithinBoundary rides the cgroup boundary (the
            // descendant inherits the run cgroup ⇒ killable/counted/namespace-trapped) — NOT
            // seccomp. Backed ONLY when a cgroup base was probed.
            RequirementKind::ChildSpawnAllowDescendants => "cgroup_descendant_boundary",
            RequirementKind::NetworkAllowList
            // ChildSpawn::AllowThreads is the open clone3-pointer/classic-BPF problem (S6) —
            // unenforceable, FailClosed, named honestly as unimplemented.
            | RequirementKind::ChildSpawnAllowThreads
            | RequirementKind::InheritedFdsOnly
            | RequirementKind::TempRoot
            | RequirementKind::ExposePath
            | RequirementKind::CommitArtifact
            | RequirementKind::DiscardArtifact
            | RequirementKind::ListOutputs => "none/unimplemented-this-chunk",
        };
        format!("{}:{primitive}:{enforcement:?}", self.id)
    }

    fn execute(&self, plan: &BoundaryPlan) -> BoundaryReportBody {
        execute_confined(self, plan)
    }
}

/// Run the admitted plan with REAL landlock confinement APPLIED BY THE LAUNCHER,
/// returning the honest observed body. Never panics: every failure resolves to an
/// honest [`Outcome`] (fail-closed — the workload never runs unconfined).
fn execute_confined(backend: &LinuxBackend, plan: &BoundaryPlan) -> BoundaryReportBody {
    let mut observed = Vec::new();

    // Only a native process workload is runnable by this backend.
    let (exe, args) = match &plan.workload {
        Workload::Process { exe, args } => (exe.clone(), args.clone()),
        Workload::Wasm { module_ref } => {
            observed.push(ObservedFact {
                kind: "workload_unsupported".to_string(),
                detail: format!("linux backend cannot run wasm module {module_ref}"),
            });
            return fail_closed(backend, plan, Outcome::Unsupported, observed);
        }
    };

    // The admitted Filesystem capability ⇒ confine; absent ⇒ no FS confinement was
    // requested (the launcher's Confinement phase resolves NotRequired). The plan
    // was admitted against our ceiling, so a Filesystem capability is
    // DeclaredRootsOnly + a PathSet.
    let fs = filesystem_capability(plan);

    // Fail closed below the ABI floor: we MUST NOT run a Filesystem-scoped workload
    // unconfined while the profile claims enforcement. (The launcher ALSO re-probes
    // and refuses below its floor, but the backend refuses FIRST so it never even
    // builds an unenforceable confinement plan.)
    if fs.is_some() && !backend.filesystem_enforced() {
        observed.push(ObservedFact {
            kind: "filesystem_confinement_unavailable".to_string(),
            detail: format!(
                "landlock abi {} below floor {LANDLOCK_ABI_FLOOR}; refusing to run unconfined",
                backend.landlock_abi
            ),
        });
        return fail_closed(backend, plan, Outcome::Unsupported, observed);
    }

    // LOWER the admitted Environment::Exact policy to the concrete envp the launcher
    // serves (proof-spine §5 D2). FAIL CLOSED on any lowering fault (invalid policy or
    // an unresolvable lease): the workload never runs.
    let envp = match lower_environment(backend, plan, observed) {
        Ok((envp, facts)) => {
            observed = facts;
            envp
        }
        Err(facts) => return fail_closed(backend, plan, Outcome::Unsupported, facts),
    };

    // LOWER the admitted InheritedFds policy onto the launcher fd-scrub (proof-spine S5).
    // The descriptor-table-driven scrub realizes `FdPolicy::None` (every undeclared
    // inherited fd closed before fexecve). FAIL CLOSED if the admitted policy is one this
    // backend does not realize (`Only`): the workload never runs under an unrealized fd
    // guarantee.
    observed = match lower_inherited_fds(backend, plan, observed) {
        Ok(facts) => facts,
        Err(facts) => return fail_closed(backend, plan, Outcome::Unsupported, facts),
    };

    // LOWER the admitted Network policy onto the launcher's empty-netns engagement
    // (proof-spine S9 / D3). NetPolicy::DenyAll ⇒ deny_network (the launcher births the
    // child in a NEW, EMPTY netns + the userns rendezvous it requires). FAIL CLOSED on a
    // policy this backend does not realize (AllowList — no broker in v1): the workload
    // never runs under an unrealized network guarantee.
    let deny_network = match lower_network(backend, plan, observed) {
        Ok(NetLowering {
            deny_network,
            observed: facts,
        }) => {
            observed = facts;
            deny_network
        }
        Err(facts) => return fail_closed(backend, plan, Outcome::Unsupported, facts),
    };

    // LOWER the admitted ChildSpawn policy onto the launcher's child-task confinement
    // (proof-spine S10). DenyNewTasks ⇒ deny_new_tasks (the launcher installs a seccomp
    // denylist refusing the task-creation family); AllowDescendants ⇒ no filter (the cgroup
    // boundary is the mechanism). FAIL CLOSED on AllowThreads (the unenforceable open
    // problem): the workload never runs under an unrealized child-task guarantee.
    let deny_new_tasks = match lower_child_spawn(backend, plan, observed) {
        Ok(ChildTaskLowering {
            deny_new_tasks,
            observed: facts,
        }) => {
            observed = facts;
            deny_new_tasks
        }
        Err(facts) => return fail_closed(backend, plan, Outcome::Unsupported, facts),
    };

    run_prepared(
        backend,
        plan,
        &exe,
        &args,
        fs.as_ref(),
        LoweredLaunch {
            envp,
            deny_network,
            deny_new_tasks,
        },
        observed,
    )
}

/// The per-run cgroup leaf + launcher build + launcher run, factored out of
/// [`execute_confined`] to hold it under the complexity budget. Creates the cgroup leaf
/// (fail-closed if the plan admitted cgroup-backed guarantees but it cannot be created),
/// builds the launcher plan over the lowered `envp` + authority handles, resolves +
/// attests the launcher binary, runs it, and ALWAYS tears the leaf down via `finish`.
/// Every fault path resolves to an honest fail-closed [`BoundaryReportBody`].
/// The lowered launch inputs `execute_confined` hands to `run_prepared`: the concrete
/// `envp` (lowered Environment::Exact) + whether to engage the empty netns (lowered
/// NetworkDenyAll). Bundled so `run_prepared` stays within the argument budget (zero
/// `#[allow]` doctrine — no `too_many_arguments` lint).
struct LoweredLaunch {
    /// The explicit environment served to the workload (literals + resolved leases).
    envp: Vec<(String, String)>,
    /// Whether the admitted NetworkDenyAll engages the empty network namespace (S9 / D3).
    deny_network: bool,
    /// Whether the admitted ChildSpawn::DenyNewTasks engages the seccomp task-creation
    /// denylist (S10). `false` ⇒ no task-creation deny (no ChildSpawn / AllowDescendants).
    deny_new_tasks: bool,
}

fn run_prepared(
    backend: &LinuxBackend,
    plan: &BoundaryPlan,
    exe: &str,
    args: &[String],
    fs: Option<&(FsAccess, PathSet)>,
    lowered: LoweredLaunch,
    observed: Vec<ObservedFact>,
) -> BoundaryReportBody {
    let LoweredLaunch {
        envp,
        deny_network,
        deny_new_tasks,
    } = lowered;
    // Prepare the per-run cgroup leaf; FAIL CLOSED if the plan admitted cgroup-backed
    // guarantees but the leaf cannot be created (see `cgroup_for_run`). The launcher births
    // the child INTO the leaf via CLONE_INTO_CGROUP; `finish` tears it down after the run.
    let (cgroup_leaf, cgroup_dir_fd, mut observed) = match cgroup_for_run(backend, plan, observed) {
        Ok(triple) => triple,
        Err(observed) => return fail_closed(backend, plan, Outcome::SupervisorFault, observed),
    };

    // Build the launcher plan + the pre-opened authority handles host-side (now
    // including the optional CgroupDir slot). A host wiring fault (a root/exe that
    // cannot be opened, a slot that does not fit) fails closed — the workload never
    // runs, and the leaf is torn down via `finish`.
    let prepared = match plan_build::prepare_launch(plan_build::LaunchInputs {
        exe,
        args,
        plan,
        fs,
        cgroup_dir_fd,
        envp,
        deny_network,
        deny_new_tasks,
    }) {
        Ok(prepared) => prepared,
        Err(detail) => {
            observed.push(ObservedFact {
                kind: "launch_plan_construction_failed".to_string(),
                detail,
            });
            return finish(
                cgroup_leaf,
                fail_closed(backend, plan, Outcome::SupervisorFault, observed),
            );
        }
    };
    let plan_build::Prepared {
        launch_plan,
        authority,
        read_roots,
        write_roots,
        confined,
    } = prepared;

    // Record the confinement mechanism + the exact roots as honest evidence: the
    // landlock policy is now applied by the LAUNCHER's child-window restrict_self
    // (after the fd scrub, before fexecve), NOT a backend pre_exec.
    if confined {
        observed.push(ObservedFact {
            kind: "filesystem_confined".to_string(),
            detail: format!(
                "landlock abi {} (launcher restrict_self): read-roots {read_roots:?}, \
                 write-roots {write_roots:?}",
                backend.landlock_abi
            ),
        });
    }

    // Resolve the launcher binary; fail closed if unresolvable (NEVER run the
    // workload unconfined). An injected backend launcher path (constructor injection)
    // takes precedence over the env / co-located fallback.
    let launcher_path = match resolve_launcher(backend) {
        Ok(path) => path,
        Err(detail) => {
            observed.push(ObservedFact {
                kind: "launcher_unresolvable".to_string(),
                detail,
            });
            return finish(
                cgroup_leaf,
                fail_closed(backend, plan, Outcome::Unsupported, observed),
            );
        }
    };
    // Attest the launcher's BLAKE3 content identity (step 12 — see `attest_launcher`).
    attest_launcher(&launcher_path, &mut observed);

    // Run the launcher; map its honest observation onto the report contract, then ALWAYS
    // tear down the leaf (kill → drain → remove) via `finish`.
    let report = match launch::run_launcher(&launcher_path, &launch_plan, authority) {
        Ok(obs) => {
            // Read the cgroup pids high-water mark BEFORE teardown — the honest
            // `observed_usage` for the process_count budget witness. `pids.peak` persists
            // the max even after the workload exited, so this is valid here. `None` ⇒ no
            // cap installed or the kernel lacks `pids.peak` (then the witness stays
            // unwitnessed — Hard guarantee, ObservationUnavailable — never a fake peak).
            let process_peak = cgroup_leaf
                .as_ref()
                .and_then(|l| l.peak_pids().ok().flatten());
            map_observation(backend, plan, exe, confined, &obs, observed, process_peak)
        }
        Err(error) => {
            // A harness fault BEFORE the launcher produced a verdict (encode/slot/OS):
            // the workload never ran ⇒ SupervisorFault (fail-closed).
            observed.push(ObservedFact {
                kind: "launcher_harness_fault".to_string(),
                detail: format!("linux launcher harness fault: {error}"),
            });
            fail_closed(backend, plan, Outcome::SupervisorFault, observed)
        }
    };
    finish(cgroup_leaf, report)
}

// Launcher-binary resolution + content attestation (resolve_launcher/attest_launcher),
// split out to hold this file under the non-overridable file-size cap. SAFE std.
#[path = "backend_impl_resolve.rs"]
mod launcher_resolve;
use launcher_resolve::{attest_launcher, resolve_launcher};

// The launcher-observation → report-body mapping (map_observation/exec_exit/body +
// the fail_closed + filesystem_capability helpers), split out to hold this file under
// the non-overridable size cap. SAFE std; it only shapes the honest observation.
#[path = "backend_impl_report.rs"]
mod report_mapping;
use report_mapping::{fail_closed, filesystem_capability, map_observation};

#[cfg(test)]
#[path = "backend_impl_tests.rs"]
mod tests;
