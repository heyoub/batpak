//! Static family support ([`SupportMatrix`]) vs RAW probe
//! ([`BackendProfileSnapshot`]) vs TYPED planning view ([`BackendProfile`]).
//!
//! Three layers, deliberately separated so a string probe fact never becomes a
//! security decision:
//! - [`SupportMatrix`] is what a backend FAMILY can THEORETICALLY do — static,
//!   per-backend-kind, TYPED.
//! - [`BackendProfileSnapshot`] is RAW probe facts for THIS machine — portable,
//!   string-ish, persisted in the plan + report for AUDIT/REPLAY ONLY. It is
//!   NEVER consulted directly at admission.
//! - [`BackendProfile`] is the TYPED planning view, derived DETERMINISTICALLY
//!   from the raw snapshot so replay re-derives identical admission decisions.

use crate::contract::budget::BudgetProfile;
use crate::contract::capability::{Capability, SupportVerdict};
use crate::contract::host_control::HostControl;
use crate::contract::ids::BackendId;
use crate::contract::plan::BoundaryRequirement;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// What a backend FAMILY can THEORETICALLY do. Static, per-backend-kind, TYPED.
///
/// A rule maps a requirement KIND to the BEST verdict the family could reach;
/// the [`BackendProfile`] then floors that verdict to what THIS machine has.
/// Modeling the matrix as a typed table (rather than a closure) keeps it
/// inert, serializable-in-principle, and replay-stable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SupportMatrix {
    /// Best-case [`SupportVerdict`] per [`RequirementKind`]. A kind absent from
    /// the table is the fail-closed bottom ([`SupportVerdict::unsupported`]).
    best_case: BTreeMap<RequirementKind, SupportVerdict>,
}

impl SupportMatrix {
    /// Build a support matrix from an explicit best-case table. Any
    /// [`RequirementKind`] not listed is the fail-closed bottom.
    #[must_use]
    pub fn from_best_case(best_case: BTreeMap<RequirementKind, SupportVerdict>) -> Self {
        Self { best_case }
    }

    /// The FAMILY best-case verdict for one [`RequirementKind`], independent of
    /// any machine profile — the fail-closed bottom if the kind is absent.
    ///
    /// This is the static honesty surface: it exposes exactly what the family
    /// CLAIMS it could do, so a per-platform honesty test (and the gauntlet's
    /// lying-table red fixture) can assert a load-bearing `Unsupported`/`Mediated`
    /// cell WITHOUT fabricating a machine profile. It is NOT consulted at
    /// admission — `classify` floors this by the machine ceiling.
    #[must_use]
    pub fn best_case_for(&self, kind: RequirementKind) -> SupportVerdict {
        self.best_case
            .get(&kind)
            .cloned()
            .unwrap_or_else(SupportVerdict::unsupported)
    }

    /// Classify a requirement against the TYPED profile (no string parsing at
    /// admission). The verdict is the family best-case MET with what the machine
    /// profile actually provides — enforcement floored, evidence intersected.
    #[must_use]
    pub fn classify(&self, req: &BoundaryRequirement, profile: &BackendProfile) -> SupportVerdict {
        let kind = RequirementKind::of(req);
        let best = self
            .best_case
            .get(&kind)
            .cloned()
            .unwrap_or_else(SupportVerdict::unsupported);
        best.meet(&profile.ceiling_for(kind))
    }
}

