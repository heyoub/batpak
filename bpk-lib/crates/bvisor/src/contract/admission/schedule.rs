//! The schedule-validity-and-canonicality membrane — IMPERATIVE reference half.
//!
//! A backend's [`crate::contract::lowering::compile_schedule`] produces a canonical
//! [`crate::contract::lowering::LoweringSchedule`]; the admission object must not
//! TRUST that order — it must VERIFY it, inside the same validated circuit and plan
//! identity, so two valid orders cannot mint two identities from one `(S,P,D)`.
//!
//! This module is the authoritative imperative twin (mirroring
//! [`super::shadow::reference_admission`] for the budget/support/etc. membranes): a
//! total, fail-closed reference that, given the normalized [`ScheduleInputs`], returns
//! the first-failing [`ScheduleRefusal`] in canonical priority order, or admits. The
//! NC¹ circuit and the shadow-parity comparison land next; this is the spec they must
//! reproduce bit-for-bit.
//!
//! ## The model
//!
//! The membrane verifies a SUPPLIED schedule `L` against a TRUSTED declaration set
//! `D` (indexed `0..N`) and the spec's required coverage `R(S)`:
//! - `D[p]` carries the authenticated `phase`, `covers`, `prerequisites`, `conflicts`,
//!   and the canonical `decl_digest` / `param_digest` (what the planner admitted).
//! - `L[i]` names a primitive index plus the digests it CLAIMS for that slot.
//!
//! Identity is reduced to a **small index** `0..N` (the universe is bounded — `N ≤ 64`,
//! so every primitive set is a `u64` bitset). The canonical key is `(phase, index)`:
//! the index is unique, so version never breaks a tie (it is authenticated through
//! `decl_digest`, not used for ordering), exactly as in `compile_schedule`.
//!
//! ## The checks (canonical priority order — the first failure is the refusal)
//!
//! 1. [`ScheduleRefusal::IndexOutOfRange`] — every slot names an index `< N`.
//! 2. [`ScheduleRefusal::DuplicatePrimitive`] — no index appears twice (with (1),
//!    `L` injects into `0..N`).
//! 3. [`ScheduleRefusal::DeclIntegrity`] — each slot's claimed `decl_digest` /
//!    `param_digest` equals the trusted `D[L[i]]` value (authenticates version,
//!    parameters, and the producing profile in one equality).
//! 4. [`ScheduleRefusal::MissingPrerequisite`] — every present primitive's prerequisite
//!    set is `⊆ present` (a prereq outside the scheduled set is dangling).
//! 5. [`ScheduleRefusal::ConflictCoPresent`] — no present primitive declares a conflict
//!    with another present one.
//! 6. [`ScheduleRefusal::PrereqOutOfOrder`] — every prerequisite is placed strictly
//!    BEFORE its dependent (a smuggled cycle cannot be linearized, so it surfaces here).
//! 7. [`ScheduleRefusal::PhaseOutOfOrder`] — phases are non-decreasing along the order.
//! 8. [`ScheduleRefusal::RequirementUncovered`] — the union of `covers` over the
//!    scheduled set is `⊇ R(S)`.
//! 9. [`ScheduleRefusal::NonCanonical`] — the order is the lexicographically-canonical
//!    Kahn order: at each step the smallest-key READY primitive is chosen. Violated iff
//!    some unselected primitive `q` with `key(q) < key(L[i])` is already ready (all its
//!    prerequisites are placed before `i`) yet was passed over.
//!
//! Every check reduces to bounded comparisons, bitset membership, and balanced Boolean
//! reductions over the `N ≤ 64` universe — so the circuit half fits the NC¹ evaluator.

/// The bounded universe size the `u64` bitset encoding supports. A lowering schedule
/// names a handful of primitives; this ceiling is far above any real plan and exists
/// only so the `index → bit` mapping is total without a wider word.
pub const MAX_PRIMITIVES: usize = 64;

