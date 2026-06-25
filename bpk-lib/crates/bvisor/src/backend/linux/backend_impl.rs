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
//! EVERYTHING ELSE (`Environment`, `InheritedFds`, `ChildSpawn`, `NetworkDenyAll`, `TempRoot`, …) is ABSENT from the
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
use crate::backend::linux::launch::{self, LaunchObservation};
use crate::backend::linux::{cgroup, plan_build, sys};
use crate::contract::backend::Backend;
use crate::contract::budget::{BudgetAvailability, BudgetProfile};
use crate::contract::budget_witness::BudgetWitnesses;
use crate::contract::capability::{
    Capability, Enforcement, EvidenceClaim, EvidenceSet, FsAccess, PathSet, SupportVerdict,
};
use crate::contract::ids::BackendId;
use crate::contract::plan::{BoundaryPlan, BoundaryRequirement, Workload};
use crate::contract::report::{
    BoundaryReportBody, CaptureRefs, ExitStatus, ObservedFact, Outcome,
    BOUNDARY_REPORT_SCHEMA_VERSION,
};
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
        }
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
        }
    }

    /// Whether the live ABI meets the floor required to enforce FS confinement.
    fn filesystem_enforced(&self) -> bool {
        self.landlock_abi >= LANDLOCK_ABI_FLOOR
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
        // Environment + InheritedFds are NOT in the ceiling (⇒ Unsupported ⇒ plan()
        // fails closed). BACKED OUT after a codex adversarial review (2026-06-25): the
        // confinement contract admits on a RequirementKind (support.rs::of_capability is
        // policy-BLIND for these two — unlike Network, which splits DenyAll/AllowList into
        // distinct kinds), but the launcher only implements ONE policy shape and never
        // lowers the admitted policy. plan_build emits a HARDCODED envp regardless of the
        // spec's `EnvPolicy::EmptyExcept(..)`, and the scrub only realises
        // `FdPolicy::None`, never `Only(..)`. So advertising Enforced here would ADMIT
        // `Environment{EmptyExcept(["FOO"])}` / `InheritedFds{Only(fd)}` and silently NOT
        // deliver them — an over-claim. The launcher MECHANISM proofs survive as building
        // blocks (`tests/launcher_env_linux.rs`, `tests/launcher_inherited_fds_linux.rs`):
        // they prove the launcher CAN serve an explicit envp / scrub undeclared fds, NOT
        // that the BoundaryPlanner→execute() path admits + honors the contract policy.
        // Genuine completion (policy-aware admission + spec→envp/allowlist lowering + a
        // CONTRACT-level oracle) is real work, tracked with NetworkDenyAll/ChildSpawn.
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
}

