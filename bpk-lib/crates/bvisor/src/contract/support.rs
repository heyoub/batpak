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

/// The classification key: a requirement's shape, independent of its payload.
///
/// Guarantee-shaped, not mechanism-shaped — the matrix grades the KIND of thing
/// asked, and the planner inspects the concrete grade where it must distinguish
/// (e.g. `Network { DenyAll }` vs `Network { AllowList }`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum RequirementKind {
    /// [`Capability::Filesystem`].
    Filesystem,
    /// [`Capability::Network`] with `DenyAll`.
    NetworkDenyAll,
    /// [`Capability::Network`] with `AllowList`.
    NetworkAllowList,
    /// [`Capability::ChildSpawn`].
    ChildSpawn,
    /// [`Capability::Environment`].
    Environment,
    /// [`Capability::InheritedFds`].
    InheritedFds,
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
    /// Derive the classification key from a concrete requirement.
    #[must_use]
    pub fn of(req: &BoundaryRequirement) -> Self {
        match req {
            BoundaryRequirement::Capability(cap) => Self::of_capability(cap),
            BoundaryRequirement::HostControl(ctrl) => Self::of_control(ctrl),
        }
    }

    fn of_capability(cap: &Capability) -> Self {
        match cap {
            Capability::Filesystem { .. } => Self::Filesystem,
            Capability::Network {
                policy: crate::contract::capability::NetPolicy::DenyAll,
            } => Self::NetworkDenyAll,
            Capability::Network {
                policy: crate::contract::capability::NetPolicy::AllowList(_),
            } => Self::NetworkAllowList,
            Capability::ChildSpawn { .. } => Self::ChildSpawn,
            Capability::Environment { .. } => Self::Environment,
            Capability::InheritedFds { .. } => Self::InheritedFds,
        }
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
}
