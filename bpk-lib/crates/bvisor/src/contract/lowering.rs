//! The canonical lowering schedule: the backend-produced, bvisor-validated link
//! between pure [`PrimitiveDecl`]s and effectful execution.
//!
//! Split from `primitive.rs` (declaration vs schedule). A backend selects the
//! primitives a `(spec, profile)` needs and calls [`compile_schedule`] — the
//! deterministic, version-aware Kahn compiler `K:(S,P,D)→L`. The result is a
//! **canonical** [`LoweringSchedule`]: ordered [`ScheduleEntry`]s, each carrying the
//! declaration digest that binds it to the EXACT [`PrimitiveDecl`] it came from.
//!
//! The admission circuit does NOT search for an order — it **validates** a supplied
//! schedule (validity + canonicality) so two valid orders cannot mint two identities
//! from identical inputs. That membrane lands next; this module is the shape it
//! verifies and the `H_L` it folds into plan identity.

use crate::contract::ids::Digest32;
use crate::contract::primitive::{LoweringPhase, PrimitiveDecl, PrimitiveId, PrimitiveVersion};
use std::collections::{BTreeMap, BTreeSet};

/// Domain separator for a primitive declaration digest. Frozen.
const DECL_DIGEST_DOMAIN: &str = "bvisor.primitive-decl.v1";
/// Domain separator for a primitive parameter digest. Frozen.
const PARAM_DIGEST_DOMAIN: &str = "bvisor.primitive-params.v1";
/// Domain separator for the whole-schedule digest `H_L`. Frozen.
const SCHEDULE_DIGEST_DOMAIN: &str = "bvisor.lowering.v1";

/// One entry of a canonical [`LoweringSchedule`].
///
/// `(id, version)` identifies the primitive; `phase` is its setup phase;
/// `decl_digest` binds the entry to the EXACT declaration (so a stale/forged decl
/// behind a known id is caught by the membrane); `param_digest` binds the primitive
/// instance's canonical parameters (no primitive carries parameters yet — every
/// entry shares the empty-parameter digest until they do).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleEntry {
    id: PrimitiveId,
    version: PrimitiveVersion,
    phase: LoweringPhase,
    param_digest: Digest32,
    decl_digest: Digest32,
}

impl ScheduleEntry {
    /// The primitive's stable id.
    #[must_use]
    pub fn id(&self) -> &PrimitiveId {
        &self.id
    }

    /// The declaration revision this entry was compiled from.
    #[must_use]
    pub fn version(&self) -> PrimitiveVersion {
        self.version
    }

    /// The setup phase.
    #[must_use]
    pub fn phase(&self) -> LoweringPhase {
        self.phase
    }

    /// Digest of the primitive instance's canonical parameters.
    #[must_use]
    pub fn param_digest(&self) -> &Digest32 {
        &self.param_digest
    }

    /// Digest of the EXACT declaration this entry was compiled from.
    #[must_use]
    pub fn decl_digest(&self) -> &Digest32 {
        &self.decl_digest
    }
}

/// A serializable view of a [`ScheduleEntry`] for canonical digesting — the newtypes
/// are reduced to their frozen wire shape (`as_str`, `get`, `code`, raw digests).
#[derive(serde::Serialize)]
struct EntryDigestView<'a> {
    id: &'a str,
    version: u32,
    phase: u8,
    param_digest: &'a [u8; 32],
    decl_digest: &'a [u8; 32],
}

impl ScheduleEntry {
    fn digest_view(&self) -> EntryDigestView<'_> {
        EntryDigestView {
            id: self.id.as_str(),
            version: self.version.get(),
            phase: self.phase.code(),
            param_digest: &self.param_digest,
            decl_digest: &self.decl_digest,
        }
    }
}

/// The canonical parameter digest for a parameterless primitive. Frozen: every
/// schedule entry shares it until primitives carry parameters, at which point the
/// per-instance parameter bytes fold in here.
#[must_use]
fn empty_param_digest() -> Digest32 {
    #[derive(serde::Serialize)]
    struct ParamDigestInput<'a> {
        domain: &'a str,
        params: (),
    }
    let input = ParamDigestInput {
        domain: PARAM_DIGEST_DOMAIN,
        params: (),
    };
    // `()` + a frozen domain string always canonicalize; the unwrap is unreachable.
    let bytes = batpak::canonical::to_bytes(&input).unwrap_or_default();
    batpak::event::hash::compute_hash(&bytes)
}

