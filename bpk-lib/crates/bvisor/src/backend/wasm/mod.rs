//! Wasm backend — WASI-preopen confinement (scaffolding).
//!
//! STEP (a) scaffolding: the HONEST per-platform [`SupportMatrix`] (pure data,
//! always-compiled, cross-platform unit-testable) plus a [`WasmBackend`] struct
//! whose `execute()` is a stub returning [`Outcome::Unsupported`]. A `wasmi`
//! interpreter skeleton lands in step (c); `wasmtime` depth runs on CI. Wasm has
//! NO unsafe basement (wasmtime/wasmi are safe Rust), so there is NO `sys.rs`.
//!
//! HONESTY (SCOPE §4 — Wasm): FS/env/stdio/`TempRoot`/`Commit`/`Discard`/`List`
//! Enforced via WASI preopens. `ChildSpawn` / `Kill` / `ExposePath`(Mount) are
//! STRUCTURALLY [`Enforcement::Unsupported`] — a wasm guest has no native fork or
//! kill, and there is no host mount to expose. `NetworkAllowList` is also
//! `Unsupported`. These are load-bearing fail-closed cells — NEVER fake a fork.

use crate::contract::capability::{Enforcement, EvidenceClaim, SupportVerdict};
use crate::contract::support::{RequirementKind, SupportMatrix};
use std::collections::BTreeMap;

/// The HONEST Wasm family support matrix (SCOPE §4). Pure data — constructible
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

    // FS via WASI preopen: structurally confined to the preopened dirs.
    insert(
        &mut best,
        RequirementKind::Filesystem,
        Enforcement::Enforced,
        &[
            EvidenceClaim::AllowedActions,
            EvidenceClaim::FilesystemDelta,
            EvidenceClaim::MechanismAttestation,
        ],
    );
    insert(
        &mut best,
        RequirementKind::Environment,
        Enforcement::Enforced,
        &[EvidenceClaim::MechanismAttestation],
    );
    insert(
        &mut best,
        RequirementKind::TempRoot,
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

    // Deny-all network: the guest gets no WASI socket capabilities at all.
    insert(
        &mut best,
        RequirementKind::NetworkDenyAll,
        Enforcement::Enforced,
        &[EvidenceClaim::DeniedAttempts],
    );

    // STRUCTURALLY UNSUPPORTED — no native fork/kill/mount in a wasm guest, no
    // allow-list broker. Listed explicitly so the honesty is a stated answer.
    insert(
        &mut best,
        RequirementKind::ChildSpawn,
        Enforcement::Unsupported,
        &[],
    );
    insert(
        &mut best,
        RequirementKind::Kill,
        Enforcement::Unsupported,
        &[],
    );
    insert(
        &mut best,
        RequirementKind::ExposePath,
        Enforcement::Unsupported,
        &[],
    );
    insert(
        &mut best,
        RequirementKind::NetworkAllowList,
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

// The wasm runtime backend compiles only behind the feature (the interpreter is
// host-OS-independent, so it is NOT target_os-gated — only feature-gated).
#[cfg(feature = "backend-wasm")]
mod backend_impl;
#[cfg(feature = "backend-wasm")]
pub use backend_impl::WasmBackend;

#[cfg(test)]
mod tests {
    use super::support_matrix;
    use crate::contract::capability::Enforcement;
    use crate::contract::support::RequirementKind;

    #[test]
    fn spawn_kill_mount_and_allow_list_are_structurally_unsupported() {
        // SCOPE §4 load-bearing honest cells: no native fork/kill/mount/broker.
        let m = support_matrix();
        for kind in [
            RequirementKind::ChildSpawn,
            RequirementKind::Kill,
            RequirementKind::ExposePath,
            RequirementKind::NetworkAllowList,
        ] {
            assert_eq!(
                m.best_case_for(kind).enforcement,
                Enforcement::Unsupported,
                "{kind:?} must be structurally Unsupported on wasm"
            );
        }
    }

    #[test]
    fn filesystem_is_enforced_via_preopen() {
        let m = support_matrix();
        assert_eq!(
            m.best_case_for(RequirementKind::Filesystem).enforcement,
            Enforcement::Enforced
        );
    }
}
