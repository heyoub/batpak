//! [`Capability`] — the guest-invokable admitted authority POLICY, plus
//! [`Enforcement`] (the matrix verdict) and the guarantee-shaped grades.
//!
//! A [`Capability`] is the admitted rule the boundary ENFORCES on what the
//! WORKLOAD may attempt. It carries GRANTS and RESTRICTIONS — a deny-all
//! network policy is a restriction, still a Capability because it is the
//! admitted authority policy the backend must honor. Host lifecycle lives in
//! [`crate::HostControl`], NOT here: the confined workload cannot self-grant a
//! commit, a temp root, or its own launch.
//!
//! GRADES ARE GUARANTEE-SHAPED, NOT MECHANISM-SHAPED. The spec says WHAT
//! guarantee is required; the backend says HOW (pivot_root / Landlock / preopen
//! / Job Object / …) and records it in
//! [`crate::AdmittedRequirement::mechanism`] as evidence. [`Enforcement::Unsupported`]
//! is NEVER a requested value — it is only ever the backend's answer.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// The enforcement-strength axis of a support verdict.
///
/// One of two ORTHOGONAL axes (the other is [`EvidenceSet`]): this grades how
/// strongly a requirement is held; the evidence set grades what can be
/// witnessed about it. The two never collapse — a backend may enforce strongly
/// yet witness little (a structural guarantee with nothing per-attempt to see).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Enforcement {
    /// The backend can guarantee the requirement (strong primitive present).
    Enforced,
    /// The backend can honor the requirement only by mediating each attempt
    /// (e.g. a broker / notifier), not by a structural guarantee.
    Mediated,
    /// The backend cannot honor the requirement at all on this machine. Only
    /// ever a backend ANSWER; never a requested value. Forces `plan()` closed.
    Unsupported,
}

impl Enforcement {
    /// The MEET of the enforcement lattice in the SECURITY order (`Enforced`
    /// strongest, `Unsupported` the fail-closed bottom). `Unsupported` on either
    /// side wins (absorbing); this is the algebra the admission matrix floors a
    /// family best-case by a machine ceiling with. (Note: the derived `Ord` runs
    /// the other way — declaration order — so this meet equals the `Ord` MAX.)
    #[must_use]
    pub fn meet(self, other: Self) -> Self {
        match (self, other) {
            (Self::Unsupported, _) | (_, Self::Unsupported) => Self::Unsupported,
            (Self::Mediated, _) | (_, Self::Mediated) => Self::Mediated,
            (Self::Enforced, Self::Enforced) => Self::Enforced,
        }
    }
}

/// One kind of evidence a backend can produce for a requirement.
///
/// The members of the EVIDENCE axis — orthogonal to [`Enforcement`]. A scalar
/// "coverage" level would be dishonest: a backend that witnesses denied attempts
/// but not allowed actions is incomparable to one that does the reverse. So
/// evidence is a SET of explicit claims, and composition is set intersection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EvidenceClaim {
    /// The run's terminal outcome + exit status are observable.
    TerminalOutcome,
    /// Captured stdout/stderr are observable.
    CapturedStreams,
    /// CPU/memory/IO resource usage is observable.
    ResourceUsage,
    /// The operations the workload performed are observable.
    AllowedActions,
    /// Each attempt the boundary blocked is observable.
    DeniedAttempts,
    /// Filesystem creations/modifications are observable.
    FilesystemDelta,
    /// The child process tree is observable.
    ProcessTree,
    /// Network connections/traffic are observable.
    NetworkActivity,
    /// Produced-artifact provenance is observable.
    ArtifactLineage,
    /// The confinement mechanism actually applied is attestable.
    MechanismAttestation,
}

/// A set of [`EvidenceClaim`]s — the evidence a backend can produce (the
/// "available" set) or a caller requires (the "required" set).
///
/// Forms a lattice under `⊆`: the MEET is INTERSECTION (composing two backends/
/// ceilings yields only the evidence BOTH can produce); the JOIN is UNION (the
/// total evidence a plan can produce across its admitted requirements). The
/// empty set is the absorbing bottom of the meet; planning admits only when the
/// required set is a subset of the available set.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EvidenceSet(BTreeSet<EvidenceClaim>);

