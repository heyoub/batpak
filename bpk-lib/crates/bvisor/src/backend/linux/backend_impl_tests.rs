//! Unit tests for the Linux backend ceiling/budget honesty (split out of
//! `backend_impl.rs` to keep the production file under the non-overridable
//! structural-check size cap). `super` is the `backend_impl` module.

use super::{LinuxBackend, LANDLOCK_ABI_FLOOR};
use crate::contract::backend::Backend;
use crate::contract::capability::{Capability, Enforcement, FsAccess, FsConfinement, PathSet};
use crate::contract::plan::BoundaryRequirement;
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
        RequirementKind::ChildSpawn,
        RequirementKind::TempRoot,
    ] {
        assert_eq!(
            profile.ceiling_for(kind).enforcement,
            Enforcement::Unsupported,
            "{kind:?} must stay Unsupported until its chunk lands (no inflation)"
        );
    }
}

#[test]
fn kill_is_enforced_with_a_cgroup_base_and_unsupported_without() {
    // WITH a probed cgroup base (atomic cgroup.kill): Kill{RunTree,Atomic} is
    // Enforced — the workload runs in a cgroup leaf the host can SIGKILL atomically.
    let with = LinuxBackend::with_cgroup_for_test();
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
    // WITH a cgroup base: process_count is a structural cgroup pids.max cap ⇒
    // Enforced; the witness reads pids.peak (ResourceUsage evidence).
    let with = LinuxBackend::with_cgroup_for_test();
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