/// One declared primitive in the membrane's bounded universe (its position in
/// [`ScheduleInputs::declarations`] is its index `p ∈ 0..N`). This is the TRUSTED
/// declaration the schedule is checked against — the planner authenticated it, and a
/// slot's claimed digest must reproduce `decl_digest` / `param_digest` exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrimitiveDeclInputs {
    /// The lowering phase code (`LoweringPhase::code`, `0..=7`) — the high half of the
    /// canonical key and the phase-ordering subject.
    pub phase: u8,
    /// Requirement-kinds this primitive covers, as a bitset.
    pub covers: u64,
    /// Prerequisite primitives, as a bitset over indices `0..N`.
    pub prerequisites: u64,
    /// Conflicting primitives, as a bitset over indices `0..N`.
    pub conflicts: u64,
    /// The canonical declaration digest the planner admitted (truncated to the lane;
    /// the full 256-bit integrity is the promotion proof's domain, as for the profile
    /// hash in [`super::shadow`]).
    pub decl_digest: u64,
    /// The canonical parameter digest the planner admitted.
    pub param_digest: u64,
}

/// One slot of the supplied schedule `L`: which declared primitive it places, plus the
/// digests it CLAIMS for that slot (authenticated against the trusted declaration).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScheduleSlotInputs {
    /// The placed primitive's index into [`ScheduleInputs::declarations`] (may be
    /// out-of-range adversarially — caught by [`ScheduleRefusal::IndexOutOfRange`]).
    pub primitive: u64,
    /// The declaration digest this slot claims (must equal the trusted value).
    pub claimed_decl_digest: u64,
    /// The parameter digest this slot claims (must equal the trusted value).
    pub claimed_param_digest: u64,
}

/// The normalized, immutable schedule-membrane inputs — the trusted declaration
/// universe `D`, the supplied order `L`, and the spec's required coverage `R(S)`.
/// Probed and normalized ONCE; fed identically to the imperative reference and (next)
/// the shadow circuit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleInputs {
    /// The trusted declaration universe, indexed `0..N` (`N ≤ `[`MAX_PRIMITIVES`]).
    pub declarations: Vec<PrimitiveDeclInputs>,
    /// The supplied schedule `L` — the order under verification.
    pub schedule: Vec<ScheduleSlotInputs>,
    /// The required requirement-kinds `R(S)`, as a bitset.
    pub required: u64,
}

/// Why the schedule membrane refused, in canonical priority order. The numeric
/// [`ScheduleRefusal::code`] is the stable selector the circuit and shadow trace agree
/// on (`0` = admitted).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScheduleRefusal {
    /// A slot names a primitive index `≥ N`.
    IndexOutOfRange,
    /// A primitive index appears in more than one slot.
    DuplicatePrimitive,
    /// A slot's claimed declaration / parameter digest does not match the trusted one.
    DeclIntegrity,
    /// A present primitive requires a prerequisite that is not in the scheduled set.
    MissingPrerequisite,
    /// Two present primitives declare a conflict.
    ConflictCoPresent,
    /// A prerequisite is placed at or after its dependent (a smuggled cycle lands here).
    PrereqOutOfOrder,
    /// Two consecutive slots descend in phase.
    PhaseOutOfOrder,
    /// A required requirement-kind is covered by no scheduled primitive.
    RequirementUncovered,
    /// The order is valid but not the lexicographically-canonical Kahn order.
    NonCanonical,
}