impl EvidenceSet {
    /// The empty evidence set.
    #[must_use]
    pub fn new() -> Self {
        Self(BTreeSet::new())
    }

    /// Insert a claim; returns true if newly added.
    pub fn insert(&mut self, claim: EvidenceClaim) -> bool {
        self.0.insert(claim)
    }

    /// Whether the set contains a claim.
    #[must_use]
    pub fn contains(&self, claim: EvidenceClaim) -> bool {
        self.0.contains(&claim)
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The number of claims in the set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether every claim in `self` is also in `other` (the lattice `⊆`).
    #[must_use]
    pub fn is_subset(&self, other: &Self) -> bool {
        self.0.is_subset(&other.0)
    }

    /// The MEET: claims present in BOTH sets.
    #[must_use]
    pub fn intersection(&self, other: &Self) -> Self {
        Self(self.0.intersection(&other.0).copied().collect())
    }

    /// The JOIN: claims present in EITHER set.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        Self(self.0.union(&other.0).copied().collect())
    }

    /// Fold another set's claims into this one (in-place union).
    pub fn extend_from(&mut self, other: &Self) {
        self.0.extend(other.0.iter().copied());
    }

    /// Iterate the claims in canonical (sorted) order.
    pub fn iter(&self) -> impl Iterator<Item = EvidenceClaim> + '_ {
        self.0.iter().copied()
    }
}

impl FromIterator<EvidenceClaim> for EvidenceSet {
    fn from_iter<I: IntoIterator<Item = EvidenceClaim>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

/// The full support answer for one requirement: a PRODUCT of the two orthogonal
/// axes — [`Enforcement`] (how strongly held) and [`EvidenceSet`] (what can be
/// witnessed). The matrix grades both; planning floors both via [`Self::meet`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportVerdict {
    /// How strongly the requirement is enforced.
    pub enforcement: Enforcement,
    /// What evidence the backend can produce for it.
    pub evidence: EvidenceSet,
}

impl SupportVerdict {
    /// Construct a verdict from both axes.
    #[must_use]
    pub fn new(enforcement: Enforcement, evidence: EvidenceSet) -> Self {
        Self {
            enforcement,
            evidence,
        }
    }

    /// The fail-closed bottom: unsupported, witnessing nothing.
    #[must_use]
    pub fn unsupported() -> Self {
        Self {
            enforcement: Enforcement::Unsupported,
            evidence: EvidenceSet::new(),
        }
    }

    /// The MEET of two verdicts — floor the enforcement, intersect the evidence.
    /// A product of two meet-semilattices is a meet-semilattice, so flooring a
    /// family best-case by a machine ceiling (or composing N ceilings) is
    /// commutative, associative, and order-independent.
    #[must_use]
    pub fn meet(&self, other: &Self) -> Self {
        Self {
            enforcement: self.enforcement.meet(other.enforcement),
            evidence: self.evidence.intersection(&other.evidence),
        }
    }
}

/// Guest-invokable admitted authority policy (grants AND restrictions).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Capability {
    /// Filesystem authority confined to a declared scope.
    Filesystem {
        /// Read / write / read-write grant.
        access: FsAccess,
        /// The declared roots the access is scoped to.
        scope: PathSet,
        /// Whether the scope applies recursively under each root.
        recursive: bool,
        /// The confinement GUARANTEE required (not a mechanism).
        confinement: FsConfinement,
    },
    /// Network authority: deny-all (restriction) or a scoped allow-list (grant).
    Network {
        /// The admitted network policy.
        policy: NetPolicy,
    },
    /// Authority for the workload to spawn its OWN children. The workload's
    /// initial launch is a [`crate::HostControl::LaunchWorkload`], not this.
    ChildSpawn {
        /// Whether the workload may spawn children.
        policy: SpawnPolicy,
    },
    /// Environment authority: empty-by-default; explicit grants only.
    Environment {
        /// The admitted environment policy.
        policy: EnvPolicy,
    },
    /// Which host file descriptors survive into the workload; default is none.
    InheritedFds {
        /// The admitted fd-inheritance policy.
        policy: FdPolicy,
    },
}

