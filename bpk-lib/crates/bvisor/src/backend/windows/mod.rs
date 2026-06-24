//! Windows backend — AppContainer + Job Object confinement (scaffolding).
//!
//! STEP (a) scaffolding: the HONEST per-platform [`SupportMatrix`] (pure data,
//! always-compiled, cross-platform unit-testable) plus a [`WindowsBackend`]
//! struct whose `execute()` is a stub returning [`Outcome::Unsupported`]. Real
//! syscalls (AppContainer/LowBox token, Job Object teardown, WFP) land in step
//! (d), in the [`super::windows::sys`] unsafe basement (`windows-sys` FFI). NO
//! `unsafe` here.
//!
//! HONESTY (SCOPE §4 — Windows): mostly Enforced (AppContainer + Job Object).
//! `ExposePath`(Mount) is [`Enforcement::Mediated`] (no first-class bind mount;
//! mediated via symlink/junction shims) — and `Unsupported` for the private-view
//! guarantee. `NetworkAllowList` is `Mediated` (WFP filters per-attempt, not a
//! structural guarantee). These are load-bearing honest cells.

use crate::contract::capability::{Enforcement, EvidenceClaim, SupportVerdict};
use crate::contract::support::{RequirementKind, SupportMatrix};
use std::collections::BTreeMap;

/// The HONEST Windows family support matrix (SCOPE §4). Pure data — constructible
/// and unit-testable on any host.
#[must_use]
pub fn support_matrix() -> SupportMatrix {
    let mut best = BTreeMap::new();

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

    // FS via AppContainer capability SIDs + DACLs.
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

    // Deny-all net: AppContainer with no network capability SID.
    insert(
        &mut best,
        RequirementKind::NetworkDenyAll,
        Enforcement::Enforced,
        &[EvidenceClaim::DeniedAttempts],
    );
    // NetworkAllowList: MEDIATED via WFP (per-attempt filter, not structural).
    insert(
        &mut best,
        RequirementKind::NetworkAllowList,
        Enforcement::Mediated,
        &[
            EvidenceClaim::NetworkActivity,
            EvidenceClaim::DeniedAttempts,
        ],
    );

    insert(
        &mut best,
        RequirementKind::ChildSpawn,
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
        RequirementKind::InheritedFds,
        Enforcement::Enforced,
        &[EvidenceClaim::MechanismAttestation],
    );
    insert(
        &mut best,
        RequirementKind::TempRoot,
        Enforcement::Enforced,
        &[EvidenceClaim::MechanismAttestation],
    );

    // ExposePath(Mount): MEDIATED — no first-class bind mount; symlink/junction
    // shim only, and it cannot deliver a private-to-boundary view. Honest cell.
    insert(
        &mut best,
        RequirementKind::ExposePath,
        Enforcement::Mediated,
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

    // Kill: Job Object Terminate = atomic subtree teardown.
    insert(
        &mut best,
        RequirementKind::Kill,
        Enforcement::Enforced,
        &[EvidenceClaim::ProcessTree, EvidenceClaim::TerminalOutcome],
    );

    SupportMatrix::from_best_case(best)
}

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

#[cfg(all(feature = "backend-windows", target_os = "windows"))]
mod backend_impl;
#[cfg(all(feature = "backend-windows", target_os = "windows"))]
pub use backend_impl::WindowsBackend;

#[cfg(all(feature = "backend-windows", target_os = "windows"))]
pub(crate) mod sys;

#[cfg(test)]
mod tests {
    use super::support_matrix;
    use crate::contract::capability::Enforcement;
    use crate::contract::support::RequirementKind;

    #[test]
    fn expose_path_and_allow_list_are_mediated() {
        // SCOPE §4 load-bearing honest cells: no first-class mount; WFP mediation.
        let m = support_matrix();
        assert_eq!(
            m.best_case_for(RequirementKind::ExposePath).enforcement,
            Enforcement::Mediated
        );
        assert_eq!(
            m.best_case_for(RequirementKind::NetworkAllowList)
                .enforcement,
            Enforcement::Mediated
        );
    }

    #[test]
    fn kill_is_enforced_via_job_object() {
        let m = support_matrix();
        assert_eq!(
            m.best_case_for(RequirementKind::Kill).enforcement,
            Enforcement::Enforced
        );
    }
}