impl ScheduleRefusal {
    /// The stable 1-based refusal code (`0` is reserved for "admitted"). Frozen — the
    /// circuit's refusal-code lane and the shadow trace key off these exact values.
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Self::IndexOutOfRange => 1,
            Self::DuplicatePrimitive => 2,
            Self::DeclIntegrity => 3,
            Self::MissingPrerequisite => 4,
            Self::ConflictCoPresent => 5,
            Self::PrereqOutOfOrder => 6,
            Self::PhaseOutOfOrder => 7,
            Self::RequirementUncovered => 8,
            Self::NonCanonical => 9,
        }
    }

    /// The refusal for a frozen [`ScheduleRefusal::code`], or `None` for `0` (admitted)
    /// or an unknown code — the inverse the shadow uses to read the circuit's lane.
    #[must_use]
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::IndexOutOfRange),
            2 => Some(Self::DuplicatePrimitive),
            3 => Some(Self::DeclIntegrity),
            4 => Some(Self::MissingPrerequisite),
            5 => Some(Self::ConflictCoPresent),
            6 => Some(Self::PrereqOutOfOrder),
            7 => Some(Self::PhaseOutOfOrder),
            8 => Some(Self::RequirementUncovered),
            9 => Some(Self::NonCanonical),
            _ => None,
        }
    }
}

impl std::fmt::Display for ScheduleRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self {
            Self::IndexOutOfRange => "a schedule slot names an out-of-range primitive index",
            Self::DuplicatePrimitive => "a primitive is scheduled more than once",
            Self::DeclIntegrity => {
                "a slot's claimed declaration digest does not match the trusted one"
            }
            Self::MissingPrerequisite => {
                "a scheduled primitive requires an unscheduled prerequisite"
            }
            Self::ConflictCoPresent => "two conflicting primitives are co-scheduled",
            Self::PrereqOutOfOrder => "a prerequisite is scheduled at or after its dependent",
            Self::PhaseOutOfOrder => "the schedule descends in lowering phase",
            Self::RequirementUncovered => {
                "a required requirement-kind is covered by no scheduled primitive"
            }
            Self::NonCanonical => "the schedule is valid but not the canonical Kahn order",
        };
        write!(f, "schedule refusal {}: {reason}", self.code())
    }
}

impl std::error::Error for ScheduleRefusal {}

/// The canonical schedule-membrane decision the imperative reference and the shadow
/// circuit must agree on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScheduleOutcome {
    /// The supplied order is valid, complete, conflict-free, phase-ordered, covering,
    /// and the canonical Kahn order.
    Admitted,
    /// Refused at the first failing check (canonical priority order).
    Refused {
        /// The first-failing reason.
        reason: ScheduleRefusal,
    },
}

impl ScheduleOutcome {
    /// The refusal code (`0` admitted), the stable selector both paths key on.
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Self::Admitted => 0,
            Self::Refused { reason } => reason.code(),
        }
    }
}

/// `1 << i` for an in-range index `i < `[`MAX_PRIMITIVES`]. A wrapping guard keeps the
/// function total without a panic; callers only ever pass authenticated `< N` indices.
fn bit(i: usize) -> u64 {
    1u64.checked_shl(u32::try_from(i).unwrap_or(u32::MAX))
        .unwrap_or(0)
}

/// The authoritative imperative schedule decision over normalized inputs: the
/// first-failing check in canonical priority order, or [`ScheduleOutcome::Admitted`].
/// Total and fail-closed — never panics on adversarial input.
#[must_use]
pub fn reference_schedule_admission(inputs: &ScheduleInputs) -> ScheduleOutcome {
    match schedule_refusal(inputs) {
        Some(reason) => ScheduleOutcome::Refused { reason },
        None => ScheduleOutcome::Admitted,
    }
}

/// The first-failing [`ScheduleRefusal`] in canonical priority order, or `None` when
/// the schedule is admitted. Factored so the circuit-equivalence proof can drive the
/// reference directly. The structural checks (1)–(2) run first and build the verified
/// `present`/`pos` view; the order-sensitive checks (3)–(9) are then one short-circuit
/// chain over that view (each is its own bounded predicate).
#[must_use]
pub fn schedule_refusal(inputs: &ScheduleInputs) -> Option<ScheduleRefusal> {
    let verified = match Verified::build(inputs) {
        Ok(verified) => verified,
        Err(reason) => return Some(reason),
    };
    verified
        .decl_integrity()
        .or_else(|| verified.prereq_closure())
        .or_else(|| verified.conflict_freedom())
        .or_else(|| verified.prereq_order())
        .or_else(|| verified.phase_order())
        .or_else(|| verified.coverage())
        .or_else(|| verified.canonicality())
}