/// Filesystem access grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum FsAccess {
    /// Read only.
    Read,
    /// Write only.
    Write,
    /// Read and write.
    ReadWrite,
}

/// GUARANTEE: "reads/writes confined to the declared scope" — not a mechanism.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum FsConfinement {
    /// Access is confined to the declared roots and nothing outside them.
    DeclaredRootsOnly,
}

/// GUARANTEE: deny vs scoped-allow (a policy, not a mechanism).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum NetPolicy {
    /// All network access is denied (a restriction).
    DenyAll,
    /// Only the listed destinations are reachable (a scoped grant).
    AllowList(Vec<NetDest>),
}

/// Whether the workload may spawn child processes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SpawnPolicy {
    /// The workload may not spawn children.
    Deny,
    /// The workload may spawn children.
    Allow,
}

/// Environment-variable policy: empty by default, explicit keys only.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EnvPolicy {
    /// The environment is empty except for the explicitly granted keys.
    EmptyExcept(Vec<String>),
}

/// Host-fd inheritance policy: none by default, explicit fds only.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum FdPolicy {
    /// No host file descriptors survive into the workload.
    None,
    /// Only the listed raw fds survive into the workload.
    Only(Vec<u32>),
}

/// A declared set of filesystem roots a [`Capability::Filesystem`] is scoped to.
///
/// Portable, inert string paths — the contract never touches the filesystem, so
/// these are evidence/scope data, not opened handles.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PathSet {
    /// The declared roots, as portable path strings.
    pub roots: Vec<String>,
}

impl PathSet {
    /// An empty path set (no roots declared).
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }
}

/// A single allow-listed network destination (host + port), inert evidence.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NetDest {
    /// Destination host (name or address), as a portable string.
    pub host: String,
    /// Destination port.
    pub port: u16,
}

#[cfg(test)]
mod lattice_laws {
    //! The admission algebra is a bounded meet-semilattice on each axis and on
    //! their product. These exhaustive laws pin it: `Enforcement::meet` (the
    //! enforcement floor), `EvidenceSet::intersection` (the evidence meet), and
    //! `SupportVerdict::meet` (the product). Associativity is what makes
    //! composing N machine ceilings / primitives deterministic regardless of
    //! grouping; the absorbing bottoms (`Unsupported`, the empty set) are the
    //! fail-closed properties.
    use super::{Enforcement, EvidenceClaim, EvidenceSet, SupportVerdict};

    const ENFORCEMENTS: [Enforcement; 3] = [
        Enforcement::Enforced,
        Enforcement::Mediated,
        Enforcement::Unsupported,
    ];

    fn enforcement_strength(e: Enforcement) -> u8 {
        match e {
            Enforcement::Enforced => 2,
            Enforcement::Mediated => 1,
            Enforcement::Unsupported => 0,
        }
    }

    /// All evidence claims — the lattice top, listed in-crate (we own the enum).
    fn full_evidence() -> EvidenceSet {
        [
            EvidenceClaim::TerminalOutcome,
            EvidenceClaim::CapturedStreams,
            EvidenceClaim::ResourceUsage,
            EvidenceClaim::AllowedActions,
            EvidenceClaim::DeniedAttempts,
            EvidenceClaim::FilesystemDelta,
            EvidenceClaim::ProcessTree,
            EvidenceClaim::NetworkActivity,
            EvidenceClaim::ArtifactLineage,
            EvidenceClaim::MechanismAttestation,
        ]
        .into_iter()
        .collect()
    }

    /// A small, representative spread of evidence sets for brute-forcing laws.
    fn evidence_samples() -> Vec<EvidenceSet> {
        vec![
            EvidenceSet::new(),
            [EvidenceClaim::TerminalOutcome].into_iter().collect(),
            [
                EvidenceClaim::TerminalOutcome,
                EvidenceClaim::CapturedStreams,
            ]
            .into_iter()
            .collect(),
            [
                EvidenceClaim::CapturedStreams,
                EvidenceClaim::NetworkActivity,
            ]
            .into_iter()
            .collect(),
            full_evidence(),
        ]
    }

    // ── Enforcement meet (moved from support::floor; same algebra) ──

