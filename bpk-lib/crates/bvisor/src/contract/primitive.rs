//! Primitive-adapter model: algebra for *support*, ordered plan for *execution*.
//!
//! A [`ConfinePrimitive`] is one composable confinement mechanism (an empty
//! netns, a Landlock ruleset, a seccomp filter, a WASI preopen, …). Two axes,
//! deliberately separated (master plan §10):
//!
//! - **Classification is algebraic and order-independent.** Each primitive
//!   declares, via [`ConfinePrimitive::classify`], the [`SupportVerdict`] it can
//!   reach for a requirement on a given machine profile. Combining the primitives
//!   that *cover* a requirement is a `meet` (enforcement floored, evidence
//!   intersected); an uncovered requirement is the fail-closed bottom
//!   ([`classify_via_primitives`]). Order does not matter here.
//!
//! - **Execution is ordered and validated.** Order-independence does NOT extend
//!   to setup: a seccomp filter must not install before fds are prepared,
//!   `no_new_privs` must precede the filter, nothing launches before confinement
//!   is in place. Each primitive declares a [`LoweringPhase`], `prerequisites`,
//!   and `conflicts`; [`compile_lowering_plan`] compiles a **validated**
//!   [`LoweringPlan`] — a phase-ordered, prerequisite-respecting, conflict-free,
//!   acyclic sequence — and the runner executes THAT, never an arbitrary
//!   iteration.
//!
//! **Layering boundary (honest):** this module owns the *planning* surface —
//! everything needed to grade support and compile the ordered plan, all pure and
//! proven here. The genuinely effectful execution methods (`lower`, `rollback`)
//! and their context/mechanism types belong to the executor and land with the
//! first real backend (master plan step 6), where an OS execution model exists to
//! thread fds / namespaces / installed-policy handles through. Defining them now
//! against placeholder types would be vaporware, not contract.

use crate::contract::capability::{EvidenceSet, SupportVerdict};
use crate::contract::plan::BoundaryRequirement;
use crate::contract::report::BoundaryReportBody;
use crate::contract::support::{BackendProfile, RequirementKind};

/// The ordered setup phase a primitive lowers in.
///
/// Declaration order IS setup order — the derived `Ord` ranks an earlier phase
/// below a later one, so `NamespaceCreate < FsSetup < … < Teardown`. (Unlike
/// [`crate::Enforcement`], whose derived `Ord` is deliberately *reversed* from
/// security strength, here the derived order is the intended order — confinement
/// is built outside-in, then the workload launches, then evidence is observed,
/// then teardown runs.)
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LoweringPhase {
    /// Create namespaces / isolation domains (mount, pid, net, user).
    NamespaceCreate,
    /// Establish the filesystem view (binds, pivot_root, preopens).
    FsSetup,
    /// Drop privileges irreversibly (`no_new_privs`, setuid/gid, cap drop).
    PrivilegeDrop,
    /// Sanitize inherited file descriptors (CLOEXEC sweep, handle list).
    FdHygiene,
    /// Install enforcement policy (seccomp-BPF, LSM, WFP, Job Object limits).
    PolicyInstall,
    /// Launch the workload inside the now-established confinement.
    Launch,
    /// Observe the running workload (collect evidence; no new confinement).
    Observe,
    /// Tear down / clean up established confinement (best-effort).
    Teardown,
}

impl LoweringPhase {
    /// A stable wire code for canonical schedule encoding (NEVER reorder — the
    /// schedule digest `H_L` and the cross-path membrane depend on these being
    /// frozen; the derived `Ord` governs *setup* order, this governs *bytes*).
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Self::NamespaceCreate => 0,
            Self::FsSetup => 1,
            Self::PrivilegeDrop => 2,
            Self::FdHygiene => 3,
            Self::PolicyInstall => 4,
            Self::Launch => 5,
            Self::Observe => 6,
            Self::Teardown => 7,
        }
    }
}

/// Stable identity of a [`ConfinePrimitive`] within a backend's primitive set.
///
/// Used to express prerequisite and conflict relations between primitives and to
/// order the compiled [`LoweringPlan`]. Opaque, backend-assigned.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrimitiveId(String);

impl PrimitiveId {
    /// Construct a primitive id from any string-like value.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Borrow the underlying id string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PrimitiveId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A primitive declaration's revision, paired with [`PrimitiveId`] to bind a
/// schedule entry to the EXACT declaration it was compiled from.
///
/// `(PrimitiveId, PrimitiveVersion)` is the link between the pure declaration
/// (here) and the effectful backend implementation: the backend resolves the pair
/// to an impl, and the admission schedule membrane checks the declaration digest so
/// a stale or forged declaration cannot masquerade behind a known id. Monotonic per
/// id; a behavioural change to a primitive bumps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrimitiveVersion(u32);

impl PrimitiveVersion {
    /// Construct a version from its revision number.
    #[must_use]
    pub fn new(version: u32) -> Self {
        Self(version)
    }