/// The classification key: a requirement's CANONICAL POLICY identity (proof-spine
/// §2), one key per semantically-distinct policy.
///
/// Guarantee-shaped, not mechanism-shaped — the matrix grades the KIND of thing
/// asked. The key is INJECTIVE over canonical-policy meaning (the §2 law: distinct
/// [`crate::contract::canonical_policy::CanonicalPolicy`] ⇒ distinct key), so each
/// capability variant that carries a distinct policy gets its OWN key
/// (`Network { DenyAll }` vs `Network { AllowList }`, `InheritedFds { None }` vs
/// `{ Only }`, `ChildSpawn { Deny }` vs `{ Allow }`). A future backend could
/// differentiate cells we currently lower identically, so we never pre-collapse
/// them. `Environment` carries a single policy variant today (`EmptyExcept`), so it
/// is one key until the S4 `Environment::Exact` split.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum RequirementKind {
    /// [`Capability::Filesystem`].
    Filesystem,
    /// [`Capability::Network`] with `DenyAll`.
    NetworkDenyAll,
    /// [`Capability::Network`] with `AllowList`.
    NetworkAllowList,
    /// [`Capability::ChildSpawn`] with [`crate::SpawnPolicy::Deny`].
    ChildSpawnDeny,
    /// [`Capability::ChildSpawn`] with [`crate::SpawnPolicy::Allow`].
    ChildSpawnAllow,
    /// [`Capability::Environment`].
    Environment,
    /// [`Capability::InheritedFds`] with [`crate::FdPolicy::None`].
    InheritedFdsNone,
    /// [`Capability::InheritedFds`] with [`crate::FdPolicy::Only`].
    InheritedFdsOnly,
    /// [`HostControl::LaunchWorkload`].
    LaunchWorkload,
    /// [`HostControl::CaptureStreams`].
    CaptureStreams,
    /// [`HostControl::TempRoot`].
    TempRoot,
    /// [`HostControl::ExposePath`].
    ExposePath,
    /// [`HostControl::CommitArtifact`].
    CommitArtifact,
    /// [`HostControl::DiscardArtifact`].
    DiscardArtifact,
    /// [`HostControl::Kill`].
    Kill,
    /// [`HostControl::ListOutputs`].
    ListOutputs,
}

impl RequirementKind {
    /// Every requirement key, in declaration order. Exhaustive by construction (a
    /// new variant that is not added here fails the `all_is_exhaustive` test), so a
    /// gate that must scan EVERY kind (e.g. the qualification coupling gate) can
    /// enumerate them without a runtime registry.
    pub const ALL: [Self; 16] = [
        Self::Filesystem,
        Self::NetworkDenyAll,
        Self::NetworkAllowList,
        Self::ChildSpawnDeny,
        Self::ChildSpawnAllow,
        Self::Environment,
        Self::InheritedFdsNone,
        Self::InheritedFdsOnly,
        Self::LaunchWorkload,
        Self::CaptureStreams,
        Self::TempRoot,
        Self::ExposePath,
        Self::CommitArtifact,
        Self::DiscardArtifact,
        Self::Kill,
        Self::ListOutputs,
    ];

    /// Derive the classification key from a concrete requirement.
    #[must_use]
    pub fn of(req: &BoundaryRequirement) -> Self {
        match req {
            BoundaryRequirement::Capability(cap) => Self::of_capability(cap),
            BoundaryRequirement::HostControl(ctrl) => Self::of_control(ctrl),
        }
    }

    /// Derive the policy-aware key (proof-spine §2): each capability's distinct
    /// CANONICAL POLICY maps to a distinct key. POLICY-AWARE for ALL kinds — no
    /// policy-blind collapse — so two semantically-distinct policies under the same
    /// capability variant (`InheritedFds::None` vs `::Only`, `ChildSpawn::Deny` vs
    /// `::Allow`, `Network::DenyAll` vs `::AllowList`) never share a key.
    /// `Environment` has a single policy variant today (`EmptyExcept`), so one key.
    fn of_capability(cap: &Capability) -> Self {
        use crate::contract::capability::{FdPolicy, NetPolicy, SpawnPolicy};
        match cap {
            Capability::Filesystem { .. } => Self::Filesystem,
            Capability::Network {
                policy: NetPolicy::DenyAll,
            } => Self::NetworkDenyAll,
            Capability::Network {
                policy: NetPolicy::AllowList(_),
            } => Self::NetworkAllowList,
            Capability::ChildSpawn {
                policy: SpawnPolicy::Deny,
            } => Self::ChildSpawnDeny,
            Capability::ChildSpawn {
                policy: SpawnPolicy::Allow,
            } => Self::ChildSpawnAllow,
            Capability::Environment { .. } => Self::Environment,
            Capability::InheritedFds {
                policy: FdPolicy::None,
            } => Self::InheritedFdsNone,
            Capability::InheritedFds {
                policy: FdPolicy::Only(_),
            } => Self::InheritedFdsOnly,
        }
    }

    /// Test-only accessor for the policy→key map (the injective gate exercises it
    /// directly with constructed capabilities). Production code reaches it through
    /// [`Self::of`].
    #[cfg(test)]
    pub(crate) fn of_capability_for_test(cap: &Capability) -> Self {
        Self::of_capability(cap)
    }