/// A schedule whose structural checks (1) in-range and (2) distinct have passed, so
/// the scheduled set `present` and the position map `pos` are well-defined. The
/// order/integrity/coverage/canonicality checks are methods over this view.
struct Verified<'a> {
    decls: &'a [PrimitiveDeclInputs],
    schedule: &'a [ScheduleSlotInputs],
    required: u64,
    /// Bitset of scheduled primitive indices.
    present: u64,
    /// `pos[p]` = the slot index of primitive `p` (`usize::MAX` if unscheduled).
    pos: Vec<usize>,
}

impl<'a> Verified<'a> {
    /// Run the structural checks (1) in-range and (2) distinct, building the verified
    /// view — or the first structural [`ScheduleRefusal`].
    fn build(inputs: &'a ScheduleInputs) -> Result<Self, ScheduleRefusal> {
        let n = inputs.declarations.len();
        let schedule = &inputs.schedule;

        // (1) In-range: every slot indexes a declared primitive.
        let n_u64 = u64::try_from(n).unwrap_or(u64::MAX);
        if schedule.iter().any(|slot| slot.primitive >= n_u64) {
            return Err(ScheduleRefusal::IndexOutOfRange);
        }

        // (2) Distinct: no index is scheduled twice. With (1) this makes L inject into
        // 0..N, so `present` is exactly the scheduled set and `pos` is well-defined.
        let mut present = 0u64;
        let mut pos = vec![usize::MAX; n];
        for (i, slot) in schedule.iter().enumerate() {
            let p = slot_index(slot);
            if present & bit(p) != 0 {
                return Err(ScheduleRefusal::DuplicatePrimitive);
            }
            present |= bit(p);
            pos[p] = i;
        }

        Ok(Self {
            decls: &inputs.declarations,
            schedule,
            required: inputs.required,
            present,
            pos,
        })
    }

    /// (3) Each slot's claimed digests match the trusted declaration — authenticates
    /// version, parameters, and the producing profile in one equality.
    fn decl_integrity(&self) -> Option<ScheduleRefusal> {
        let bad = self.schedule.iter().any(|slot| {
            let decl = &self.decls[slot_index(slot)];
            slot.claimed_decl_digest != decl.decl_digest
                || slot.claimed_param_digest != decl.param_digest
        });
        bad.then_some(ScheduleRefusal::DeclIntegrity)
    }

    /// (4) Every present primitive's prerequisites are themselves present.
    fn prereq_closure(&self) -> Option<ScheduleRefusal> {
        let dangling = self
            .schedule
            .iter()
            .any(|slot| self.decls[slot_index(slot)].prerequisites & !self.present != 0);
        dangling.then_some(ScheduleRefusal::MissingPrerequisite)
    }

    /// (5) No present primitive conflicts with another present one.
    fn conflict_freedom(&self) -> Option<ScheduleRefusal> {
        let clash = self.schedule.iter().any(|slot| {
            let p = slot_index(slot);
            self.decls[p].conflicts & (self.present & !bit(p)) != 0
        });
        clash.then_some(ScheduleRefusal::ConflictCoPresent)
    }

    /// (6) Every present prerequisite is placed strictly earlier than its dependent.
    /// A smuggled cycle cannot be linearized, so one of its edges lands at/after here.
    fn prereq_order(&self) -> Option<ScheduleRefusal> {
        let out_of_order = self.schedule.iter().enumerate().any(|(i, slot)| {
            let prereqs = self.decls[slot_index(slot)].prerequisites & self.present;
            !prereqs_before(prereqs, &self.pos, i)
        });
        out_of_order.then_some(ScheduleRefusal::PrereqOutOfOrder)
    }