    /// The revision number (for canonical encoding + ordering).
    #[must_use]
    pub fn get(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for PrimitiveVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "v{}", self.0)
    }
}

/// A privilege a primitive requires in order to lower.
///
/// An open, platform-varying set (a Linux `CAP_SYS_ADMIN`, a Windows token
/// privilege, a macOS entitlement) — a newtype string rather than a guessed
/// enum, so backends name exactly the privileges their mechanisms need without
/// the contract pretending to enumerate a closed universe it cannot.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Privilege(String);

impl Privilege {
    /// Construct a privilege token from any string-like value.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Borrow the underlying privilege token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Privilege {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One composable confinement mechanism. The planning + evidence surface; see
/// the module-level layering note for why `lower`/`rollback` arrive with the
/// executor.
///
/// Object-safe so a backend can hold its primitives as `&[&dyn PrimitiveDecl]`
/// and feed them to [`crate::contract::lowering::compile_schedule`] /
/// [`classify_via_primitives`].
///
/// This is the **pure declaration** half of the primitive split: identity, version,
/// phase, coverage, prerequisites, conflicts, profile-predicated verdict, and
/// evidence — all batpak-free and provable here. The effectful half (prepare /
/// construct-visibility / scrub / install / launch / observe / terminate / recover)
/// is backend-owned and resolved by `(id, version)`.
pub trait PrimitiveDecl {
    /// This primitive's stable identity (for prerequisite/conflict relations).
    fn id(&self) -> PrimitiveId;

    /// This declaration's revision; with [`PrimitiveDecl::id`] it binds a compiled
    /// schedule entry to the exact declaration (the schedule membrane checks it).
    fn version(&self) -> PrimitiveVersion;

    /// The requirement kinds this primitive contributes to confining. Used to
    /// select the primitives that classify a given requirement.
    fn covers(&self) -> &[RequirementKind];

    /// The verdict this primitive can reach for `req` on `profile`. Combined by
    /// `meet` across all covering primitives (see [`classify_via_primitives`]).
    fn classify(&self, req: &BoundaryRequirement, profile: &BackendProfile) -> SupportVerdict;

    /// The setup phase this primitive lowers in.
    fn phase(&self) -> LoweringPhase;

    /// Primitives that MUST be lowered before this one (by id). Every id named
    /// here must be present in the compiled set, in this phase or an earlier one.
    fn prerequisites(&self) -> &[PrimitiveId];

    /// Primitives that MUST NOT be composed with this one (by id). A symmetric
    /// relation — declaring it on either side aborts the plan.
    fn conflicts(&self) -> &[PrimitiveId];

    /// Privileges this primitive needs to lower (audit/pre-flight evidence).
    fn required_privileges(&self) -> &[Privilege];

    /// Extract the evidence claims this primitive witnessed from an observed run,
    /// inserting them into `out`. Pure: reads the sealed observations, mutates
    /// only the accumulator. (Effect-free; the observing happened in the backend.)
    fn witness(&self, observed: &BoundaryReportBody, out: &mut EvidenceSet);
}

/// Classify a requirement against a set of primitives, FAIL-CLOSED.
///
/// The classification algebra: the verdict is the `meet` (enforcement floored,
/// evidence intersected) of every primitive that *covers* the requirement's
/// kind. If no primitive covers it, the result is the bottom
/// ([`SupportVerdict::unsupported`]) — an uncovered requirement can never be
/// admitted. Order-independent (`meet` is commutative + associative).
#[must_use]
pub fn classify_via_primitives(
    primitives: &[&dyn PrimitiveDecl],
    req: &BoundaryRequirement,
    profile: &BackendProfile,
) -> SupportVerdict {
    let kind = RequirementKind::of(req);
    let mut verdict: Option<SupportVerdict> = None;
    for primitive in primitives {
        if primitive.covers().contains(&kind) {
            let next = primitive.classify(req, profile);
            verdict = Some(match verdict {
                Some(acc) => acc.meet(&next),
                None => next,
            });
        }
    }
    verdict.unwrap_or_else(SupportVerdict::unsupported)
}

#[cfg(test)]
mod primitive_tests {
    use super::{
        classify_via_primitives, LoweringPhase, PrimitiveDecl, PrimitiveId, PrimitiveVersion,
        Privilege,
    };
    use crate::contract::capability::{Enforcement, EvidenceClaim, EvidenceSet, SupportVerdict};
    use crate::contract::host_control::HostControl;
    use crate::contract::plan::BoundaryRequirement;
    use crate::contract::support::{BackendProfile, RequirementKind};
    use std::collections::BTreeMap;