/// Digest the EXACT identity-relevant declaration of `decl`: id, version, phase, and
/// the SORTED coverage / prerequisite / conflict sets (sorted so the digest is
/// independent of declaration order — the membrane recomputes it the same way).
///
/// # Errors
/// [`LoweringError::CanonicalEncoding`] if canonical encoding fails (the inputs are
/// plain strings + integers, so this is effectively unreachable).
fn decl_digest(decl: &dyn PrimitiveDecl) -> Result<Digest32, LoweringError> {
    let mut covers: Vec<_> = decl.covers().to_vec();
    covers.sort_unstable();
    covers.dedup();
    let mut prerequisites: Vec<&str> = decl
        .prerequisites()
        .iter()
        .map(PrimitiveId::as_str)
        .collect();
    prerequisites.sort_unstable();
    prerequisites.dedup();
    let mut conflicts: Vec<&str> = decl.conflicts().iter().map(PrimitiveId::as_str).collect();
    conflicts.sort_unstable();
    conflicts.dedup();

    #[derive(serde::Serialize)]
    struct DeclDigestInput<'a> {
        domain: &'a str,
        id: &'a str,
        version: u32,
        phase: u8,
        covers: Vec<crate::contract::support::RequirementKind>,
        prerequisites: Vec<&'a str>,
        conflicts: Vec<&'a str>,
    }
    let id = decl.id();
    let input = DeclDigestInput {
        domain: DECL_DIGEST_DOMAIN,
        id: id.as_str(),
        version: decl.version().get(),
        phase: decl.phase().code(),
        covers,
        prerequisites,
        conflicts,
    };
    let bytes = batpak::canonical::to_bytes(&input)
        .map_err(|e| LoweringError::CanonicalEncoding(e.to_string()))?;
    Ok(batpak::event::hash::compute_hash(&bytes))
}

/// A validated, ordered, canonical schedule of primitives to lower.
///
/// Constructed ONLY by [`compile_schedule`], so possessing one is proof the sequence
/// is phase-ordered, prerequisite-respecting, conflict-free, acyclic, AND the
/// lexicographically-canonical Kahn order for its inputs. The runner lowers
/// [`LoweringSchedule::steps`] in order; rollback walks them in reverse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoweringSchedule {
    entries: Vec<ScheduleEntry>,
}

impl LoweringSchedule {
    /// The ordered schedule entries.
    #[must_use]
    pub fn entries(&self) -> &[ScheduleEntry] {
        &self.entries
    }

    /// The primitive ids in validated lowering order (execution convenience).
    pub fn steps(&self) -> impl Iterator<Item = &PrimitiveId> {
        self.entries.iter().map(ScheduleEntry::id)
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the schedule lowers nothing (a no-confinement plan).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The canonical schedule digest `H_L = H("bvisor.lowering.v1" ‖ canonical(L))` —
    /// folded into plan identity once the circuit is promoted.
    ///
    /// # Errors
    /// [`LoweringError::CanonicalEncoding`] if canonical encoding fails (unreachable
    /// for the frozen wire shape).
    pub fn digest(&self) -> Result<Digest32, LoweringError> {
        #[derive(serde::Serialize)]
        struct ScheduleDigestInput<'a> {
            domain: &'a str,
            entries: Vec<EntryDigestView<'a>>,
        }
        let input = ScheduleDigestInput {
            domain: SCHEDULE_DIGEST_DOMAIN,
            entries: self
                .entries
                .iter()
                .map(ScheduleEntry::digest_view)
                .collect(),
        };
        let bytes = batpak::canonical::to_bytes(&input)
            .map_err(|e| LoweringError::CanonicalEncoding(e.to_string()))?;
        Ok(batpak::event::hash::compute_hash(&bytes))
    }
}

