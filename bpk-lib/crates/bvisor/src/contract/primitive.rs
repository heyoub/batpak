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
use std::collections::{BTreeMap, BTreeSet};

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
/// Object-safe so a backend can hold its primitives as `&[&dyn ConfinePrimitive]`
/// and feed them to [`compile_lowering_plan`] / [`classify_via_primitives`].
pub trait ConfinePrimitive {
    /// This primitive's stable identity (for prerequisite/conflict relations).
    fn id(&self) -> PrimitiveId;

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
    primitives: &[&dyn ConfinePrimitive],
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

/// A validated, ordered sequence of primitive ids to lower.
///
/// Constructed ONLY by [`compile_lowering_plan`], so possessing one is proof the
/// sequence is phase-ordered, prerequisite-respecting, conflict-free, and
/// acyclic. The runner lowers `steps()` in order; rollback walks them in reverse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoweringPlan {
    steps: Vec<PrimitiveId>,
}

impl LoweringPlan {
    /// The primitive ids in validated lowering order.
    #[must_use]
    pub fn steps(&self) -> &[PrimitiveId] {
        &self.steps
    }

    /// Number of steps.
    #[must_use]
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Whether the plan lowers nothing (a no-confinement plan).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// Why a set of primitives could not be compiled into a [`LoweringPlan`]. The
/// compiler fails closed: any inconsistency aborts rather than emitting a partial
/// or out-of-order plan.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LoweringError {
    /// Two primitives share the same [`PrimitiveId`].
    DuplicatePrimitive {
        /// The duplicated id.
        id: PrimitiveId,
    },
    /// A primitive names a prerequisite that is not in the compiled set.
    MissingPrerequisite {
        /// The primitive declaring the prerequisite.
        primitive: PrimitiveId,
        /// The absent prerequisite id.
        missing: PrimitiveId,
    },
    /// Two composed primitives declare a conflict (named on either side).
    ConflictingPrimitives {
        /// The lexicographically smaller id.
        a: PrimitiveId,
        /// The lexicographically larger id.
        b: PrimitiveId,
    },
    /// A prerequisite lowers in a LATER phase than the primitive that needs it —
    /// a contradiction (a dependency cannot run after its dependent).
    PhaseOrderViolation {
        /// The prerequisite primitive.
        prerequisite: PrimitiveId,
        /// Its (later) phase.
        prerequisite_phase: LoweringPhase,
        /// The dependent primitive.
        dependent: PrimitiveId,
        /// Its (earlier) phase.
        dependent_phase: LoweringPhase,
    },
    /// The prerequisite graph contains a cycle; the named primitives are the ones
    /// that could not be ordered.
    CyclicDependency {
        /// The primitives left unordered (in id order).
        involved: Vec<PrimitiveId>,
    },
}

impl std::fmt::Display for LoweringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicatePrimitive { id } => write!(f, "duplicate confine primitive {id}"),
            Self::MissingPrerequisite { primitive, missing } => write!(
                f,
                "primitive {primitive} requires {missing}, which is not in the compiled set"
            ),
            Self::ConflictingPrimitives { a, b } => {
                write!(f, "primitives {a} and {b} conflict and cannot be composed")
            }
            Self::PhaseOrderViolation {
                prerequisite,
                prerequisite_phase,
                dependent,
                dependent_phase,
            } => write!(
                f,
                "prerequisite {prerequisite} (phase {prerequisite_phase:?}) lowers after its \
                 dependent {dependent} (phase {dependent_phase:?})"
            ),
            Self::CyclicDependency { involved } => {
                write!(f, "cyclic prerequisite dependency among {involved:?}")
            }
        }
    }
}

impl std::error::Error for LoweringError {}