    /// A minimal in-test primitive: fixed metadata + a fixed classify verdict.
    struct FakePrim {
        id: PrimitiveId,
        version: PrimitiveVersion,
        covers: Vec<RequirementKind>,
        phase: LoweringPhase,
        prereqs: Vec<PrimitiveId>,
        conflicts: Vec<PrimitiveId>,
        privileges: Vec<Privilege>,
        verdict: SupportVerdict,
    }

    impl FakePrim {
        fn new(id: &str, phase: LoweringPhase) -> Self {
            Self {
                id: PrimitiveId::new(id),
                version: PrimitiveVersion::new(1),
                covers: Vec::new(),
                phase,
                prereqs: Vec::new(),
                conflicts: Vec::new(),
                privileges: Vec::new(),
                verdict: SupportVerdict::unsupported(),
            }
        }
        fn covers(mut self, kinds: &[RequirementKind]) -> Self {
            self.covers = kinds.to_vec();
            self
        }
        fn verdict(mut self, verdict: SupportVerdict) -> Self {
            self.verdict = verdict;
            self
        }
    }

    impl PrimitiveDecl for FakePrim {
        fn id(&self) -> PrimitiveId {
            self.id.clone()
        }
        fn version(&self) -> PrimitiveVersion {
            self.version
        }
        fn covers(&self) -> &[RequirementKind] {
            &self.covers
        }
        fn classify(&self, _req: &BoundaryRequirement, _p: &BackendProfile) -> SupportVerdict {
            self.verdict.clone()
        }
        fn phase(&self) -> LoweringPhase {
            self.phase
        }
        fn prerequisites(&self) -> &[PrimitiveId] {
            &self.prereqs
        }
        fn conflicts(&self) -> &[PrimitiveId] {
            &self.conflicts
        }
        fn required_privileges(&self) -> &[Privilege] {
            &self.privileges
        }
        fn witness(
            &self,
            _observed: &crate::contract::report::BoundaryReportBody,
            _out: &mut EvidenceSet,
        ) {
        }
    }

    fn launch_req() -> BoundaryRequirement {
        BoundaryRequirement::HostControl(HostControl::LaunchWorkload)
    }

    fn empty_profile() -> BackendProfile {
        BackendProfile::from_ceiling(BTreeMap::new())
    }

    #[test]
    fn classify_meets_covering_primitives_and_fails_closed_when_uncovered() {
        let profile = empty_profile();
        let req = launch_req();

        // Uncovered → fail-closed bottom.
        let bystander = FakePrim::new("other", LoweringPhase::Launch); // covers nothing
        assert_eq!(
            classify_via_primitives(&[&bystander], &req, &profile).enforcement,
            Enforcement::Unsupported,
        );

        // Two covering primitives: Enforced{TerminalOutcome} meet
        // Mediated{TerminalOutcome,CapturedStreams} = Mediated{TerminalOutcome}
        // (enforcement floored to Mediated, evidence intersected).
        let strong = FakePrim::new("strong", LoweringPhase::Launch)
            .covers(&[RequirementKind::LaunchWorkload])
            .verdict(SupportVerdict::new(
                Enforcement::Enforced,
                [EvidenceClaim::TerminalOutcome].into_iter().collect(),
            ));
        let weak = FakePrim::new("weak", LoweringPhase::Launch)
            .covers(&[RequirementKind::LaunchWorkload])
            .verdict(SupportVerdict::new(
                Enforcement::Mediated,
                [
                    EvidenceClaim::TerminalOutcome,
                    EvidenceClaim::CapturedStreams,
                ]
                .into_iter()
                .collect(),
            ));

        let met = classify_via_primitives(&[&strong, &weak], &req, &profile);
        assert_eq!(
            met.enforcement,
            Enforcement::Mediated,
            "enforcement floored"
        );
        assert!(met.evidence.contains(EvidenceClaim::TerminalOutcome));
        assert!(
            !met.evidence.contains(EvidenceClaim::CapturedStreams),
            "evidence intersected: a claim only one primitive makes is dropped"
        );

        // Order-independent: meet is commutative.
        let met_rev = classify_via_primitives(&[&weak, &strong], &req, &profile);
        assert_eq!(met, met_rev);
    }
}
