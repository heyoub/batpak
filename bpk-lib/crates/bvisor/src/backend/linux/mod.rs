//! Linux backend — landlock + cgroup-v2 + launcher-process confinement.
//!
//! The HONEST per-platform [`SupportMatrix`] (pure data, always-compiled,
//! cross-platform unit-testable) plus a [`LinuxBackend`] whose `execute()` runs the
//! workload through the confinement launcher (a clone3 child: fd-scrub → landlock
//! `restrict_self` → cgroup placement → `fexecve`). The unsafe OS code is quarantined
//! to the [`super::linux::sys`] basement + the `launcher/linux/` binary; this module's
//! orchestration is SAFE. The machine ceiling (`profile()`) advertises ONLY what
//! `execute()` genuinely backs with a real syscall — Filesystem (landlock above the
//! ABI floor), LaunchWorkload + CaptureStreams (always), Kill + process_count (with a
//! cgroup base). Everything else floors to Unsupported (see `backend_impl`).
//!
//! HONESTY (SCOPE §4 — Linux): the family `support_matrix()` below is the ASPIRATION
//! table (what the platform COULD enforce), independent of this build's machine
//! ceiling; the mechanism notes on its cells describe the INTENDED mechanism, not
//! necessarily one `execute()` implements yet. `NetworkAllowList` is
//! [`Enforcement::Unsupported`] in v1 (it needs a broker that does not exist yet);
//! claiming otherwise would be a lie the gauntlet must catch. `Kill` is Enforced
//! (cgroup-v2 `cgroup.kill` + pidfd), `Filesystem` Enforced (landlock, fails
//! closed below the ABI floor).

use crate::contract::capability::{Enforcement, EvidenceClaim, SupportVerdict};
use crate::contract::support::{RequirementKind, SupportMatrix};
use std::collections::BTreeMap;

/// The HONEST Linux family support matrix (SCOPE §4). Pure data — constructible
/// and unit-testable on ANY host, so the honesty is provable off-Linux.
///
/// Every [`RequirementKind`] NOT listed here defaults to the fail-closed bottom
/// ([`SupportVerdict::unsupported`]); `NetworkAllowList` is DELIBERATELY listed as
/// `Unsupported` (v1, no broker) so the absence is a stated answer, not an omission.
#[must_use]
pub fn support_matrix() -> SupportMatrix {
    let mut best = BTreeMap::new();

    // Launch + stdio: structural, always available.
    insert(
        &mut best,
        RequirementKind::LaunchWorkload,
        Enforcement::Enforced,
        &[EvidenceClaim::TerminalOutcome, EvidenceClaim::ProcessTree],
    );
    insert(
        &mut best,
        RequirementKind::CaptureStreams,
        Enforcement::Enforced,
        &[EvidenceClaim::CapturedStreams],
    );

    // Filesystem confinement: landlock (fails closed below ABI floor at probe()).
    insert(
        &mut best,
        RequirementKind::Filesystem,
        Enforcement::Enforced,
        &[
            EvidenceClaim::AllowedActions,
            EvidenceClaim::DeniedAttempts,
            EvidenceClaim::FilesystemDelta,
            EvidenceClaim::MechanismAttestation,
        ],
    );

    // Network: deny-all is Enforced (drop CAP_NET / empty net namespace).
    insert(
        &mut best,
        RequirementKind::NetworkDenyAll,
        Enforcement::Enforced,
        &[EvidenceClaim::DeniedAttempts],
    );
    // NetworkAllowList: UNSUPPORTED in v1 — needs a broker that does not exist yet.
    // Load-bearing honest fail-closed cell (NEVER fake a broker to fill the table).
    insert(
        &mut best,
        RequirementKind::NetworkAllowList,
        Enforcement::Unsupported,
        &[],
    );

    // Child spawn / env / fds: clone3 + namespaces. ChildSpawn + InheritedFds carry
    // distinct CANONICAL POLICIES (proof-spine §2), so each policy variant is its own
    // aspiration cell.
    insert(
        &mut best,
        RequirementKind::ChildSpawnDeny,
        Enforcement::Enforced,
        &[EvidenceClaim::ProcessTree],
    );
    insert(
        &mut best,
        RequirementKind::ChildSpawnAllow,
        Enforcement::Enforced,
        &[EvidenceClaim::ProcessTree],
    );
    insert(
        &mut best,
        RequirementKind::Environment,
        Enforcement::Enforced,
        &[EvidenceClaim::MechanismAttestation],
    );
    insert(
        &mut best,
        RequirementKind::InheritedFdsNone,
        Enforcement::Enforced,
        &[EvidenceClaim::MechanismAttestation],
    );
    insert(
        &mut best,
        RequirementKind::InheritedFdsOnly,
        Enforcement::Enforced,
        &[EvidenceClaim::MechanismAttestation],
    );

    // Host controls: temp root + expose-path (bind mount) + artifact commit/discard.
    insert(
        &mut best,
        RequirementKind::TempRoot,
        Enforcement::Enforced,
        &[EvidenceClaim::MechanismAttestation],
    );
    insert(
        &mut best,
        RequirementKind::ExposePath,
        Enforcement::Enforced,
        &[EvidenceClaim::MechanismAttestation],
    );
    insert(
        &mut best,
        RequirementKind::CommitArtifact,
        Enforcement::Enforced,
        &[EvidenceClaim::ArtifactLineage],
    );
    insert(
        &mut best,
        RequirementKind::DiscardArtifact,
        Enforcement::Enforced,
        &[EvidenceClaim::ArtifactLineage],
    );
    insert(
        &mut best,
        RequirementKind::ListOutputs,
        Enforcement::Enforced,
        &[EvidenceClaim::ArtifactLineage],
    );

    // Kill: cgroup-v2 cgroup.kill + pidfd = atomic subtree teardown.
    insert(
        &mut best,
        RequirementKind::Kill,
        Enforcement::Enforced,
        &[EvidenceClaim::ProcessTree, EvidenceClaim::TerminalOutcome],
    );

    SupportMatrix::from_best_case(best)
}

