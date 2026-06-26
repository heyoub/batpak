//! The QUALIFICATION axis (proof-spine §0/§1) — ORTHOGONAL to the RuntimeGuarantee
//! axis ([`crate::contract::capability::Enforcement`]). Never collapse them.
//!
//! - **RuntimeGuarantee** ([`Enforcement`]): what a backend CLAIMS it can guarantee
//!   for a particular plan + machine profile.
//! - **QualificationStatus** (this module): what the REPOSITORY has INDEPENDENTLY
//!   qualified through the complete path `spec → admission → lowering → execution →
//!   independent observation`, INCLUDING the fail-closed / teardown branches.
//!
//! THE COUPLING LAW: a production profile may advertise [`Enforcement::Enforced`]
//! for a requirement key ONLY when the committed qualification ledger holds
//! [`QualificationStatus::Proven`] for that key — with a profile floor the machine
//! satisfies and a matching mechanism digest. The coupling is enforced as a CI
//! GATE: the backend's ceiling claim is explicit; the ledger is DERIVED from proof
//! receipts; the gate asserts they agree. A backend can never self-stamp `Proven`.
//!
//! This is the structural fix for the over-claim class: the family `support_matrix`
//! may CLAIM `Enforced` as aspiration, but production advertises it only when the
//! claim is `Proven` — so an unproven advertised cell is an `Incomplete`
//! qualification, not a lie.

use crate::contract::capability::Enforcement;
use crate::contract::support::RequirementKind;
use serde::{Deserialize, Serialize};

/// What the repository has INDEPENDENTLY qualified about a capability claim.
/// Distinct from [`Enforcement`] (the runtime claim a backend makes).
///
/// Serialized to/from the committed qualification ledger (kebab-case) so the
/// integrity coupling gate — which cannot depend on this crate — reads it as data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum QualificationStatus {
    /// The complete contract path PLUS its fail-closed / teardown branches were
    /// observed by an INDEPENDENT oracle (host-side kernel-state preferred over
    /// workload self-report; dual-channel). The ONLY status that permits a
    /// production `Enforced` claim, for the matching key + profile-floor + digest.
    Proven,
    /// Explicitly unsupported on this platform/profile; planning REFUSES before
    /// execution. The fail-closed default for any unproven capability. NOT a
    /// waiver — a platform limitation is `FailClosed`, never `Waived`.
    FailClosed,
    /// Some layers exist (a mechanism, a partial path) but the claimed guarantee
    /// is NOT proven end-to-end. May not be advertised `Enforced` in production.
    Incomplete,
    /// Mechanically impossible to witness, OR deliberately excluded — carried as an
    /// owner-signed, EXPIRING waiver (owner + reason + expiry). NEVER describes a
    /// platform capability limitation.
    Waived,
    /// Test-only support that exists to PROVE detection (a red fixture's planted
    /// lie). Never a production claim.
    FaultInjected,
}

impl QualificationStatus {
    /// The coupling-law predicate: whether this qualification permits a production
    /// [`Enforcement::Enforced`] runtime claim. ONLY [`Self::Proven`] does.
    #[must_use]
    pub fn permits_enforced(self) -> bool {
        matches!(self, Self::Proven)
    }
}

/// The coupling-law check at a single (key, profile, claim) point: a runtime
/// [`Enforcement`] claim is admissible against a qualification only when either the
/// claim is not `Enforced`, or the qualification is `Proven`. (The full gate also
/// matches the profile floor and mechanism digest — those live in the ledger + the
/// integrity gate; this is the pointwise core, kept here so it is unit-testable and
/// shared by the runtime if it ever consults a qualification.)
#[must_use]
pub fn enforced_claim_is_qualified(claim: Enforcement, qualification: QualificationStatus) -> bool {
    match claim {
        Enforcement::Enforced => qualification.permits_enforced(),
        // A backend may always claim a weaker guarantee than it has proven.
        Enforcement::Mediated | Enforcement::Unsupported => true,
    }
}