/// Compile a set of primitives into a validated [`LoweringPlan`], FAIL-CLOSED.
///
/// Validation, in order:
/// 1. duplicate ids → [`LoweringError::DuplicatePrimitive`];
/// 2. declared conflicts present together → [`LoweringError::ConflictingPrimitives`];
/// 3. prerequisite absent → [`LoweringError::MissingPrerequisite`];
/// 4. prerequisite in a later phase than its dependent →
///    [`LoweringError::PhaseOrderViolation`];
/// 5. topological sort over the prerequisite edges, breaking ties by
///    `(phase, id)` so phase order governs independent primitives and the result
///    is deterministic; a remaining cycle → [`LoweringError::CyclicDependency`].
///
/// # Errors
/// Any [`LoweringError`] above.
pub fn compile_lowering_plan(
    primitives: &[&dyn ConfinePrimitive],
) -> Result<LoweringPlan, LoweringError> {
    // 1. Index by id; reject duplicates.
    let mut by_id: BTreeMap<PrimitiveId, &dyn ConfinePrimitive> = BTreeMap::new();
    for primitive in primitives {
        if by_id.insert(primitive.id(), *primitive).is_some() {
            return Err(LoweringError::DuplicatePrimitive { id: primitive.id() });
        }
    }

    // 2. Conflicts (symmetric): id-sorted iteration → deterministic first hit.
    for (id, primitive) in &by_id {
        for other in primitive.conflicts() {
            if by_id.contains_key(other) {
                let (a, b) = if id <= other {
                    (id.clone(), other.clone())
                } else {
                    (other.clone(), id.clone())
                };
                return Err(LoweringError::ConflictingPrimitives { a, b });
            }
        }
    }

    // 3 + 4. Prerequisites present and phase-consistent (dedup per node).
    let mut prereqs: BTreeMap<PrimitiveId, BTreeSet<PrimitiveId>> = BTreeMap::new();
    for (id, primitive) in &by_id {
        let mut set = BTreeSet::new();
        for pre in primitive.prerequisites() {
            let Some(pre_primitive) = by_id.get(pre) else {
                return Err(LoweringError::MissingPrerequisite {
                    primitive: id.clone(),
                    missing: pre.clone(),
                });
            };
            if pre_primitive.phase() > primitive.phase() {
                return Err(LoweringError::PhaseOrderViolation {
                    prerequisite: pre.clone(),
                    prerequisite_phase: pre_primitive.phase(),
                    dependent: id.clone(),
                    dependent_phase: primitive.phase(),
                });
            }
            set.insert(pre.clone());
        }
        prereqs.insert(id.clone(), set);
    }

    // 5. Kahn's topological sort; ready set ordered by (phase, id).
    let mut indegree: BTreeMap<PrimitiveId, usize> =
        by_id.keys().map(|id| (id.clone(), 0usize)).collect();
    let mut dependents: BTreeMap<PrimitiveId, Vec<PrimitiveId>> = BTreeMap::new();
    for (id, set) in &prereqs {
        for pre in set {
            *indegree.get_mut(id).expect("id is in the set") += 1;
            dependents.entry(pre.clone()).or_default().push(id.clone());
        }
    }

    let mut ready: BTreeSet<(LoweringPhase, PrimitiveId)> = indegree
        .iter()
        .filter(|(_, deg)| **deg == 0)
        .map(|(id, _)| (by_id[id].phase(), id.clone()))
        .collect();

    let mut steps = Vec::with_capacity(by_id.len());
    while let Some(entry) = ready.iter().next().cloned() {
        ready.remove(&entry);
        let (_, id) = entry;
        if let Some(children) = dependents.get(&id) {
            for child in children {
                let deg = indegree.get_mut(child).expect("child is in the set");
                *deg -= 1;
                if *deg == 0 {
                    ready.insert((by_id[child].phase(), child.clone()));
                }
            }
        }
        steps.push(id);
    }

    if steps.len() != by_id.len() {
        let involved = indegree
            .iter()
            .filter(|(_, deg)| **deg > 0)
            .map(|(id, _)| id.clone())
            .collect();
        return Err(LoweringError::CyclicDependency { involved });
    }

    Ok(LoweringPlan { steps })
}

#[cfg(test)]
mod primitive_tests {
    use super::{
        classify_via_primitives, compile_lowering_plan, ConfinePrimitive, LoweringError,
        LoweringPhase, LoweringPlan, PrimitiveId, Privilege,
    };
    use crate::contract::capability::{Enforcement, EvidenceClaim, EvidenceSet, SupportVerdict};
    use crate::contract::host_control::HostControl;
    use crate::contract::plan::BoundaryRequirement;
    use crate::contract::support::{BackendProfile, RequirementKind};
    use std::collections::BTreeMap;