/// Why a set of primitives could not be compiled into a [`LoweringSchedule`]. The
/// compiler fails closed: any inconsistency aborts rather than emitting a partial or
/// out-of-order schedule.
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
    /// A prerequisite lowers in a LATER phase than the primitive that needs it.
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
    /// The prerequisite graph contains a cycle; the named primitives could not be
    /// ordered.
    CyclicDependency {
        /// The primitives left unordered (in id order).
        involved: Vec<PrimitiveId>,
    },
    /// A declaration could not be canonically encoded for its digest (the rendered
    /// encoder error — a `String` so [`LoweringError`] stays `Clone + PartialEq`).
    CanonicalEncoding(String),
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
            Self::CanonicalEncoding(err) => {
                write!(
                    f,
                    "could not canonically encode a primitive declaration: {err}"
                )
            }
        }
    }
}

impl std::error::Error for LoweringError {}

/// Compile selected primitives into a canonical, validated [`LoweringSchedule`] —
/// the deterministic backend-side compiler `K:(S,P,D)→L`, FAIL-CLOSED.
///
/// Validation, in order: duplicate ids · declared conflicts present together ·
/// prerequisite absent · prerequisite in a later phase than its dependent · Kahn
/// topological sort with the canonical ready key `(phase, id, version)` (smallest
/// ready key selected each step), a remaining cycle aborting. The emitted order IS
/// the lexicographically-canonical Kahn order, which the admission membrane later
/// re-verifies rather than trusts.
///
/// # Errors
/// Any [`LoweringError`].
pub fn compile_schedule(
    primitives: &[&dyn PrimitiveDecl],
) -> Result<LoweringSchedule, LoweringError> {
    // 1. Index by id; reject duplicates.
    let mut by_id: BTreeMap<PrimitiveId, &dyn PrimitiveDecl> = BTreeMap::new();
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

    // 5. Kahn's topological sort; ready set keyed by the canonical (phase, id,
    // version) — smallest ready key first. (Ids are unique, so version never breaks a
    // tie; it is in the key to match the canonical schedule ordering exactly.)
    let key = |id: &PrimitiveId| -> (LoweringPhase, PrimitiveId, PrimitiveVersion) {
        let decl = by_id[id];
        (decl.phase(), id.clone(), decl.version())
    };
    let mut indegree: BTreeMap<PrimitiveId, usize> =
        by_id.keys().map(|id| (id.clone(), 0usize)).collect();
    let mut dependents: BTreeMap<PrimitiveId, Vec<PrimitiveId>> = BTreeMap::new();
    for (id, set) in &prereqs {
        for pre in set {
            *indegree.get_mut(id).expect("id is in the set") += 1;
            dependents.entry(pre.clone()).or_default().push(id.clone());
        }
    }

    let mut ready: BTreeSet<(LoweringPhase, PrimitiveId, PrimitiveVersion)> = indegree
        .iter()
        .filter(|(_, deg)| **deg == 0)
        .map(|(id, _)| key(id))
        .collect();

    let mut ordered: Vec<PrimitiveId> = Vec::with_capacity(by_id.len());
    while let Some(entry) = ready.iter().next().cloned() {
        ready.remove(&entry);
        let (_, id, _) = entry;
        if let Some(children) = dependents.get(&id) {
            for child in children {
                let deg = indegree.get_mut(child).expect("child is in the set");
                *deg -= 1;
                if *deg == 0 {
                    ready.insert(key(child));
                }
            }
        }
        ordered.push(id);
    }

    if ordered.len() != by_id.len() {
        let involved = indegree
            .iter()
            .filter(|(_, deg)| **deg > 0)
            .map(|(id, _)| id.clone())
            .collect();
        return Err(LoweringError::CyclicDependency { involved });
    }

    // 6. Emit canonical entries (declaration digest binds each to its exact decl).
    let param_digest = empty_param_digest();
    let mut entries = Vec::with_capacity(ordered.len());
    for id in ordered {
        let decl = by_id[&id];
        entries.push(ScheduleEntry {
            id: decl.id(),
            version: decl.version(),
            phase: decl.phase(),
            param_digest,
            decl_digest: decl_digest(decl)?,
        });
    }
    Ok(LoweringSchedule { entries })
}