/// A `blake3` digest of a backend's MECHANISM string for a requirement key (§1's
/// `H_M`). The coupling law binds a production `Enforced` claim to a `Proven`
/// ledger row ONLY when the row's digest matches the digest of the mechanism the
/// backend would actually use — so a backend cannot satisfy the gate by swapping
/// in a different (unproven) mechanism under the same key.
///
/// Derived with [`batpak::event::hash::compute_hash`] (the same blake3 the
/// launcher attestation uses), over the UTF-8 bytes of the mechanism string a
/// backend authors in its `mechanism(req, enforcement)` (e.g.
/// `"linux:landlock:Enforced"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MechanismDigest(pub [u8; 32]);

impl MechanismDigest {
    /// Derive the digest from a backend's mechanism string (the exact bytes a
    /// backend's `mechanism(req, enforcement)` returns, e.g.
    /// `"linux:landlock:Enforced"`).
    #[must_use]
    pub fn of_mechanism(mechanism: &str) -> Self {
        Self(batpak::event::hash::compute_hash(mechanism.as_bytes()))
    }

    /// The lowercase hex spelling of the 32-byte digest (for the committed ledger
    /// + diagnostics). Deterministic, so a ledger row's digest is one stable line.
    #[must_use]
    pub fn to_hex(self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// The MINIMUM machine facts a qualification covers (§3 PROFILE-CLASS): the floor
/// predicate a production profile must DOMINATE (`p_prod ⊒ floor`) for a `Proven`
/// receipt earned at the floor to transfer to it.
///
/// A qualification is earned on ONE probed CI runner, but production runs
/// elsewhere; an exact-profile digest would never transfer. So the floor states
/// the LEAST the mechanism needs, and the load-bearing argument is that proving AT
/// the floor generalizes UPWARD — a stronger machine still satisfies every minimum.
/// `satisfied_by` is exactly that domination check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProfileFloor {
    /// The minimum landlock ABI the mechanism requires (`None` = landlock is not
    /// part of this floor — e.g. a cgroup-only Kill cell does not need landlock).
    pub landlock_abi_min: Option<u8>,
    /// The mechanism requires atomic `cgroup.kill` run-tree teardown.
    pub requires_cgroup_kill: bool,
    /// The mechanism requires the `pids.peak` usage witness (cgroup v2 ≥ 6.1).
    pub requires_pids_peak: bool,
    /// The mechanism requires UNPRIVILEGED user + network namespace creation (the
    /// `NetworkDenyAll` empty-netns floor, S9 / D3): an unprivileged process may create a
    /// new netns ONLY when it is also root in a new userns (the S8 rendezvous). A kernel
    /// that forbids unprivileged userns cannot realize the empty netns, so the cell is
    /// FAIL_CLOSED there — never a silent pass.
    pub requires_unprivileged_userns: bool,
}

impl ProfileFloor {
    /// The empty floor: no minimum machine facts (structural mechanisms that hold
    /// on every machine of the platform — e.g. process spawn / pipe capture).
    #[must_use]
    pub const fn structural() -> Self {
        Self {
            landlock_abi_min: None,
            requires_cgroup_kill: false,
            requires_pids_peak: false,
            requires_unprivileged_userns: false,
        }
    }

    /// The empty-netns `NetworkDenyAll` floor (S9 / D3): requires unprivileged
    /// user + network namespace creation, no landlock / cgroup minimum. Structural
    /// otherwise (the empty netns holds on any kernel that permits unprivileged userns).
    #[must_use]
    pub const fn unprivileged_userns_netns() -> Self {
        Self {
            landlock_abi_min: None,
            requires_cgroup_kill: false,
            requires_pids_peak: false,
            requires_unprivileged_userns: true,
        }
    }