    /// A minimal in-test primitive: fixed metadata + a fixed classify verdict.
    struct FakePrim {
        id: PrimitiveId,
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
                covers: Vec::new(),
                phase,
                prereqs: Vec::new(),
                conflicts: Vec::new(),
                privileges: Vec::new(),
                verdict: SupportVerdict::unsupported(),
            }
        }
        fn prereqs(mut self, ids: &[&str]) -> Self {
            self.prereqs = ids.iter().map(|i| PrimitiveId::new(*i)).collect();
            self
        }
        fn conflicts(mut self, ids: &[&str]) -> Self {
            self.conflicts = ids.iter().map(|i| PrimitiveId::new(*i)).collect();
            self
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

    impl ConfinePrimitive for FakePrim {
        fn id(&self) -> PrimitiveId {
            self.id.clone()
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

    fn ids(plan: &LoweringPlan) -> Vec<&str> {
        plan.steps().iter().map(PrimitiveId::as_str).collect()
    }

    fn launch_req() -> BoundaryRequirement {
        BoundaryRequirement::HostControl(HostControl::LaunchWorkload)
    }

    fn empty_profile() -> BackendProfile {
        BackendProfile::from_ceiling(BTreeMap::new())
    }

    #[test]
    fn empty_set_compiles_to_an_empty_plan() {
        let plan = compile_lowering_plan(&[]).expect("empty set is valid");
        assert!(plan.is_empty());
        assert_eq!(plan.len(), 0);
    }

    #[test]
    fn independent_primitives_order_by_phase_then_id() {
        // Declared out of order; the compiler sorts by (phase, id).
        let launch = FakePrim::new("z_launch", LoweringPhase::Launch);
        let ns = FakePrim::new("a_ns", LoweringPhase::NamespaceCreate);
        let policy = FakePrim::new("m_policy", LoweringPhase::PolicyInstall);
        let plan = compile_lowering_plan(&[&launch, &ns, &policy]).expect("valid");
        assert_eq!(ids(&plan), ["a_ns", "m_policy", "z_launch"]);
    }

    #[test]
    fn same_phase_ties_break_by_id_deterministically() {
        let b = FakePrim::new("b", LoweringPhase::FsSetup);
        let a = FakePrim::new("a", LoweringPhase::FsSetup);
        let c = FakePrim::new("c", LoweringPhase::FsSetup);
        let plan = compile_lowering_plan(&[&b, &a, &c]).expect("valid");
        assert_eq!(ids(&plan), ["a", "b", "c"]);
    }

    #[test]
    fn prerequisite_forces_order_within_a_phase() {
        // Same phase, but "second" requires "first" → first lowers first even
        // though its id sorts later.
        let first = FakePrim::new("zzz_first", LoweringPhase::PolicyInstall);
        let second =
            FakePrim::new("aaa_second", LoweringPhase::PolicyInstall).prereqs(&["zzz_first"]);
        let plan = compile_lowering_plan(&[&second, &first]).expect("valid");
        assert_eq!(ids(&plan), ["zzz_first", "aaa_second"]);
    }

    #[test]
    fn duplicate_ids_fail_closed() {
        let one = FakePrim::new("dup", LoweringPhase::FsSetup);
        let two = FakePrim::new("dup", LoweringPhase::Launch);
        let err = compile_lowering_plan(&[&one, &two]).expect_err("duplicate");
        assert_eq!(
            err,
            LoweringError::DuplicatePrimitive {
                id: PrimitiveId::new("dup")
            }
        );
    }

    #[test]
    fn missing_prerequisite_fails_closed() {
        let p = FakePrim::new("needs", LoweringPhase::Launch).prereqs(&["absent"]);
        let err = compile_lowering_plan(&[&p]).expect_err("missing prereq");
        assert_eq!(
            err,
            LoweringError::MissingPrerequisite {
                primitive: PrimitiveId::new("needs"),
                missing: PrimitiveId::new("absent"),
            }
        );
    }

    #[test]
    fn conflicting_primitives_fail_closed_with_sorted_pair() {
        // Conflict declared on only one side still aborts; pair is id-sorted.
        let z = FakePrim::new("z", LoweringPhase::FsSetup).conflicts(&["a"]);
        let a = FakePrim::new("a", LoweringPhase::FsSetup);
        let err = compile_lowering_plan(&[&z, &a]).expect_err("conflict");
        assert_eq!(
            err,
            LoweringError::ConflictingPrimitives {
                a: PrimitiveId::new("a"),
                b: PrimitiveId::new("z"),
            }
        );
    }

    #[test]
    fn prerequisite_in_a_later_phase_fails_closed() {
        // "early" (NamespaceCreate) requires "late" (Launch) — impossible.
        let early = FakePrim::new("early", LoweringPhase::NamespaceCreate).prereqs(&["late"]);
        let late = FakePrim::new("late", LoweringPhase::Launch);
        let err = compile_lowering_plan(&[&early, &late]).expect_err("phase order");
        assert_eq!(
            err,
            LoweringError::PhaseOrderViolation {
                prerequisite: PrimitiveId::new("late"),
                prerequisite_phase: LoweringPhase::Launch,
                dependent: PrimitiveId::new("early"),
                dependent_phase: LoweringPhase::NamespaceCreate,
            }
        );
    }

    #[test]
    fn cyclic_prerequisites_fail_closed() {
        // a → b → a within one phase (so no phase-order short-circuit fires).
        let a = FakePrim::new("a", LoweringPhase::PolicyInstall).prereqs(&["b"]);
        let b = FakePrim::new("b", LoweringPhase::PolicyInstall).prereqs(&["a"]);
        let err = compile_lowering_plan(&[&a, &b]).expect_err("cycle");
        assert_eq!(
            err,
            LoweringError::CyclicDependency {
                involved: vec![PrimitiveId::new("a"), PrimitiveId::new("b")],
            }
        );
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
