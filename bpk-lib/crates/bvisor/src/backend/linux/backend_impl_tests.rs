//! Unit tests for the Linux backend ceiling/budget honesty (split out of
//! `backend_impl.rs` to keep the production file under the non-overridable
//! structural-check size cap). `super` is the `backend_impl` module.

use super::{LinuxBackend, LANDLOCK_ABI_FLOOR};
use crate::backend::linux::cgroup_run::{cgroup_for_run, pids_cap_for, requires_cgroup_backing};
use crate::contract::backend::Backend;
use crate::contract::budget::{budget_admit, BudgetRequirements, DerivedMinimums, MinGuarantee};
use crate::contract::capability::{
    Capability, Enforcement, EvidenceClaim, EvidenceSet, FsAccess, FsConfinement, PathSet,
};
use crate::contract::ids::{BackendId, BoundaryPlanHash};
use crate::contract::plan::{
    BoundaryPlan, BoundaryRequirement, EvidenceRequirements, Workload, BOUNDARY_PLAN_SCHEMA_VERSION,
};
use crate::contract::support::RequirementKind;

fn fs_requirement() -> BoundaryRequirement {
    BoundaryRequirement::Capability(Capability::Filesystem {
        access: FsAccess::Read,
        // An inert scope path — classify() never touches disk, so the value is
        // immaterial; a relative placeholder avoids leaking an absolute path.
        scope: PathSet {
            roots: vec!["quarantine/root".to_string()],
        },
        recursive: true,
        confinement: FsConfinement::DeclaredRootsOnly,
    })
}

#[test]
fn filesystem_is_enforced_at_or_above_the_abi_floor() {
    // At the floor the machine ceiling backs Filesystem, so classify is Enforced.
    let backend = LinuxBackend::with_abi_for_test(LANDLOCK_ABI_FLOOR);
    let profile = backend.profile(&backend.probe());
    let verdict = backend.classify(&fs_requirement(), &profile);
    assert_eq!(
        verdict.enforcement,
        Enforcement::Enforced,
        "at/above the ABI floor, Filesystem must be Enforced"
    );
    // The ceiling lists Filesystem at the floor.
    assert_eq!(
        profile.ceiling_for(RequirementKind::Filesystem).enforcement,
        Enforcement::Enforced
    );
}

#[test]
fn filesystem_fails_closed_below_the_abi_floor() {
    // Below the floor (e.g. landlock unavailable ⇒ probed ABI 0) the machine
    // ceiling does NOT back Filesystem, so the family Enforced best-case is
    // floored to Unsupported — and plan() will refuse a FS spec fail-closed.
    let backend = LinuxBackend::with_abi_for_test(LANDLOCK_ABI_FLOOR - 1);
    let profile = backend.profile(&backend.probe());
    let verdict = backend.classify(&fs_requirement(), &profile);
    assert_eq!(
        verdict.enforcement,
        Enforcement::Unsupported,
        "below the ABI floor, Filesystem MUST fail closed (no unbacked guarantee)"
    );
}

#[test]
fn unimplemented_kinds_fail_closed_this_chunk() {
    // HONESTY: this chunk backs Filesystem/LaunchWorkload/CaptureStreams (+ Kill
    // ONLY with a cgroup base — see the dedicated test). NetworkDenyAll / ChildSpawn
    // / TempRoot are NOT in the ceiling, so they floor to Unsupported and plan()
    // fails closed for them.
    let backend = LinuxBackend::with_abi_for_test(LANDLOCK_ABI_FLOOR);
    let profile = backend.profile(&backend.probe());
    for kind in [
        RequirementKind::NetworkDenyAll,
        RequirementKind::ChildSpawnDeny,
        RequirementKind::ChildSpawnAllow,
        RequirementKind::TempRoot,
        // Environment + InheritedFds were backed out of the ceiling after a codex
        // review: even with policy-AWARE keys (proof-spine §2 splits InheritedFds into
        // None/Only), the launcher only realises one policy shape and never lowers the
        // admitted policy, so Enforced would over-admit unimplemented policy variants.
        RequirementKind::Environment,
        RequirementKind::InheritedFdsNone,
        RequirementKind::InheritedFdsOnly,
    ] {
        assert_eq!(
            profile.ceiling_for(kind).enforcement,
            Enforcement::Unsupported,
            "{kind:?} must stay Unsupported until its chunk lands (no inflation)"
        );
    }
}

// (Environment + InheritedFds positive ceiling tests were removed when those cells
// were backed out of the ceiling after a codex review — see
// unimplemented_kinds_fail_closed_this_chunk. The launcher MECHANISM proofs live in
// tests/launcher_env_linux.rs + tests/launcher_inherited_fds_linux.rs.)

#[test]
fn kill_is_enforced_with_a_cgroup_base_and_unsupported_without() {
    // WITH a probed cgroup base (atomic cgroup.kill): Kill{RunTree,Atomic} is
    // Enforced — the workload runs in a cgroup leaf the host can SIGKILL atomically.
    let with = LinuxBackend::with_cgroup_for_test(true);
    assert_eq!(
        with.profile(&with.probe())
            .ceiling_for(RequirementKind::Kill)
            .enforcement,
        Enforcement::Enforced,
        "a cgroup base with atomic cgroup.kill backs Kill Enforced"
    );
    // WITHOUT a cgroup base: Kill is absent from the ceiling ⇒ Unsupported ⇒ plan()
    // fails closed for a kill spec (NO unbacked atomic-kill guarantee — no inflation).
    let without = LinuxBackend::with_abi_for_test(LANDLOCK_ABI_FLOOR);
    assert_eq!(
        without
            .profile(&without.probe())
            .ceiling_for(RequirementKind::Kill)
            .enforcement,
        Enforcement::Unsupported,
        "no cgroup base ⇒ Kill Unsupported (no unbacked atomic-kill guarantee)"
    );
}