    /// THE §3 DOMINATION CHECK: whether the concrete machine `facts` satisfy every
    /// minimum this floor requires (`facts ⊒ self`). A production profile may
    /// advertise the qualification's key ONLY when this holds — so a receipt earned
    /// at the floor transfers UPWARD to any stronger machine but NEVER downward to a
    /// weaker one.
    #[must_use]
    pub fn satisfied_by(&self, facts: &ProfileFacts) -> bool {
        if let Some(min) = self.landlock_abi_min {
            if facts.landlock_abi < i64::from(min) {
                return false;
            }
        }
        if self.requires_cgroup_kill && !facts.has_cgroup_kill {
            return false;
        }
        if self.requires_pids_peak && !facts.has_pids_peak {
            return false;
        }
        if self.requires_unprivileged_userns && !facts.has_unprivileged_userns {
            return false;
        }
        true
    }
}

/// The concrete, TYPED machine facts a [`ProfileFloor`] is checked against — the
/// probe truths a backend derives once at construction (the live landlock ABI, and
/// whether the kernel exposes atomic `cgroup.kill` / the `pids.peak` witness).
///
/// Distinct from [`crate::contract::support::BackendProfile`] (the per-kind
/// enforcement CEILING): the ceiling is the OUTPUT of these facts, while the floor
/// predicate is stated over the facts THEMSELVES (the ABI integer, the cgroup
/// presence) — a ceiling has no ABI integer to compare a floor against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProfileFacts {
    /// The live landlock ABI integer probed from the kernel (`0` = unavailable).
    pub landlock_abi: i64,
    /// Whether the probed cgroup base exposes atomic `cgroup.kill`.
    pub has_cgroup_kill: bool,
    /// Whether the probed cgroup base exposes the `pids.peak` usage witness.
    pub has_pids_peak: bool,
    /// Whether the host permits UNPRIVILEGED user + network namespace creation (the
    /// `NetworkDenyAll` empty-netns floor, S9 / D3). Probed once at construction
    /// (`unprivileged_userns_available`); `false` ⇒ the empty-netns cell fails closed.
    pub has_unprivileged_userns: bool,
}

/// One committed row of the qualification LEDGER (§1 `Qualification(p,k)` joined to
/// its profile-class + mechanism digest + proof receipt). The ledger is the
/// REPOSITORY's independent record; a backend never authors it.
///
/// A row may carry [`QualificationStatus::Proven`] ONLY if every entry in
/// `proof_receipts` cites a REAL, currently-passing oracle test (`path::fn`) — and
/// together they prove the COMPLETE §4 contract path INCLUDING the fail-closed /
/// teardown branch (a cell's guarantee-holds and its setup-failure ⇒ no-effect are
/// usually distinct oracles). The coupling test enforces the §1 law: every
/// production-`Enforced` ceiling cell must have a `Proven` row here whose
/// [`ProfileFloor`] the running profile satisfies AND whose [`MechanismDigest`]
/// matches the backend's mechanism for that key; the proof-receipt RESOLVER
/// (`coupling_proof.rs`) additionally fails CI if any cited receipt is a ghost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QualificationRow {
    /// The backend family id this qualification was earned for (e.g. `"linux"`).
    pub backend: &'static str,
    /// The requirement key this row qualifies.
    pub key: RequirementKind,
    /// The minimum machine facts the proof generalizes from (§3).
    pub profile_floor: ProfileFloor,
    /// The exact backend MECHANISM STRING this row proves (e.g.
    /// `"linux:landlock:Enforced"`). Stored as the source string — not a
    /// pre-baked digest — because blake3 is not `const`; the coupling test
    /// re-derives [`MechanismDigest::of_mechanism`] from this AND from the
    /// backend's live `mechanism(req, enforcement)` and asserts they MATCH, so a
    /// backend cannot satisfy the gate under a different (unproven) mechanism.
    pub mechanism: &'static str,
    /// What the repository independently qualified for this cell.
    pub status: QualificationStatus,
    /// The proof receipts: the `path::fn`s of the oracle tests that TOGETHER prove
    /// the cell's complete §4 path (guarantee-holds + fail-closed branch). Empty for
    /// non-`Proven` rows. Every entry is machine-resolved to a real `#[test]` by the
    /// resolver gate — a `Proven` row may NOT cite a ghost.
    pub proof_receipts: &'static [&'static str],
}