#[cfg(test)]
mod lowering_tests {
    use super::{compile_schedule, LoweringError, LoweringSchedule};
    use crate::contract::capability::{EvidenceSet, SupportVerdict};
    use crate::contract::plan::BoundaryRequirement;
    use crate::contract::primitive::{LoweringPhase, PrimitiveDecl, PrimitiveId, PrimitiveVersion};
    use crate::contract::report::BoundaryReportBody;
    use crate::contract::support::{BackendProfile, RequirementKind};

    /// A minimal in-test declaration: fixed metadata, unsupported verdict.
    struct FakeDecl {
        id: PrimitiveId,
        version: PrimitiveVersion,
        covers: Vec<RequirementKind>,
        phase: LoweringPhase,
        prereqs: Vec<PrimitiveId>,
        conflicts: Vec<PrimitiveId>,
    }

    impl FakeDecl {
        fn new(id: &str, phase: LoweringPhase) -> Self {
            Self {
                id: PrimitiveId::new(id),
                version: PrimitiveVersion::new(1),
                covers: Vec::new(),
                phase,
                prereqs: Vec::new(),
                conflicts: Vec::new(),
            }
        }
        fn version(mut self, v: u32) -> Self {
            self.version = PrimitiveVersion::new(v);
            self
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
    }

    impl PrimitiveDecl for FakeDecl {
        fn id(&self) -> PrimitiveId {
            self.id.clone()
        }
        fn version(&self) -> PrimitiveVersion {
            self.version
        }
        fn covers(&self) -> &[RequirementKind] {
            &self.covers
        }
        fn classify(&self, _r: &BoundaryRequirement, _p: &BackendProfile) -> SupportVerdict {
            SupportVerdict::unsupported()
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
        fn required_privileges(&self) -> &[crate::contract::primitive::Privilege] {
            &[]
        }
        fn witness(&self, _o: &BoundaryReportBody, _out: &mut EvidenceSet) {}
    }

    fn ids(schedule: &LoweringSchedule) -> Vec<&str> {
        schedule.steps().map(PrimitiveId::as_str).collect()
    }

    #[test]
    fn empty_set_compiles_to_an_empty_schedule() {
        let schedule = compile_schedule(&[]).expect("empty set is valid");
        assert!(schedule.is_empty());
        assert_eq!(schedule.len(), 0);
    }

    #[test]
    fn independent_primitives_order_by_phase_then_id() {
        let launch = FakeDecl::new("z_launch", LoweringPhase::Launch);
        let ns = FakeDecl::new("a_ns", LoweringPhase::NamespaceCreate);
        let policy = FakeDecl::new("m_policy", LoweringPhase::PolicyInstall);
        let schedule = compile_schedule(&[&launch, &ns, &policy]).expect("valid");
        assert_eq!(ids(&schedule), ["a_ns", "m_policy", "z_launch"]);
    }

    #[test]
    fn same_phase_ties_break_by_id_deterministically() {
        let b = FakeDecl::new("b", LoweringPhase::FsSetup);
        let a = FakeDecl::new("a", LoweringPhase::FsSetup);
        let c = FakeDecl::new("c", LoweringPhase::FsSetup);
        let schedule = compile_schedule(&[&b, &a, &c]).expect("valid");
        assert_eq!(ids(&schedule), ["a", "b", "c"]);
    }

    #[test]
    fn prerequisite_forces_order_within_a_phase() {
        let first = FakeDecl::new("zzz_first", LoweringPhase::PolicyInstall);
        let second =
            FakeDecl::new("aaa_second", LoweringPhase::PolicyInstall).prereqs(&["zzz_first"]);
        let schedule = compile_schedule(&[&second, &first]).expect("valid");
        assert_eq!(ids(&schedule), ["zzz_first", "aaa_second"]);
    }

    #[test]
    fn duplicate_ids_fail_closed() {
        let one = FakeDecl::new("dup", LoweringPhase::FsSetup);
        let two = FakeDecl::new("dup", LoweringPhase::Launch);
        let err = compile_schedule(&[&one, &two]).expect_err("duplicate");
        assert_eq!(
            err,
            LoweringError::DuplicatePrimitive {
                id: PrimitiveId::new("dup")
            }
        );
    }