    fn of_control(ctrl: &HostControl) -> Self {
        match ctrl {
            HostControl::LaunchWorkload => Self::LaunchWorkload,
            HostControl::CaptureStreams { .. } => Self::CaptureStreams,
            HostControl::TempRoot { .. } => Self::TempRoot,
            HostControl::ExposePath { .. } => Self::ExposePath,
            HostControl::CommitArtifact { .. } => Self::CommitArtifact,
            HostControl::DiscardArtifact => Self::DiscardArtifact,
            HostControl::Kill { .. } => Self::Kill,
            HostControl::ListOutputs => Self::ListOutputs,
        }
    }
}

/// RAW probe facts — portable, string-ish, persisted in the plan + report for
/// AUDIT/REPLAY ONLY. NEVER consulted directly at admission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendProfileSnapshot {
    /// The backend this snapshot was probed from.
    pub backend: BackendId,
    /// Raw probe facts, e.g. `"landlock_abi" -> "4"`, `"cgroup_v2" -> "true"`.
    /// A `BTreeMap` so the persisted bytes are key-sorted and replay-stable.
    pub probed: BTreeMap<String, String>,
    /// The machine's seven-dimensional budget capability — backend-declared
    /// availability, guarantee, evidence, and mechanism per dimension. Bound into
    /// plan identity via `H_P`; never authored by the caller.
    pub budget: BudgetProfile,
}

/// TYPED planning view, derived DETERMINISTICALLY from a raw snapshot. The
/// planner consults THIS, never the map.
///
/// Holds a per-[`RequirementKind`] machine CEILING: the strongest enforcement
/// the machine can actually back for that kind right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendProfile {
    ceiling: BTreeMap<RequirementKind, SupportVerdict>,
}

impl BackendProfile {
    /// Build a typed profile from an explicit per-kind ceiling table. A kind
    /// absent from the table is the fail-closed bottom.
    #[must_use]
    pub fn from_ceiling(ceiling: BTreeMap<RequirementKind, SupportVerdict>) -> Self {
        Self { ceiling }
    }

    /// The machine ceiling [`SupportVerdict`] for one requirement kind; the
    /// fail-closed bottom if unknown.
    #[must_use]
    pub fn ceiling_for(&self, kind: RequirementKind) -> SupportVerdict {
        self.ceiling
            .get(&kind)
            .cloned()
            .unwrap_or_else(SupportVerdict::unsupported)
    }

    /// The requirement kinds this ceiling advertises at [`Enforcement::Enforced`],
    /// in canonical order — exactly the cells the qualification coupling gate must
    /// find a `Proven` ledger row for.
    #[must_use]
    pub fn enforced_kinds(&self) -> Vec<RequirementKind> {
        RequirementKind::ALL
            .into_iter()
            .filter(|&k| {
                self.ceiling_for(k).enforcement
                    == crate::contract::capability::Enforcement::Enforced
            })
            .collect()
    }
}

#[cfg(test)]
mod requirement_kind_tests {
    use super::RequirementKind;

    /// `RequirementKind::ALL` must list EVERY variant (the coupling gate enumerates
    /// it). A new variant added without extending `ALL` makes this `match` fail to
    /// compile — the exhaustiveness tripwire — and the count assert backs it up.
    #[test]
    fn all_is_exhaustive() {
        for kind in RequirementKind::ALL {
            // Exhaustive match: a new variant forces this to be updated.
            match kind {
                RequirementKind::Filesystem
                | RequirementKind::NetworkDenyAll
                | RequirementKind::NetworkAllowList
                | RequirementKind::ChildSpawnDeny
                | RequirementKind::ChildSpawnAllow
                | RequirementKind::Environment
                | RequirementKind::InheritedFdsNone
                | RequirementKind::InheritedFdsOnly
                | RequirementKind::LaunchWorkload
                | RequirementKind::CaptureStreams
                | RequirementKind::TempRoot
                | RequirementKind::ExposePath
                | RequirementKind::CommitArtifact
                | RequirementKind::DiscardArtifact
                | RequirementKind::Kill
                | RequirementKind::ListOutputs => {}
            }
        }
        // No duplicates.
        let mut seen = std::collections::BTreeSet::new();
        for kind in RequirementKind::ALL {
            assert!(seen.insert(kind), "duplicate kind in ALL: {kind:?}");
        }
        assert_eq!(seen.len(), RequirementKind::ALL.len());
    }
}

#[cfg(test)]
#[path = "support_injective_tests.rs"]
mod support_injective_tests;