    #[test]
    fn enforcement_meet_is_commutative_associative_idempotent() {
        for a in ENFORCEMENTS {
            assert_eq!(a.meet(a), a, "idempotent at {a:?}");
            for b in ENFORCEMENTS {
                assert_eq!(a.meet(b), b.meet(a), "commutative at ({a:?},{b:?})");
                for c in ENFORCEMENTS {
                    assert_eq!(
                        a.meet(b).meet(c),
                        a.meet(b.meet(c)),
                        "associative at ({a:?},{b:?},{c:?})",
                    );
                }
            }
        }
    }

    #[test]
    fn enforcement_unsupported_absorbs_and_enforced_is_identity() {
        for a in ENFORCEMENTS {
            assert_eq!(Enforcement::Unsupported.meet(a), Enforcement::Unsupported);
            assert_eq!(a.meet(Enforcement::Unsupported), Enforcement::Unsupported);
            assert_eq!(Enforcement::Enforced.meet(a), a);
            assert_eq!(a.meet(Enforcement::Enforced), a);
        }
    }

    #[test]
    fn enforcement_meet_is_the_glb_in_security_order() {
        for a in ENFORCEMENTS {
            for b in ENFORCEMENTS {
                let m = a.meet(b);
                assert_eq!(
                    enforcement_strength(m),
                    enforcement_strength(a).min(enforcement_strength(b)),
                    "meet is the GLB at ({a:?},{b:?})",
                );
            }
        }
    }

    // ── Evidence intersection (the evidence meet) ──

    #[test]
    fn evidence_intersection_is_commutative_associative_idempotent() {
        for a in &evidence_samples() {
            assert_eq!(&a.intersection(a), a, "idempotent");
            for b in &evidence_samples() {
                assert_eq!(a.intersection(b), b.intersection(a), "commutative");
                for c in &evidence_samples() {
                    assert_eq!(
                        a.intersection(b).intersection(c),
                        a.intersection(&b.intersection(c)),
                        "associative",
                    );
                }
            }
        }
    }

    #[test]
    fn evidence_empty_absorbs_and_full_is_identity() {
        let empty = EvidenceSet::new();
        let full = full_evidence();
        for a in &evidence_samples() {
            assert_eq!(
                a.intersection(&empty),
                empty,
                "empty is the absorbing bottom"
            );
            assert_eq!(&a.intersection(&full), a, "full is the identity");
        }
    }

    #[test]
    fn evidence_intersection_is_a_lower_bound() {
        for a in &evidence_samples() {
            for b in &evidence_samples() {
                let m = a.intersection(b);
                assert!(m.is_subset(a) && m.is_subset(b), "meet ⊆ both inputs");
            }
        }
    }

    // ── Product verdict meet ──

    #[test]
    fn verdict_meet_is_commutative_associative_idempotent() {
        let verdicts: Vec<SupportVerdict> = ENFORCEMENTS
            .iter()
            .zip(evidence_samples())
            .map(|(&e, ev)| SupportVerdict::new(e, ev))
            .collect();
        for a in &verdicts {
            assert_eq!(&a.meet(a), a, "idempotent");
            for b in &verdicts {
                assert_eq!(a.meet(b), b.meet(a), "commutative");
                for c in &verdicts {
                    assert_eq!(a.meet(b).meet(c), a.meet(&b.meet(c)), "associative",);
                }
            }
        }
    }

    #[test]
    fn verdict_meet_floors_both_axes() {
        let a = SupportVerdict::new(
            Enforcement::Enforced,
            [
                EvidenceClaim::TerminalOutcome,
                EvidenceClaim::CapturedStreams,
            ]
            .into_iter()
            .collect(),
        );
        let b = SupportVerdict::new(
            Enforcement::Mediated,
            [
                EvidenceClaim::CapturedStreams,
                EvidenceClaim::NetworkActivity,
            ]
            .into_iter()
            .collect(),
        );
        let m = a.meet(&b);
        assert_eq!(m.enforcement, Enforcement::Mediated);
        assert_eq!(
            m.evidence,
            [EvidenceClaim::CapturedStreams].into_iter().collect()
        );
    }
}