    #[test]
    fn missing_prerequisite_fails_closed() {
        let p = FakeDecl::new("needs", LoweringPhase::Launch).prereqs(&["absent"]);
        let err = compile_schedule(&[&p]).expect_err("missing prereq");
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
        let z = FakeDecl::new("z", LoweringPhase::FsSetup).conflicts(&["a"]);
        let a = FakeDecl::new("a", LoweringPhase::FsSetup);
        let err = compile_schedule(&[&z, &a]).expect_err("conflict");
        assert_eq!(
            err,
            LoweringError::ConflictingPrimitives {
                a: PrimitiveId::new("a"),
                b: PrimitiveId::new("z")
            }
        );
    }

    #[test]
    fn prerequisite_in_a_later_phase_fails_closed() {
        let early = FakeDecl::new("early", LoweringPhase::NamespaceCreate).prereqs(&["late"]);
        let late = FakeDecl::new("late", LoweringPhase::Launch);
        let err = compile_schedule(&[&early, &late]).expect_err("phase order");
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
        let a = FakeDecl::new("a", LoweringPhase::PolicyInstall).prereqs(&["b"]);
        let b = FakeDecl::new("b", LoweringPhase::PolicyInstall).prereqs(&["a"]);
        let err = compile_schedule(&[&a, &b]).expect_err("cycle");
        assert_eq!(
            err,
            LoweringError::CyclicDependency {
                involved: vec![PrimitiveId::new("a"), PrimitiveId::new("b")],
            }
        );
    }

    #[test]
    fn decl_digest_is_order_independent_over_covers_prereqs_conflicts() {
        // Two declarations identical up to the ORDER of covers/prereqs/conflicts must
        // produce the same entry decl_digest (the digest sorts before hashing).
        let pre_x = FakeDecl::new("x", LoweringPhase::NamespaceCreate);
        let pre_y = FakeDecl::new("y", LoweringPhase::NamespaceCreate);
        let a = FakeDecl::new("p", LoweringPhase::Launch)
            .covers(&[
                RequirementKind::LaunchWorkload,
                RequirementKind::CaptureStreams,
            ])
            .prereqs(&["x", "y"]);
        let b = FakeDecl::new("p", LoweringPhase::Launch)
            .covers(&[
                RequirementKind::CaptureStreams,
                RequirementKind::LaunchWorkload,
            ])
            .prereqs(&["y", "x"]);
        let sa = compile_schedule(&[&a, &pre_x, &pre_y]).expect("valid");
        let sb = compile_schedule(&[&b, &pre_x, &pre_y]).expect("valid");
        let da = sa
            .entries()
            .iter()
            .find(|e| e.id().as_str() == "p")
            .expect("p")
            .decl_digest();
        let db = sb
            .entries()
            .iter()
            .find(|e| e.id().as_str() == "p")
            .expect("p")
            .decl_digest();
        assert_eq!(da, db, "decl_digest is independent of declaration order");
    }

    #[test]
    fn decl_digest_changes_with_version() {
        let v1 = FakeDecl::new("p", LoweringPhase::Launch).version(1);
        let v2 = FakeDecl::new("p", LoweringPhase::Launch).version(2);
        let s1 = compile_schedule(&[&v1]).expect("valid");
        let s2 = compile_schedule(&[&v2]).expect("valid");
        assert_ne!(
            s1.entries()[0].decl_digest(),
            s2.entries()[0].decl_digest(),
            "a version bump changes the declaration digest"
        );
    }

    #[test]
    fn schedule_digest_is_deterministic_and_param_digest_is_shared_empty() {
        let a = FakeDecl::new("a", LoweringPhase::NamespaceCreate);
        let b = FakeDecl::new("b", LoweringPhase::Launch);
        let s1 = compile_schedule(&[&a, &b]).expect("valid");
        let s2 = compile_schedule(&[&b, &a]).expect("valid");
        assert_eq!(
            s1, s2,
            "compilation is deterministic regardless of input order"
        );
        assert_eq!(
            s1.digest().expect("H_L"),
            s2.digest().expect("H_L"),
            "H_L is deterministic"
        );
        // No primitive carries parameters yet → every entry shares the empty digest.
        assert_eq!(
            s1.entries()[0].param_digest(),
            s1.entries()[1].param_digest()
        );
    }
}
