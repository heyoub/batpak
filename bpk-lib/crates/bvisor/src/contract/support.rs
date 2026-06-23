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

use crate::contract::capability::{Capability, Enforcement};
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
    /// Best-case verdict per [`RequirementKind`]. A kind absent from the table
    /// is [`Enforcement::Unsupported`] by default — fail closed.
    best_case: BTreeMap<RequirementKind, Enforcement>,
}

impl SupportMatrix {
    /// Build a support matrix from an explicit best-case table. Any
    /// [`RequirementKind`] not listed is treated as [`Enforcement::Unsupported`].
    #[must_use]
    pub fn from_best_case(best_case: BTreeMap<RequirementKind, Enforcement>) -> Self {
        Self { best_case }
    }

    /// Classify a requirement against the TYPED profile (no string parsing at
    /// admission). The verdict is the family best-case floored by what the
    /// machine profile actually provides.
    #[must_use]
    pub fn classify(&self, req: &BoundaryRequirement, profile: &BackendProfile) -> Enforcement {
        let kind = RequirementKind::of(req);
        let best = self
            .best_case
            .get(&kind)
            .copied()
            .unwrap_or(Enforcement::Unsupported);
        floor(best, profile.ceiling_for(kind))
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

/// Floor `best` by the machine `ceiling`: the verdict can never EXCEED what the
/// machine provides, and `Unsupported` on either side wins (fail closed).
fn floor(best: Enforcement, ceiling: Enforcement) -> Enforcement {
    match (best, ceiling) {
        (Enforcement::Unsupported, _) | (_, Enforcement::Unsupported) => Enforcement::Unsupported,
        (Enforcement::Mediated, _) | (_, Enforcement::Mediated) => Enforcement::Mediated,
        (Enforcement::Enforced, Enforcement::Enforced) => Enforcement::Enforced,
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
    ceiling: BTreeMap<RequirementKind, Enforcement>,
}

impl BackendProfile {
    /// Build a typed profile from an explicit per-kind ceiling table. A kind
    /// absent from the table is [`Enforcement::Unsupported`] (fail closed).
    #[must_use]
    pub fn from_ceiling(ceiling: BTreeMap<RequirementKind, Enforcement>) -> Self {
        Self { ceiling }
    }

    /// The machine ceiling for one requirement kind; `Unsupported` if unknown.
    #[must_use]
    pub fn ceiling_for(&self, kind: RequirementKind) -> Enforcement {
        self.ceiling
            .get(&kind)
            .copied()
            .unwrap_or(Enforcement::Unsupported)
    }
}

#[cfg(test)]
mod meet_semilattice_laws {
    //! [`floor`] is the MEET of the enforcement lattice in the SECURITY order
    //! (`Enforced` = top/strongest, `Unsupported` = bottom/fail-closed). The
    //! derived [`Ord`] on [`Enforcement`] runs the OTHER way (declaration order:
    //! `Enforced` < `Mediated` < `Unsupported`), so the security-meet equals the
    //! derived-`Ord` MAX — NOT the `min` an earlier sketch guessed. These
    //! exhaustive laws (the domain is 3 elements, so we brute-force every
    //! pairing/triple rather than sample) pin the algebra the admission matrix
    //! already relies on: a bounded meet-semilattice with `Unsupported` as the
    //! absorbing bottom (the load-bearing fail-closed property) and `Enforced`
    //! as the identity. Associativity is what makes composing N machine ceilings
    //! (layered confinement) deterministic regardless of grouping.
    use super::{floor, Enforcement};

    const ALL: [Enforcement; 3] = [
        Enforcement::Enforced,
        Enforcement::Mediated,
        Enforcement::Unsupported,
    ];

    /// Security strength: higher = stronger guarantee (`Enforced` strongest,
    /// `Unsupported` weakest). This is the REVERSE of the derived `Ord`.
    fn strength(e: Enforcement) -> u8 {
        match e {
            Enforcement::Enforced => 2,
            Enforcement::Mediated => 1,
            Enforcement::Unsupported => 0,
        }
    }

    #[test]
    fn floor_is_commutative() {
        for a in ALL {
            for b in ALL {
                assert_eq!(floor(a, b), floor(b, a), "commutativity at ({a:?},{b:?})");
            }
        }
    }

    #[test]
    fn floor_is_associative() {
        for a in ALL {
            for b in ALL {
                for c in ALL {
                    assert_eq!(
                        floor(floor(a, b), c),
                        floor(a, floor(b, c)),
                        "associativity at ({a:?},{b:?},{c:?})",
                    );
                }
            }
        }
    }

    #[test]
    fn floor_is_idempotent() {
        for a in ALL {
            assert_eq!(floor(a, a), a, "idempotence at {a:?}");
        }
    }

    #[test]
    fn unsupported_is_the_absorbing_bottom() {
        // Fail-closed: `Unsupported` on either side forces `Unsupported`. This is
        // the load-bearing security property of the whole admission lattice.
        for a in ALL {
            assert_eq!(floor(Enforcement::Unsupported, a), Enforcement::Unsupported);
            assert_eq!(floor(a, Enforcement::Unsupported), Enforcement::Unsupported);
        }
    }

    #[test]
    fn enforced_is_the_identity() {
        // `Enforced` is the lattice top: flooring by it leaves the other operand
        // unchanged (the machine could back anything; the requirement decides).
        for a in ALL {
            assert_eq!(floor(Enforcement::Enforced, a), a);
            assert_eq!(floor(a, Enforcement::Enforced), a);
        }
    }

    #[test]
    fn floor_is_the_greatest_lower_bound_in_security_order() {
        // The verdict is never STRONGER than either input, and is the strongest
        // that satisfies that bound — i.e. the GLB in the security order.
        for a in ALL {
            for b in ALL {
                let r = floor(a, b);
                assert!(
                    strength(r) <= strength(a) && strength(r) <= strength(b),
                    "floor exceeded an input at ({a:?},{b:?}) -> {r:?}",
                );
                assert_eq!(
                    strength(r),
                    strength(a).min(strength(b)),
                    "not the GLB at ({a:?},{b:?}) -> {r:?}",
                );
            }
        }
    }

    #[test]
    fn floor_equals_derived_ord_max() {
        // Consistency pin: because the derived `Ord` is reversed from strength,
        // the security-meet equals the derived-`Ord` MAX. `floor` could thus be
        // written `best.max(ceiling)`; this guards that identity (and corrects
        // the plan's "floor == min" misstatement).
        for a in ALL {
            for b in ALL {
                assert_eq!(floor(a, b), a.max(b), "floor != Ord-max at ({a:?},{b:?})");
            }
        }
    }
}