#[test]
fn process_count_budget_is_enforced_only_with_a_cgroup_base() {
    // WITH a cgroup base: process_count is a structural cgroup pids.max cap ⇒ Enforced.
    let with = LinuxBackend::with_cgroup_for_test(true);
    let with_budget = with.probe().budget;
    assert_eq!(
        with_budget.process_count.enforcement,
        Enforcement::Enforced,
        "a cgroup base backs process_count Enforced (pids.max)"
    );
    // But NO other dimension is over-claimed — they stay Mediated (observed-not-capped)
    // even with cgroup, because no cap is installed for them this step.
    assert_eq!(
        with_budget.cpu_micros.enforcement,
        Enforcement::Mediated,
        "cpu is NOT capped — Mediated, never an over-claim"
    );
    assert_eq!(
        with_budget.resident_bytes.enforcement,
        Enforcement::Mediated
    );
    // WITHOUT a cgroup base: process_count falls back to Mediated (no unbacked cap).
    let without = LinuxBackend::with_abi_for_test(LANDLOCK_ABI_FLOOR);
    assert_eq!(
        without.probe().budget.process_count.enforcement,
        Enforcement::Mediated,
        "no cgroup base ⇒ process_count Mediated (no unbacked pids cap)"
    );
}

#[test]
fn resource_usage_evidence_is_advertised_only_when_pids_peak_is_witnessable() {
    // The codex-review split: the Hard pids.max CAP (enforcement) is independent of the
    // pids.peak WITNESS (evidence). With peak present ⇒ ResourceUsage advertised; on a
    // kernel that caps pids but has no pids.peak (≥4.3 cap, <6.1 peak) ⇒ NO ResourceUsage
    // evidence, so a plan requiring that witness never admits on a kernel that can't deliver.
    let resource_usage: EvidenceSet = [EvidenceClaim::ResourceUsage].into_iter().collect();

    let with_peak = LinuxBackend::with_cgroup_for_test(true).probe().budget;
    assert_eq!(
        with_peak.process_count.enforcement,
        Enforcement::Enforced,
        "the pids.max cap is Enforced regardless of the peak witness"
    );
    assert_eq!(
        with_peak.process_count.evidence, resource_usage,
        "pids.peak present ⇒ ResourceUsage evidence advertised"
    );

    let no_peak = LinuxBackend::with_cgroup_for_test(false).probe().budget;
    assert_eq!(
        no_peak.process_count.enforcement,
        Enforcement::Enforced,
        "the cap stays Enforced even without the peak witness (cap != witness)"
    );
    assert_eq!(
        no_peak.process_count.evidence,
        EvidenceSet::new(),
        "no pids.peak ⇒ NO ResourceUsage evidence (no over-claim of an absent witness)"
    );
}

/// A `BoundaryPlan` whose process_count budget admitted `Enforced` against `backend`'s
/// probed (cgroup-backed) profile — i.e. a plan that REQUIRES cgroup backing. Built by
/// hand (not the planner) so the fail-closed unit test stays self-contained.
fn cgroup_required_plan(backend: &LinuxBackend) -> BoundaryPlan {
    let snap = backend.probe();
    let budgets = budget_admit(
        &BudgetRequirements::uniform(8, MinGuarantee::Mediated),
        &snap.budget,
        &DerivedMinimums::default(),
        [0u8; 32],
    )
    .expect("budgets admit against the cgroup-backed profile");
    BoundaryPlan {
        schema_version: BOUNDARY_PLAN_SCHEMA_VERSION,
        plan_id: BoundaryPlanHash([0u8; 32]),
        backend: BackendId::new(LinuxBackend::ID),
        profile: snap,
        admitted: Vec::new(),
        workload: Workload::Process {
            exe: "/bin/true".to_string(),
            args: Vec::new(),
        },
        budgets,
        evidence: EvidenceRequirements::default(),
    }
}

#[test]
fn cgroup_for_run_fails_closed_when_a_required_leaf_cannot_be_created() {
    // The HIGH codex finding: a plan admitted with cgroup-backed guarantees must NOT run
    // uncgrouped if the per-run leaf can't be created. `with_cgroup_for_test` advertises a
    // cgroup base (so process_count admits Enforced) but its base is a non-creatable
    // placeholder, so the real leaf creation FAILS — `cgroup_for_run` must fail CLOSED.
    let backend = LinuxBackend::with_cgroup_for_test(true);
    let plan = cgroup_required_plan(&backend);
    assert!(
        requires_cgroup_backing(&plan) && pids_cap_for(&plan).is_some(),
        "the hand-built plan must genuinely require cgroup backing (Enforced process_count)"
    );

    // The fail-closed path returns Err(observed) — the caller (execute) maps it to a
    // SupervisorFault report. Assert the refusal happened and recorded WHY.
    let observed = cgroup_for_run(&backend, &plan, Vec::new()).expect_err(
        "a required-but-uncreatable cgroup leaf MUST fail closed, never run uncgrouped",
    );
    assert!(
        observed
            .iter()
            .any(|f| f.kind == "cgroup_required_but_unavailable"),
        "the refusal must record WHY it failed closed: {observed:?}"
    );
}