/// Insert one best-case verdict into the table.
fn insert(
    table: &mut BTreeMap<RequirementKind, SupportVerdict>,
    kind: RequirementKind,
    enforcement: Enforcement,
    evidence: &[EvidenceClaim],
) {
    table.insert(
        kind,
        SupportVerdict::new(enforcement, evidence.iter().copied().collect()),
    );
}

// The FROZEN host↔launcher wire protocol (kernel plan §10.8). PURE library types
// (serde + canonical encode/decode + state-machine checker) — no OS code — so it
// compiles on ANY host with the feature, ready for a later launcher `[[bin]]` to
// `use`. Gated OFF by default, so the default public surface is unaffected.
#[cfg(feature = "backend-linux")]
pub mod protocol;

// The real backend (probe/profile/execute touch the OS) compiles ONLY on a Linux
// host with the feature enabled; the honest table above is always present.
#[cfg(all(feature = "backend-linux", target_os = "linux"))]
mod backend_impl;
#[cfg(all(feature = "backend-linux", target_os = "linux"))]
pub use backend_impl::LinuxBackend;

// HOST-SIDE launcher-plan construction (descriptor table + lowering schedule +
// authority handles), split out of `backend_impl` to keep each production file under
// the structural-check size cap. SAFE std (`File::open`) — no OS confinement here.
#[cfg(all(feature = "backend-linux", target_os = "linux"))]
mod plan_build;

// Per-run cgroup lifecycle helpers (pids cap / required-backing / create / teardown),
// split out of `backend_impl` to keep each production file under the size cap. PURE of
// any `LinuxBackend` private state; SAFE `std::fs`, no `unsafe`.
#[cfg(all(feature = "backend-linux", target_os = "linux"))]
mod cgroup_run;

#[cfg(all(feature = "backend-linux", target_os = "linux"))]
pub(crate) mod sys;

// SAFE host-side cgroup v2 manager (kernel plan §10.8, step 8a): create/configure a
// leaf cgroup, set pids.max/memory.max for delegated controllers only, read
// cgroup.procs, and atomic cgroup.kill teardown, plus a delegation probe. cgroup v2
// is a FILESYSTEM interface, so this is ALL safe `std::fs` — NO `unsafe`, fully
// runtime-shape-checked, unit-testable against a fake tree. 8b adds the launcher's
// CLONE_INTO_CGROUP placement + the Budget/Kill profile() honesty cells.
#[cfg(all(feature = "backend-linux", target_os = "linux"))]
pub mod cgroup;

// The HOST-SIDE launcher harness (kernel plan §10.8, step 7a): a REUSABLE, SAFE
// orchestration that seals a launcher plan into a memfd, spawns the confinement
// launcher with controlled inherited fds, and collects its transcript/outcome. Step
// 7b wires this into `execute()`. SAFE except the two ledgered `sys` basement calls
// (memfd seal + spawn pre_exec). Compiled only on Linux with the backend feature.
#[cfg(all(feature = "backend-linux", target_os = "linux"))]
pub mod launch;

#[cfg(test)]
mod tests {
    use super::support_matrix;
    use crate::contract::capability::Enforcement;
    use crate::contract::support::RequirementKind;

    #[test]
    fn network_allow_list_is_unsupported_in_v1() {
        // SCOPE §4 load-bearing honest cell: no broker yet ⇒ Unsupported.
        let m = super::support_matrix();
        let v = m.best_case_for(RequirementKind::NetworkAllowList);
        assert_eq!(v.enforcement, Enforcement::Unsupported);
    }

    #[test]
    fn filesystem_and_kill_are_enforced() {
        let m = support_matrix();
        assert_eq!(
            m.best_case_for(RequirementKind::Filesystem).enforcement,
            Enforcement::Enforced
        );
        assert_eq!(
            m.best_case_for(RequirementKind::Kill).enforcement,
            Enforcement::Enforced
        );
    }
}
