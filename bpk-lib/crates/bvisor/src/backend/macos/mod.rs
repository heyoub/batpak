//! macOS backend — Seatbelt (`sandbox_init`) confinement (scaffolding).
//!
//! STEP (a) scaffolding: the HONEST per-platform [`SupportMatrix`] (pure data,
//! always-compiled, cross-platform unit-testable) plus a [`MacosBackend`] struct
//! whose `execute()` is a stub returning [`Outcome::Unsupported`]. Real syscalls
//! (`sandbox_init` profile application, pgid teardown) land in step (e), in the
//! [`super::macos::sys`] unsafe basement (`libc` extern FFI). NO `unsafe` here.
//!
//! HONESTY (SCOPE §4 — macOS): honestly WEAK. FS/Net are
//! [`Enforcement::Mediated`] (deprecated-but-shipped Seatbelt — a profile, not a
//! structural namespace). `ExposePath`(Mount) is [`Enforcement::Unsupported`]
//! (no per-boundary bind mount). `Kill` is `Mediated` (pgid only — no atomic
//! subtree teardown, an escape window exists). `NetworkAllowList` is
//! `Unsupported`. These are load-bearing honest cells — NEVER inflate Seatbelt.

use crate::contract::capability::{Enforcement, EvidenceClaim, SupportVerdict};
use crate::contract::support::{RequirementKind, SupportMatrix};
use std::collections::BTreeMap;

/// The HONEST macOS family support matrix (SCOPE §4). Pure data — constructible
/// and unit-testable on any host.
#[must_use]
pub fn support_matrix() -> SupportMatrix {
    let mut best = BTreeMap::new();

    insert(
        &mut best,
        RequirementKind::LaunchWorkload,
        Enforcement::Enforced,
        &[EvidenceClaim::TerminalOutcome],
    );
    insert(
        &mut best,
        RequirementKind::CaptureStreams,
        Enforcement::Enforced,
        &[EvidenceClaim::CapturedStreams],
    );

    // FS via Seatbelt profile: MEDIATED — a deprecated profile interpreter, not a
    // structural namespace guarantee. Honest cell.
    insert(
        &mut best,
        RequirementKind::Filesystem,
        Enforcement::Mediated,
        &[
            EvidenceClaim::DeniedAttempts,
            EvidenceClaim::FilesystemDelta,
        ],
    );

    // Net deny-all via Seatbelt: MEDIATED. NetworkAllowList: UNSUPPORTED (no broker).
    insert(
        &mut best,
        RequirementKind::NetworkDenyAll,
        Enforcement::Mediated,
        &[EvidenceClaim::DeniedAttempts],
    );
    insert(
        &mut best,
        RequirementKind::NetworkAllowList,
        Enforcement::Unsupported,
        &[],
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
    insert(
        &mut best,
        RequirementKind::TempRoot,
        Enforcement::Enforced,
        &[EvidenceClaim::MechanismAttestation],
    );

    // ExposePath(Mount): UNSUPPORTED — no per-boundary bind mount. Honest cell.
    insert(
        &mut best,
        RequirementKind::ExposePath,
        Enforcement::Unsupported,
        &[],
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

    // Kill: MEDIATED — pgid signal only, no atomic subtree teardown (escape
    // window). Honest cell: weaker than Linux/Windows.
    insert(
        &mut best,
        RequirementKind::Kill,
        Enforcement::Mediated,
        &[EvidenceClaim::TerminalOutcome],
    );

    // ChildSpawn{Deny,Allow}: UNSUPPORTED — no demonstrated native mechanism to
    // deny new tasks or attenuate descendant spawning within the boundary on
    // macOS (§6 macOS SPIKE pending). Stated EXPLICITLY (never silently absent) so
    // the per-profile completeness gate sees a real answer for every key.
    insert(
        &mut best,
        RequirementKind::ChildSpawnDeny,
        Enforcement::Unsupported,
        &[],
    );
    insert(
        &mut best,
        RequirementKind::ChildSpawnAllow,
        Enforcement::Unsupported,
        &[],
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

#[cfg(all(feature = "backend-macos", target_os = "macos"))]
mod backend_impl;
#[cfg(all(feature = "backend-macos", target_os = "macos"))]
pub use backend_impl::MacosBackend;

#[cfg(all(feature = "backend-macos", target_os = "macos"))]
pub(crate) mod sys;

#[cfg(test)]
mod tests {
    use super::support_matrix;
    use crate::contract::capability::Enforcement;
    use crate::contract::support::RequirementKind;

    #[test]
    fn mount_and_allow_list_are_unsupported() {
        // SCOPE §4 load-bearing honest cells: no bind mount, no broker.
        let m = support_matrix();
        assert_eq!(
            m.best_case_for(RequirementKind::ExposePath).enforcement,
            Enforcement::Unsupported
        );
        assert_eq!(
            m.best_case_for(RequirementKind::NetworkAllowList)
                .enforcement,
            Enforcement::Unsupported
        );
    }

    #[test]
    fn filesystem_and_kill_are_mediated_not_enforced() {
        // Honestly weak: Seatbelt FS is Mediated, pgid Kill is Mediated.
        let m = support_matrix();
        assert_eq!(
            m.best_case_for(RequirementKind::Filesystem).enforcement,
            Enforcement::Mediated
        );
        assert_eq!(
            m.best_case_for(RequirementKind::Kill).enforcement,
            Enforcement::Mediated
        );
    }
}