    /// (7) Phases are non-decreasing along the schedule.
    fn phase_order(&self) -> Option<ScheduleRefusal> {
        let descends = self.schedule.windows(2).any(|window| {
            self.decls[slot_index(&window[0])].phase > self.decls[slot_index(&window[1])].phase
        });
        descends.then_some(ScheduleRefusal::PhaseOutOfOrder)
    }

    /// (8) The union of `covers` over the scheduled set ⊇ `R(S)`.
    fn coverage(&self) -> Option<ScheduleRefusal> {
        let covered = self
            .schedule
            .iter()
            .fold(0u64, |acc, slot| acc | self.decls[slot_index(slot)].covers);
        (self.required & !covered != 0).then_some(ScheduleRefusal::RequirementUncovered)
    }

    /// (9) The lexicographically-canonical Kahn order: at step `i`, no unselected
    /// primitive `q` with `key(q) < key(L[i])` may already be ready.
    fn canonicality(&self) -> Option<ScheduleRefusal> {
        let non_canonical = self
            .schedule
            .iter()
            .enumerate()
            .any(|(i, slot)| self.passed_over_a_ready_smaller(slot_index(slot), i));
        non_canonical.then_some(ScheduleRefusal::NonCanonical)
    }

    /// Whether some still-unselected `q` with a smaller canonical key than the slot
    /// chosen at step `i` (`chosen`) was already ready — the canonicality violation.
    fn passed_over_a_ready_smaller(&self, chosen: usize, i: usize) -> bool {
        let key_chosen = (self.decls[chosen].phase, chosen);
        let mut rest = self.present;
        while rest != 0 {
            let q = trailing_index(rest);
            rest &= rest - 1;
            if self.pos[q] <= i {
                continue; // already selected (or `chosen` itself)
            }
            let key_q = (self.decls[q].phase, q);
            let ready = prereqs_before(self.decls[q].prerequisites & self.present, &self.pos, i);
            if key_q < key_chosen && ready {
                return true;
            }
        }
        false
    }
}

/// A slot's primitive index as a `usize` (in-range by [`Verified::build`] check (1)).
fn slot_index(slot: &ScheduleSlotInputs) -> usize {
    usize::try_from(slot.primitive).unwrap_or(usize::MAX)
}

/// Whether every prerequisite in `prereqs` (a bitset over present indices) is placed
/// at a position strictly less than `before` — the shared core of the prerequisite
/// order check (6) and the "ready at step i" predicate inside canonicality (9).
fn prereqs_before(mut prereqs: u64, pos: &[usize], before: usize) -> bool {
    while prereqs != 0 {
        let r = trailing_index(prereqs);
        prereqs &= prereqs - 1;
        if pos[r] >= before {
            return false;
        }
    }
    true
}