impl QualificationRow {
    /// The blake3 [`MechanismDigest`] (§1 `H_M`) of this row's committed mechanism
    /// string — the digest the coupling gate matches the backend's live mechanism
    /// against.
    #[must_use]
    pub fn mechanism_digest(&self) -> MechanismDigest {
        MechanismDigest::of_mechanism(self.mechanism)
    }
}

/// Derive the [`MechanismDigest`] for a Linux mechanism string built the way
/// `backend_impl::mechanism` builds it: `"{id}:{primitive}:{enforcement:?}"`. The
/// ledger is committed `const`, but a digest is a runtime blake3, so the ledger
/// stores the SOURCE mechanism string and the coupling test re-derives the digest;
/// this helper keeps the two spellings (ledger vs. backend) in one place.
#[must_use]
pub fn linux_mechanism(primitive: &str, enforcement: Enforcement) -> String {
    format!("linux:{primitive}:{enforcement:?}")
}

/// The COMMITTED Linux qualification ledger (S1): the CURRENTLY-PROVEN cells, each
/// citing its real passing oracle(s), plus the explicitly-stated non-proven cells.
///
/// PROVEN rows cite a real, currently-passing oracle that proves the complete
/// contract path (verified to exist in `crates/bvisor/tests/`):
/// - `Filesystem` — landlock denial proven on-disk by the independent G-grid oracle
///   (above the ABI floor; below it the cell is fail-closed, never advertised).
/// - `Kill` — atomic `cgroup.kill` teardown DRAINS the tree to empty (observed via
///   the bounded `wait_until_empty` poll), paired with the pids cap proof.
/// - `LaunchWorkload` / `CaptureStreams` — the launcher spawns + the host captures
///   the workload's own stdout/stderr cleanly (no launcher contamination).
/// - `process_count` budget — the cgroup `pids.max` cap GENUINELY denies forks past
///   the cap (kernel `pids.events max` counter), witnessed from `pids.peak`.
///   (process_count is a budget dimension, not a `RequirementKind`; it rides the
///   `Kill` cell's cgroup floor, so it is documented here, not a separate row.)
///
/// `Environment` (explicit envp) — the admitted `Environment::Exact` policy is lowered
/// to the launcher's explicit env (literals plus parent-resolved secret leases); the
/// child env EQUALS the admitted table EXACTLY, witnessed by the host reading the
/// child's `proc` environ AND the workload's own self-report (dual channel, no ambient
/// leak), with the fail-closed branches (an unresolvable lease means the target never
/// runs; an invalid policy means admission refuses) proven by the same oracle.
///
/// `InheritedFdsNone` (fd-scrub) — the admitted `FdPolicy::None` drives the launcher's
/// child-side fd-scrub; the child's open fds (read HOST-SIDE from `/proc/<pid>/fd`)
/// contain ONLY the declared allowlist, a parent-opened non-CLOEXEC sentinel fd is ABSENT
/// (scrubbed), with the fail-closed branches (an undeclared fd is scrubbed before the
/// workload; an unrealized fd policy ⇒ the target never runs) proven by the oracle.
///
/// `NetworkDenyAll` (empty netns, S9 / D3) — the admitted `NetPolicy::DenyAll` engages a
/// NEW, EMPTY network namespace (`CLONE_NEWNET`, alongside the S8 userns rendezvous it
/// requires); the netns has NO external interface, witnessed HOST-SIDE (the host reads the
/// CHILD's `/proc/<pid>/net/dev` and sees ONLY `lo`) AND by the workload's own self-report
/// (it cannot reach the network), with the launcher's own control channel still working
/// through the netns (HostControl carve-out — fd-passed sockets are unaffected). Fail-closed
/// branches: a kernel without unprivileged userns+netns ⇒ the cell SKIPs LOUD (floor not
/// met), and an unrealized `AllowList` ⇒ the target never runs. `NetworkAllowList` STAYS
/// `FailClosed` (no broker in v1).
///
/// NON-PROVEN cells are stated explicitly (the coupling test asserts they are NOT
/// advertised `Enforced` in production): `InheritedFdsOnly` is `Incomplete` (the scrub
/// realizes only `None`; the selective-keep allowlist has no lowering + no oracle); every
/// other capability — including `NetworkAllowList` (no broker in v1) and all THREE
/// `ChildSpawn` child-task keys (proof-spine §2 split + the S6 3-variant freeze) — is
/// `FailClosed`.
pub const LINUX_QUALIFICATION_LEDGER: &[QualificationRow] = &[
    QualificationRow {
        backend: "linux",
        key: RequirementKind::Filesystem,
        // Landlock ABI v1 already enforces path-beneath read/write/execute — the
        // backend's floor. The proof transfers to any higher ABI.
        profile_floor: ProfileFloor {
            landlock_abi_min: Some(1),
            requires_cgroup_kill: false,
            requires_pids_peak: false,
            requires_unprivileged_userns: false,
        },
        mechanism: "linux:landlock:Enforced",
        status: QualificationStatus::Proven,
        // Guarantee-holds: independent on-disk G-grid denial oracle. Fail-closed:
        // below the ABI floor the cell drops from the ceiling (the coupling test
        // `below_floor_profile_drops_filesystem_from_the_ceiling` proves it).
        proof_receipts: &[
            "crates/bvisor/tests/grid_linux_fs.rs::g1_landlock_denies_secret_read_outside_declared_root",
        ],
    },
    QualificationRow {
        backend: "linux",
        key: RequirementKind::LaunchWorkload,
        profile_floor: ProfileFloor::structural(),
        mechanism: "linux:process_spawn:Enforced",
        status: QualificationStatus::Proven,
        // §4 BOTH branches: guarantee-holds (a workload launches + runs to a verdict)
        // AND fail-closed (a setup problem REFUSES before any child — the target
        // never runs).
        proof_receipts: &[
            "crates/bvisor/tests/launcher_capture_linux.rs::launcher_captures_workload_streams_cleanly_and_deterministically",
            "crates/bvisor/tests/launcher_skeleton_linux.rs::missing_primitive_refuses_before_any_child",
        ],
    },
    QualificationRow {
        backend: "linux",
        key: RequirementKind::CaptureStreams,
        profile_floor: ProfileFloor::structural(),
        mechanism: "linux:pipe_capture:Enforced",
        status: QualificationStatus::Proven,
        proof_receipts: &[
            "crates/bvisor/tests/launcher_capture_linux.rs::launcher_captures_workload_streams_cleanly_and_deterministically",
        ],
    },
    QualificationRow {
        backend: "linux",
        key: RequirementKind::Kill,
        // Kill rides cgroup v2 atomic `cgroup.kill`; no landlock minimum.
        profile_floor: ProfileFloor {
            landlock_abi_min: None,
            requires_cgroup_kill: true,
            requires_pids_peak: false,
            requires_unprivileged_userns: false,
        },
        mechanism: "linux:cgroup_kill:Enforced",
        status: QualificationStatus::Proven,
        // §4: host-state oracle (kernel pids.events) + workload exit (dual channel)
        // AND teardown observed (cgroup.kill drains the tree to empty).
        proof_receipts: &[
            "crates/bvisor/tests/cgroup_enforcement_linux.rs::pids_max_genuinely_denies_forks_past_the_cap_or_explicit_skip",
        ],
    },
    // Non-proven cells, status stated explicitly. These keys are absent from the
    // production ceiling, so the coupling test never demands a Proven row for them;
    // they are listed so the ledger states the qualification status explicitly. The
    // mechanism string for an unimplemented cell mirrors `backend_impl::mechanism`'s
    // `"none/unimplemented-this-chunk"` primitive (Unsupported in the ceiling).
    QualificationRow {
        backend: "linux",
        key: RequirementKind::Environment,
        // Explicit-envp lowering is structural (no kernel-version floor): the launcher
        // serves the admitted env to fexecve on any Linux. The proof transfers to every
        // machine of the platform.
        profile_floor: ProfileFloor::structural(),
        mechanism: "linux:explicit_env:Enforced",
        status: QualificationStatus::Proven,
        // §4 BOTH branches, dual-channel:
        //  - guarantee-holds: the child's env EQUALS the admitted table EXACTLY,
        //    witnessed by the HOST reading /proc/<child_pid>/environ (kernel state,
        //    independent) AND the workload's own reported env; a parent sentinel var is
        //    ABSENT in the child (no ambient leak); a SecretLease resolves IN THE CHILD
        //    while the serialized plan+report carry only the REF, never the value;
        //  - fail-closed: an unresolvable lease ⇒ the target NEVER runs (no child
        //    output), and a contract-invalid policy ⇒ admission refuses before execution.
        proof_receipts: &[
            "crates/bvisor/tests/env_exact_linux.rs::child_env_equals_the_admitted_table_with_no_ambient_leak",
            "crates/bvisor/tests/env_exact_linux.rs::an_unresolvable_lease_fails_closed_and_the_target_never_runs",
            "crates/bvisor/tests/env_exact_linux.rs::a_contract_invalid_policy_is_refused_before_execution",
            // The full execute()/BoundaryRunner contract-path witness (vs the launcher-
            // direct /proc oracle above): the durable plan+report carry only the lease ref.
            "crates/bvisor/tests/env_exact_linux.rs::a_secret_lease_resolves_but_the_durable_plan_and_report_carry_only_the_ref",
        ],
    },
    QualificationRow {
        backend: "linux",
        key: RequirementKind::InheritedFdsNone,
        // The fd-scrub is structural (no kernel-version floor): the launcher reads
        // /proc/self/fd + raw SYS_close on any Linux. The proof transfers to every
        // machine of the platform.
        profile_floor: ProfileFloor::structural(),
        mechanism: "linux:fd_scrub:Enforced",
        status: QualificationStatus::Proven,
        // §4 BOTH branches, dual-channel:
        //  - guarantee-holds: the child's open fds (read HOST-SIDE from
        //    /proc/<child_pid>/fd — kernel state, independent) contain ONLY the declared
        //    allowlist (stdio); a parent-opened non-CLOEXEC SENTINEL fd is ABSENT from the
        //    child (scrubbed), witnessed both host-side AND by the workload's own attempt
        //    to write to it failing (no leak across the boundary);
        //  - fail-closed: an undeclared inherited fd is scrubbed BEFORE the workload (the
        //    launcher mechanism proof), AND a contract-level setup failure ⇒ the target
        //    NEVER runs (the full execute()/BoundaryRunner path refuses an unrealized fd
        //    policy). The execute()-path witness proves the lowering rides the production
        //    contract, not only a run_launcher-direct plan.
        proof_receipts: &[
            "crates/bvisor/tests/inherited_fds_none_linux.rs::child_inherits_only_the_declared_fds_no_sentinel_leak",
            "crates/bvisor/tests/launcher_inherited_fds_linux.rs::undeclared_inherited_fd_is_scrubbed_before_the_workload",
            "crates/bvisor/tests/inherited_fds_none_linux.rs::an_unrealized_fd_policy_fails_closed_and_the_target_never_runs",
            // The full execute()/BoundaryRunner contract-path witness (vs the launcher-
            // direct /proc oracle above): a None-policy spec runs to a clean verdict.
            "crates/bvisor/tests/inherited_fds_none_linux.rs::a_none_policy_spec_runs_through_the_execute_path",
        ],
    },
    QualificationRow {
        backend: "linux",
        key: RequirementKind::InheritedFdsOnly,
        profile_floor: ProfileFloor::structural(),
        mechanism: "linux:none/unimplemented-this-chunk:Unsupported",
        status: QualificationStatus::Incomplete,
        proof_receipts: &[],
    },
    QualificationRow {
        backend: "linux",
        key: RequirementKind::NetworkDenyAll,
        // The empty-netns floor (S9 / D3): requires UNPRIVILEGED user + network namespace
        // creation (the S8 rendezvous makes the child root-in-userns; CLONE_NEWNET then
        // births it into an empty netns). No landlock/cgroup minimum — the empty netns is
        // structural above that floor, so the proof transfers to any kernel that permits
        // unprivileged userns. Below the floor the cell drops from the ceiling (fail-closed).
        profile_floor: ProfileFloor::unprivileged_userns_netns(),
        mechanism: "linux:empty_netns:Enforced",
        status: QualificationStatus::Proven,
        // §4 BOTH branches, DUAL channel (host-side kernel-state strongest, per §4):
        //  - guarantee-holds (A) HOST-SIDE: the host reads the CHILD's netns interface list
        //    from /proc/<child_pid>/net/dev and asserts it contains ONLY `lo` — NO external
        //    interface (the independent "zero external interfaces" witness, kernel state the
        //    launcher cannot forge); (B) WORKLOAD self-report: the workload enumerates its
        //    own interfaces + routing table and OBSERVES it has ZERO routes (only `lo`, which
        //    has no address ⇒ no reachable destination). NO-LEAK: no inherited routable socket
        //    survives (the S5 scrub).
        //    HOSTCONTROL: the launcher's own control channel still works through the netns
        //    (the workload runs to a verdict);
        //  - fail-closed: a kernel without unprivileged userns+netns ⇒ the cell SKIPs LOUD
        //    (never a silent pass), and a contract-level setup failure ⇒ the target never runs
        //    (the empty netns engages on the full execute()/BoundaryRunner path).
        proof_receipts: &[
            "crates/bvisor/tests/network_deny_all_linux.rs::host_sees_only_loopback_in_the_child_netns_no_external_interface_or_skip",
            "crates/bvisor/tests/network_deny_all_linux.rs::workload_cannot_reach_the_network_from_the_empty_netns_or_skip",
            "crates/bvisor/tests/network_deny_all_linux.rs::a_deny_all_spec_runs_through_the_execute_path_or_skip",
            "crates/bvisor/tests/network_deny_all_linux.rs::network_allow_list_fails_closed_at_admission_the_target_never_runs",
        ],
    },
    QualificationRow {
        backend: "linux",
        key: RequirementKind::NetworkAllowList,
        profile_floor: ProfileFloor::structural(),
        mechanism: "linux:none/unimplemented-this-chunk:Unsupported",
        status: QualificationStatus::FailClosed,
        proof_receipts: &[],
    },
    // The three FROZEN child-task semantics (proof-spine S6). S6 freezes the
    // taxonomy + the clone3-pointer/classic-BPF enforcement CONSTRAINT (see
    // `SpawnPolicy`); it does NOT implement enforcement (that is S10, seccomp +
    // cgroup). So ALL THREE stay FailClosed here with NO oracle (empty receipts) —
    // none is advertised Enforced in the production ceiling, so the coupling gate
    // never demands a Proven row for them.
    QualificationRow {
        backend: "linux",
        key: RequirementKind::ChildSpawnDenyNewTasks,
        profile_floor: ProfileFloor::structural(),
        mechanism: "linux:none/unimplemented-this-chunk:Unsupported",
        status: QualificationStatus::FailClosed,
        proof_receipts: &[],
    },
    QualificationRow {
        backend: "linux",
        key: RequirementKind::ChildSpawnAllowThreads,
        profile_floor: ProfileFloor::structural(),
        mechanism: "linux:none/unimplemented-this-chunk:Unsupported",
        status: QualificationStatus::FailClosed,
        proof_receipts: &[],
    },
    QualificationRow {
        backend: "linux",
        key: RequirementKind::ChildSpawnAllowDescendants,
        profile_floor: ProfileFloor::structural(),
        mechanism: "linux:none/unimplemented-this-chunk:Unsupported",
        status: QualificationStatus::FailClosed,
        proof_receipts: &[],
    },
];

/// Look up the committed Linux ledger row for a requirement key (the lookup the
/// coupling gate uses). `None` ⇒ the key is not in the ledger at all (which, for an
/// `Enforced` production cell, is itself a coupling violation).
#[must_use]
pub fn linux_ledger_row(key: RequirementKind) -> Option<&'static QualificationRow> {
    LINUX_QUALIFICATION_LEDGER.iter().find(|r| r.key == key)
}

#[cfg(test)]
#[path = "qualification_tests.rs"]
mod tests;