impl Default for LinuxBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// The honest budget profile. PROCESS COUNT is STRUCTURALLY enforced via the cgroup
/// v2 `pids` controller (`pids.max`) when a cgroup base was probed (`cgroup_pids_enforced`)
/// — then it is `Enforced`/Hard. The `ResourceUsage` evidence claim (the `pids.peak`
/// usage witness) is advertised SEPARATELY, ONLY when `pids_peak_witness` was probed: a
/// kernel can cap pids (≥ 4.3) WITHOUT exposing `pids.peak` (≥ 6.1), so a Hard cap does
/// NOT imply a witness — advertising the witness off the cap would be the over-claim
/// codex caught. Every OTHER dimension is `Mediated` (supervised, not structurally capped)
/// with no resource evidence — no cap is installed, so claiming `Enforced` there would
/// over-claim. With no cgroup base, process count is `Mediated` too (no unbacked cap).
fn observed_budget_profile(cgroup_pids_enforced: bool, pids_peak_witness: bool) -> BudgetProfile {
    let observed = |mechanism: &str| BudgetAvailability {
        // Headroom only — we do NOT cap, so we never refuse on capacity here; the
        // honest signal is the `Mediated` (not `Enforced`) guarantee + empty
        // evidence, which forbids a spec from demanding a witnessed/enforced cap.
        available: u64::MAX,
        enforcement: Enforcement::Mediated,
        evidence: EvidenceSet::new(),
        mechanism: mechanism.to_string(),
    };
    // ProcessCount: a real structural cap (cgroup pids.max) when cgroup is available. The
    // ResourceUsage evidence is advertised ONLY when the pids.peak witness is also present.
    let process_count = if cgroup_pids_enforced {
        let evidence = if pids_peak_witness {
            [EvidenceClaim::ResourceUsage].into_iter().collect()
        } else {
            EvidenceSet::new()
        };
        BudgetAvailability {
            // The pids controller caps any count up to the kernel's pid ceiling.
            available: u64::MAX,
            enforcement: Enforcement::Enforced,
            evidence,
            mechanism: "cgroup_v2_pids:enforced".to_string(),
        }
    } else {
        observed("os_process:observed-not-capped")
    };
    BudgetProfile {
        wall_micros: observed("os_process_wait:observed-not-capped"),
        cpu_micros: observed("os_rusage:observed-not-capped"),
        resident_bytes: observed("os_rusage:observed-not-capped"),
        process_count,
        handle_count: observed("os_fd:observed-not-capped"),
        storage_bytes: observed("os_fs:observed-not-capped"),
        network_bytes: observed("os_net:observed-not-capped"),
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
            RequirementKind::NetworkDenyAll
            | RequirementKind::NetworkAllowList
            | RequirementKind::ChildSpawn
            | RequirementKind::Environment
            | RequirementKind::InheritedFds
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
    let prepared = match plan_build::prepare_launch(&exe, &args, plan, fs.as_ref(), cgroup_dir_fd) {
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
            map_observation(backend, plan, &exe, confined, &obs, observed, process_peak)
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

/// Record the BLAKE3 digest of the launcher binary observed AT THE RESOLVED PATH, BEFORE
/// spawn, as a provenance evidence fact.
///
/// HONEST SCOPE (the codex-review correction): this proves "the bytes at `path` when read
/// here", NOT "the exact bytes the kernel exec'd" — `run_launcher` later spawns by PATH
/// (`Command::new`), so a swap/symlink between this read and the exec is a TOCTOU window.
/// The fact wording reflects that. Closing the race (hash an OPENED fd and `fexecve` that
/// SAME fd) and digest PINNING (refuse on mismatch) are the follow-ons; this is provenance
/// EVIDENCE, not a gate, so a read failure is silently skipped (the launch still proceeds).
fn attest_launcher(path: &std::path::Path, observed: &mut Vec<ObservedFact>) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let digest = batpak::event::hash::compute_hash(&bytes);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    observed.push(ObservedFact {
        kind: "launcher_identity".to_string(),
        detail: format!(
            "blake3={hex} observed_at_path={} (pre-spawn; not the exec'd-fd bytes — \
             fd-exec pinning is the follow-on)",
            path.display()
        ),
    });
}

/// Resolve the launcher binary path, failing closed if unresolvable. Resolution
/// order: the backend's INJECTED `launcher_path` (constructor injection) FIRST, then
/// the `BVISOR_LAUNCHER_BIN` env override, else the `bvisor-linux-launcher` binary
/// CO-LOCATED with the current executable (the documented default install layout). If
/// none resolves to an existing file ⇒ `Err` (the caller reports `Outcome::Unsupported`
/// — the workload NEVER runs unconfined). The resolved binary's CONTENT digest is then
/// attested by [`attest_launcher`]; digest-PINNING the exact bin (refuse on mismatch) is
/// the follow-on.
fn resolve_launcher(backend: &LinuxBackend) -> Result<std::path::PathBuf, String> {
    // Injected launcher path (thread-safe constructor injection) takes precedence — it
    // is how the integration tests point at the compile-time launcher without the banned
    // process-env mutation. Confirm it exists so a bad inject still fails closed.
    if let Some(path) = &backend.launcher_path {
        if path.is_file() {
            return Ok(path.clone());
        }
        return Err(format!(
            "injected launcher path does not exist: {}",
            path.display()
        ));
    }
    // The override path is trusted as supplied (step-12 note); honor it even if a
    // stat would race, but still confirm it exists so we fail closed on a typo.
    if let Ok(p) = std::env::var(launch::ENV_LAUNCHER_BIN) {
        if !p.trim().is_empty() {
            let path = std::path::PathBuf::from(p);
            if path.is_file() {
                return Ok(path);
            }
            return Err(format!(
                "{} points to a non-existent launcher binary: {}",
                launch::ENV_LAUNCHER_BIN,
                path.display()
            ));
        }
    }
    // Default: the launcher next to the current executable.
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot locate the current executable to find the launcher: {e}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "current executable has no parent directory".to_string())?;
    let default = dir.join("bvisor-linux-launcher");
    if default.is_file() {
        return Ok(default);
    }
    Err(format!(
        "no launcher binary: {} unset and no bvisor-linux-launcher beside {}",
        launch::ENV_LAUNCHER_BIN,
        exe.display()
    ))
}

/// Map the launcher's honest [`LaunchObservation`] onto the report body, preserving
/// the report/evidence contract downstream (seal/persist 0xE) consumes.
///
/// HONESTY: the launcher reports its setup transcript (the terminal, the phase
/// resolutions, and `confinement_installed`) AND the host captures the WORKLOAD's
/// stdout/stderr through the launcher's inherited piped stdio (the launcher's clone3
/// child inherits the launcher's fd 0/1/2, and the launcher is stdio-silent on every
/// workload-running path, so the launcher process's piped stdout/stderr carry exactly
/// the workload's output). Those captured bytes back `CaptureStreams=Enforced`'s
/// `CapturedStreams` evidence claim — the body records the captured stream references
/// alongside a `stream_captured` byte-count fact. A landlock denial is STILL proven by
/// the INDEPENDENT on-disk oracle (the G-grid), and the honest confinement evidence is the
/// launcher's `confinement_installed` mechanism attestation. The terminal maps via the
/// protocol's `outcome_class` (ExecSucceeded becomes Completed, SetupRefused becomes
/// Unsupported as a fail-closed deny, and SetupFaulted becomes SupervisorFault), and a
/// missing terminal becomes SupervisorFault (the launcher died before resolving, so the
/// workload never ran).
fn map_observation(
    backend: &LinuxBackend,
    plan: &BoundaryPlan,
    exe: &str,
    confined: bool,
    obs: &LaunchObservation,
    mut observed: Vec<ObservedFact>,
    process_peak: Option<u64>,
) -> BoundaryReportBody {
    observed.push(ObservedFact {
        kind: "workload_launched".to_string(),
        detail: format!("launcher exec {exe} (confined={confined})"),
    });
    observed.push(ObservedFact {
        kind: "launcher_terminal".to_string(),
        detail: format!(
            "terminal={:?} confinement_installed={} launcher_exit={:?}",
            obs.terminal, obs.confinement_installed, obs.launcher_exit
        ),
    });
    // Surface the launcher's own mechanism notes (its honest attestation: the clone3
    // child pid, the confinement result/install). These are the mechanism evidence
    // the report carries now that the launcher (not the backend) confines.
    for note in &obs.notes {
        observed.push(ObservedFact {
            kind: "launcher_note".to_string(),
            detail: note.clone(),
        });
    }
    // A confined plan whose launcher reports NO install is an honesty fault, not a
    // silent pass: record it (the Outcome below still reflects the terminal).
    if confined && !obs.confinement_installed {
        observed.push(ObservedFact {
            kind: "confinement_not_installed".to_string(),
            detail: "a Filesystem-scoped plan ran but the launcher reported no \
                     landlock install"
                .to_string(),
        });
    }

    // The host captured the workload's stdout/stderr through the launcher's inherited
    // piped stdio (the launcher is stdio-silent on every workload-running path). Record
    // the honest byte-count fact + the stream references — this backs the
    // `CaptureStreams=Enforced` ceiling's `CapturedStreams` evidence claim. The bytes
    // are referenced (not inlined) to keep the report body bounded; the byte counts are
    // the audit evidence that capture actually flowed.
    observed.push(ObservedFact {
        kind: "stream_captured".to_string(),
        detail: format!(
            "captured {} stdout byte(s), {} stderr byte(s) via the launcher's \
             inherited piped stdio",
            obs.captured_stdout.len(),
            obs.captured_stderr.len()
        ),
    });
    let captured = CaptureRefs {
        stdout: Some(format!("inline:{}b", obs.captured_stdout.len())),
        stderr: Some(format!("inline:{}b", obs.captured_stderr.len())),
    };

    // The process_count budget witness: a REAL `pids.peak` measurement when a cgroup cap
    // was installed (admitted Enforced) AND the kernel exposed `pids.peak`; otherwise the
    // unwitnessed echo (Hard guarantee from the admitted Enforced, ObservationUnavailable
    // — never a fabricated peak). Surface the measured peak as honest evidence too.
    let process_enforced = plan.budgets.process_count.selected_guarantee == Enforcement::Enforced;
    let budget = match process_peak {
        Some(peak) if process_enforced => {
            observed.push(ObservedFact {
                kind: "process_count_witnessed".to_string(),
                detail: format!(
                    "cgroup pids.peak={peak} against pids.max={} (cgroup_v2_pids: Hard cap)",
                    plan.budgets.process_count.effective_limit
                ),
            });
            BudgetWitnesses::with_process_count(&plan.budgets, peak)
        }
        _ => BudgetWitnesses::unwitnessed(&plan.budgets),
    };

    let outcome = obs.outcome().unwrap_or(Outcome::SupervisorFault);
    // The launcher does not surface the workload's own exit code (it reports its
    // setup terminal); ExecSucceeded means the workload image began executing under
    // confinement. No portable workload ExitStatus is available through this path.
    let exit = exec_exit(outcome);
    body(backend, plan, outcome, exit, captured, observed, budget)
}

/// The portable workload exit the launcher path can honestly report. The launcher
/// surfaces ONLY its own setup terminal, not the workload's exit code: a `Completed`
/// outcome means the workload exec'd under confinement (a clean image start), which
/// we report as `ExitStatus::Code(0)`; every non-Completed terminal carries no
/// workload exit (the workload never ran, or the launcher itself faulted).
fn exec_exit(outcome: Outcome) -> Option<ExitStatus> {
    match outcome {
        Outcome::Completed => Some(ExitStatus::Code(0)),
        Outcome::Denied
        | Outcome::Failed
        | Outcome::Timeout
        | Outcome::Killed
        | Outcome::Unsupported
        | Outcome::SupervisorFault => None,
    }
}

/// Extract the admitted Filesystem capability's access + scope, if one was
/// admitted into the plan.
fn filesystem_capability(plan: &BoundaryPlan) -> Option<(FsAccess, PathSet)> {
    plan.admitted.iter().find_map(|a| match &a.requirement {
        BoundaryRequirement::Capability(Capability::Filesystem { access, scope, .. }) => {
            Some((*access, scope.clone()))
        }
        BoundaryRequirement::Capability(_) | BoundaryRequirement::HostControl(_) => None,
    })
}

/// Assemble a fail-closed report body (the workload never ran / ran-but-faulted
/// honestly): the given non-Completed [`Outcome`], no exit, no captured streams, no
/// denials. The accumulated `observed` facts carry WHY it failed closed.
fn fail_closed(
    backend: &LinuxBackend,
    plan: &BoundaryPlan,
    outcome: Outcome,
    observed: Vec<ObservedFact>,
) -> BoundaryReportBody {
    // A fail-closed path: the workload never ran (or faulted), so no dimension is
    // witnessed — the unwitnessed echo preserves the admitted contract honestly.
    body(
        backend,
        plan,
        outcome,
        None,
        CaptureRefs::default(),
        observed,
        BudgetWitnesses::unwitnessed(&plan.budgets),
    )
}

/// Assemble the honest report body. `budget` is the per-dimension witness set the
/// caller computed (the process_count dimension is genuinely witnessed from `pids.peak`
/// when a cgroup cap was installed; every other path passes the unwitnessed echo).
///
/// `denied` is always empty through the launcher path: a confinement DENIAL is proven by
/// the INDEPENDENT on-disk oracle (the G-grid), NOT self-reported here (the workload
/// inherits the launcher's stdio, so there is no stderr-derived denial to surface).
fn body(
    backend: &LinuxBackend,
    plan: &BoundaryPlan,
    outcome: Outcome,
    exit: Option<ExitStatus>,
    captured: CaptureRefs,
    observed: Vec<ObservedFact>,
    budget: BudgetWitnesses,
) -> BoundaryReportBody {
    BoundaryReportBody {
        schema_version: BOUNDARY_REPORT_SCHEMA_VERSION,
        plan_id: plan.plan_id,
        backend: backend.id.clone(),
        profile: backend.probe(),
        outcome,
        admitted: plan.admitted.clone(),
        observed,
        denied: Vec::new(),
        exit,
        captured,
        budget,
        artifacts: Vec::new(),
        findings: Vec::new(),
    }
}

#[cfg(test)]
#[path = "backend_impl_tests.rs"]
mod tests;