/// The index of the lowest set bit of a non-zero bitset.
fn trailing_index(bits: u64) -> usize {
    usize::try_from(bits.trailing_zeros()).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod schedule_tests {
    use super::{
        reference_schedule_admission, schedule_refusal, PrimitiveDeclInputs, ScheduleInputs,
        ScheduleOutcome, ScheduleRefusal, ScheduleSlotInputs,
    };

    /// Phase codes used in the fixtures (mirror `LoweringPhase::code`).
    const NS: u8 = 0; // NamespaceCreate
    const FS: u8 = 1; // FsSetup

    /// A canonical three-primitive universe + schedule:
    /// - p0: phase NS, covers bit0, no prereq.
    /// - p1: phase FS, covers bit1, prereq {p0}.
    /// - p2: phase FS, covers bit2, no prereq.
    ///   Canonical Kahn order is `[p0, p1, p2]` (keys (NS,0) < (FS,1) < (FS,2), p1 ready
    ///   only after p0). All three requirement bits required.
    fn canonical() -> ScheduleInputs {
        ScheduleInputs {
            declarations: vec![
                PrimitiveDeclInputs {
                    phase: NS,
                    covers: 0b001,
                    prerequisites: 0,
                    conflicts: 0,
                    decl_digest: 0xA0,
                    param_digest: 0xB0,
                },
                PrimitiveDeclInputs {
                    phase: FS,
                    covers: 0b010,
                    prerequisites: 0b001, // needs p0
                    conflicts: 0,
                    decl_digest: 0xA1,
                    param_digest: 0xB1,
                },
                PrimitiveDeclInputs {
                    phase: FS,
                    covers: 0b100,
                    prerequisites: 0,
                    conflicts: 0,
                    decl_digest: 0xA2,
                    param_digest: 0xB2,
                },
            ],
            schedule: vec![
                ScheduleSlotInputs {
                    primitive: 0,
                    claimed_decl_digest: 0xA0,
                    claimed_param_digest: 0xB0,
                },
                ScheduleSlotInputs {
                    primitive: 1,
                    claimed_decl_digest: 0xA1,
                    claimed_param_digest: 0xB1,
                },
                ScheduleSlotInputs {
                    primitive: 2,
                    claimed_decl_digest: 0xA2,
                    claimed_param_digest: 0xB2,
                },
            ],
            required: 0b111,
        }
    }

    #[test]
    fn canonical_schedule_is_admitted() {
        assert_eq!(schedule_refusal(&canonical()), None);
        assert_eq!(
            reference_schedule_admission(&canonical()),
            ScheduleOutcome::Admitted
        );
        assert_eq!(canonical().required, 0b111);
    }

    #[test]
    fn empty_schedule_with_no_requirements_is_admitted() {
        let inputs = ScheduleInputs {
            declarations: vec![],
            schedule: vec![],
            required: 0,
        };
        assert_eq!(
            reference_schedule_admission(&inputs),
            ScheduleOutcome::Admitted
        );
    }

    fn assert_refused(inputs: &ScheduleInputs, reason: ScheduleRefusal) {
        assert_eq!(
            reference_schedule_admission(inputs),
            ScheduleOutcome::Refused { reason },
            "expected refusal {reason} (code {})",
            reason.code()
        );
    }

    #[test]
    fn out_of_range_index_fails_closed() {
        let mut inputs = canonical();
        inputs.schedule[2].primitive = 9; // ≥ N = 3
        assert_refused(&inputs, ScheduleRefusal::IndexOutOfRange);
    }

    #[test]
    fn duplicate_primitive_fails_closed() {
        let mut inputs = canonical();
        inputs.schedule[2].primitive = 0; // p0 placed twice
        inputs.schedule[2].claimed_decl_digest = 0xA0;
        inputs.schedule[2].claimed_param_digest = 0xB0;
        assert_refused(&inputs, ScheduleRefusal::DuplicatePrimitive);
    }

    #[test]
    fn stale_decl_digest_fails_closed() {
        let mut inputs = canonical();
        inputs.schedule[0].claimed_decl_digest = 0xFF; // ≠ trusted 0xA0
        assert_refused(&inputs, ScheduleRefusal::DeclIntegrity);
    }

    #[test]
    fn stale_param_digest_fails_closed() {
        let mut inputs = canonical();
        inputs.schedule[1].claimed_param_digest = 0xFF; // ≠ trusted 0xB1
        assert_refused(&inputs, ScheduleRefusal::DeclIntegrity);
    }

    #[test]
    fn missing_prerequisite_fails_closed() {
        let mut inputs = canonical();
        // p1 now requires p0 AND p3 (bit3), which is not in the universe / present set.
        inputs.declarations[1].prerequisites = 0b001 | (1 << 3);
        assert_refused(&inputs, ScheduleRefusal::MissingPrerequisite);
    }

    #[test]
    fn conflicting_primitives_fail_closed() {
        let mut inputs = canonical();
        inputs.declarations[0].conflicts = 0b100; // p0 conflicts with present p2
        assert_refused(&inputs, ScheduleRefusal::ConflictCoPresent);
    }

    #[test]
    fn prerequisite_after_dependent_fails_closed() {
        let mut inputs = canonical();
        // Place p1 (needs p0) before p0: [p1, p0, p2].
        inputs.schedule = vec![inputs.schedule[1], inputs.schedule[0], inputs.schedule[2]];
        assert_refused(&inputs, ScheduleRefusal::PrereqOutOfOrder);
    }

    #[test]
    fn smuggled_cycle_fails_closed() {
        let mut inputs = canonical();
        // Make p0 ⇄ p1 a 2-cycle (both phase FS so phase order cannot pre-empt it).
        inputs.declarations[0].phase = FS;
        inputs.declarations[0].prerequisites = 0b010; // p0 needs p1
                                                      // p1 already needs p0. No linearization places both before each other.
        assert_refused(&inputs, ScheduleRefusal::PrereqOutOfOrder);
    }

    #[test]
    fn phase_inversion_fails_closed() {
        let mut inputs = canonical();
        // [p2(FS), p0(NS), p1(FS)] — valid prereqs (p1 after p0) but FS→NS descends.
        inputs.schedule = vec![inputs.schedule[2], inputs.schedule[0], inputs.schedule[1]];
        assert_refused(&inputs, ScheduleRefusal::PhaseOutOfOrder);
    }

    #[test]
    fn uncovered_requirement_fails_closed() {
        let mut inputs = canonical();
        inputs.required = 0b1000; // bit3 — covered by no primitive
        assert_refused(&inputs, ScheduleRefusal::RequirementUncovered);
    }

    #[test]
    fn valid_but_noncanonical_order_fails_closed() {
        let mut inputs = canonical();
        // [p0, p2, p1] is valid (prereqs ok, phases NS≤FS≤FS, covering) but at step 1
        // p1 (key (FS,1)) is ready and smaller than the chosen p2 (key (FS,2)).
        inputs.schedule = vec![inputs.schedule[0], inputs.schedule[2], inputs.schedule[1]];
        assert_refused(&inputs, ScheduleRefusal::NonCanonical);
    }

    #[test]
    fn refusal_codes_are_the_frozen_priority_order() {
        assert_eq!(ScheduleRefusal::IndexOutOfRange.code(), 1);
        assert_eq!(ScheduleRefusal::DuplicatePrimitive.code(), 2);
        assert_eq!(ScheduleRefusal::DeclIntegrity.code(), 3);
        assert_eq!(ScheduleRefusal::MissingPrerequisite.code(), 4);
        assert_eq!(ScheduleRefusal::ConflictCoPresent.code(), 5);
        assert_eq!(ScheduleRefusal::PrereqOutOfOrder.code(), 6);
        assert_eq!(ScheduleRefusal::PhaseOutOfOrder.code(), 7);
        assert_eq!(ScheduleRefusal::RequirementUncovered.code(), 8);
        assert_eq!(ScheduleRefusal::NonCanonical.code(), 9);
        assert_eq!(ScheduleOutcome::Admitted.code(), 0);
        // `from_code` is the exact inverse on 1..=9; 0 and unknown codes are `None`.
        for code in 1u8..=9 {
            let reason = ScheduleRefusal::from_code(code).expect("1..=9 round-trips");
            assert_eq!(reason.code(), code);
        }
        assert_eq!(ScheduleRefusal::from_code(0), None);
        assert_eq!(ScheduleRefusal::from_code(10), None);
    }

    #[test]
    fn priority_order_reports_the_earliest_failure() {
        // An input that violates BOTH conflict (5) and canonicality (9) must report the
        // higher-priority conflict.
        let mut inputs = canonical();
        inputs.declarations[0].conflicts = 0b100; // conflict (5)
        inputs.schedule = vec![inputs.schedule[0], inputs.schedule[2], inputs.schedule[1]]; // also non-canonical (9)
        assert_refused(&inputs, ScheduleRefusal::ConflictCoPresent);
    }
}
