//! The schedule-validity-and-canonicality membrane — NC¹ CIRCUIT half + shadow parity.
//!
//! [`compile_schedule_membrane`] builds the bounded admission circuit that reproduces
//! [`super::schedule::schedule_refusal`] over encoded lanes; [`schedule_shadow_check`]
//! runs both paths over the identical normalized [`ScheduleInputs`] and returns the
//! authoritative imperative outcome, or a typed [`ScheduleDivergence`] hard finding.
//! The circuit is a SHADOW (the imperative reference is authoritative); it carries no
//! durable identity until validated, proven equivalent, and promoted.
//!
//! ## How the checks fit the NC¹ vocabulary
//!
//! The op set has no multi-bit OR, so every set-membership and position predicate is a
//! **1-bit OR-reduction over slot-index equalities** rather than a bitset union:
//! - `eq[i][q]   = Eq(slot_index[i], q)` — slot `i` places primitive `q`.
//! - `present[q] = OR_i eq[i][q]` — `q` is scheduled.
//! - `before[i][q] = OR_{j<i} eq[j][q]` — `q` is placed strictly before slot `i`.
//! - `upto[i][q]   = before[i][q] OR eq[i][q]` — `q` is placed at or before slot `i`.
//!
//! The permutation indirection `D[L[i]]` (a runtime gather) is a **select-chain keyed by
//! `Eq(slot_index[i], q)`** over the declared field lanes; an out-of-range slot matches
//! no `q` and gathers `0` (benign — the in-range check refuses it first). Bit `r` of a
//! lane is tested with `BitsetSubset(1<<r, lane)`. Phase/coverage/key comparisons are
//! `Compare`/`Eq`. Reductions are balanced [`CircuitBuilder::and_reduce`] /
//! [`CircuitBuilder::or_reduce`] — logarithmic depth.
//!
//! Lane widths are **derived losslessly** from the actual input values (see
//! [`shape_of`]), so the circuit equals the imperative reference EXACTLY over the full
//! `u64` input domain — not a truncated model. The nine check bits, ordered by priority,
//! feed [`compose_membranes`]; its priority-encoded first-failing index IS the
//! [`ScheduleRefusal::code`] (`1..=9`, `0` = admit).

use super::compile::{compose_membranes, CircuitBuilder};
use super::eval::{evaluate, Lane};
use super::program::{AdmissionProgram, NodeId, Outputs, ProgramError, Width, MAX_WIDTH};
use super::schedule::{
    reference_schedule_admission, PrimitiveDeclInputs, ScheduleInputs, ScheduleOutcome,
    ScheduleRefusal, ScheduleSlotInputs,
};

/// The fixed shape + derived lane widths a compiled schedule circuit reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScheduleShape {
    /// Number of declared primitives `N`.
    pub declarations: usize,
    /// Number of schedule slots `K`.
    pub slots: usize,
    /// Width of an index lane (holds slot indices, `0..=N`, and universe constants).
    pub index_width: Width,
    /// Width of a phase lane.
    pub phase_width: Width,
    /// Width of a digest lane.
    pub digest_width: Width,
    /// Width of the prerequisite / conflict bitset universe `M` (`M ≥ N`).
    pub universe_width: Width,
    /// Width of the coverage bitset (requirement-kinds `R`).
    pub covers_width: Width,
}

impl ScheduleShape {
    fn universe(self) -> usize {
        usize::from(self.universe_width.get())
    }
    fn covers(self) -> usize {
        usize::from(self.covers_width.get())
    }
}

/// The number of bits needed to represent `value` (at least 1, capped at `MAX_WIDTH`).
fn bits_for(value: u64) -> Width {
    let bits = 64u32.saturating_sub(value.leading_zeros()).max(1);
    let capped = u16::try_from(bits).unwrap_or(MAX_WIDTH).min(MAX_WIDTH);
    Width::new(capped).expect("1..=MAX_WIDTH")
}

/// One past the highest set bit of `bits` (`0` for an empty set) — the bitset width
/// needed to represent it losslessly.
fn bitset_span(bits: u64) -> usize {
    if bits == 0 {
        0
    } else {
        usize::try_from(64 - bits.leading_zeros()).unwrap_or(0)
    }
}

/// `value`'s low `width` bits as the little-endian byte lane a [`Lane`]/constant wants.
fn le_bytes(value: u64, width: Width) -> Vec<u8> {
    let n = usize::from(width.get()).div_ceil(8);
    value.to_le_bytes().into_iter().take(n).collect()
}

fn lane(value: u64, width: Width) -> Lane {
    Lane::from_le_bytes(&le_bytes(value, width), width)
}

/// Derive the lossless [`ScheduleShape`] for `inputs`: every lane is wide enough to
/// hold the largest value it carries, so circuit and reference agree on the exact
/// `u64` domain (no truncation modeling).
#[must_use]
pub fn shape_of(inputs: &ScheduleInputs) -> ScheduleShape {
    let n = inputs.declarations.len();
    let k = inputs.schedule.len();

    let mut prereq_conflict = 0u64;
    let mut covers_required = inputs.required;
    let mut max_phase = 0u64;
    let mut max_digest = 0u64;
    for decl in &inputs.declarations {
        prereq_conflict |= decl.prerequisites | decl.conflicts;
        covers_required |= decl.covers;
        max_phase = max_phase.max(u64::from(decl.phase));
        max_digest = max_digest.max(decl.decl_digest).max(decl.param_digest);
    }
    let mut max_index = n as u64;
    for slot in &inputs.schedule {
        max_index = max_index.max(slot.primitive);
        max_digest = max_digest
            .max(slot.claimed_decl_digest)
            .max(slot.claimed_param_digest);
    }

    let universe = n
        .max(bitset_span(prereq_conflict))
        .clamp(1, usize::from(MAX_WIDTH));
    let covers = bitset_span(covers_required).clamp(1, usize::from(MAX_WIDTH));
    max_index = max_index.max((universe - 1) as u64);

    ScheduleShape {
        declarations: n,
        slots: k,
        index_width: bits_for(max_index),
        phase_width: bits_for(max_phase),
        digest_width: bits_for(max_digest),
        universe_width: Width::new(u16::try_from(universe).unwrap_or(MAX_WIDTH))
            .expect("1..=MAX_WIDTH"),
        covers_width: Width::new(u16::try_from(covers).unwrap_or(MAX_WIDTH))
            .expect("1..=MAX_WIDTH"),
    }
}

/// The declared input-lane nodes, in the canonical grouped order [`encode`] mirrors.
struct Lanes {
    phase: Vec<NodeId>,
    covers: Vec<NodeId>,
    prereq: Vec<NodeId>,
    conflict: Vec<NodeId>,
    decl_digest: Vec<NodeId>,
    param_digest: Vec<NodeId>,
    slot_index: Vec<NodeId>,
    slot_decl: Vec<NodeId>,
    slot_param: Vec<NodeId>,
    required: NodeId,
}

fn declare_inputs(builder: &mut CircuitBuilder, shape: &ScheduleShape) -> Lanes {
    let decls =
        |b: &mut CircuitBuilder, w: Width| (0..shape.declarations).map(|_| b.input(w)).collect();
    let slots = |b: &mut CircuitBuilder, w: Width| (0..shape.slots).map(|_| b.input(w)).collect();
    Lanes {
        phase: decls(builder, shape.phase_width),
        covers: decls(builder, shape.covers_width),
        prereq: decls(builder, shape.universe_width),
        conflict: decls(builder, shape.universe_width),
        decl_digest: decls(builder, shape.digest_width),
        param_digest: decls(builder, shape.digest_width),
        slot_index: slots(builder, shape.index_width),
        slot_decl: slots(builder, shape.digest_width),
        slot_param: slots(builder, shape.digest_width),
        required: builder.input(shape.covers_width),
    }
}

/// The encoded input lanes for `inputs`, in the exact order [`declare_inputs`] reads.
fn encode(inputs: &ScheduleInputs, shape: &ScheduleShape) -> Vec<Lane> {
    let mut lanes = Vec::new();
    let push_decls = |lanes: &mut Vec<Lane>, f: &dyn Fn(&PrimitiveDeclInputs) -> u64, w: Width| {
        lanes.extend(inputs.declarations.iter().map(|d| lane(f(d), w)));
    };
    push_decls(&mut lanes, &|d| u64::from(d.phase), shape.phase_width);
    push_decls(&mut lanes, &|d| d.covers, shape.covers_width);
    push_decls(&mut lanes, &|d| d.prerequisites, shape.universe_width);
    push_decls(&mut lanes, &|d| d.conflicts, shape.universe_width);
    push_decls(&mut lanes, &|d| d.decl_digest, shape.digest_width);
    push_decls(&mut lanes, &|d| d.param_digest, shape.digest_width);
    let push_slots = |lanes: &mut Vec<Lane>, f: &dyn Fn(&ScheduleSlotInputs) -> u64, w: Width| {
        lanes.extend(inputs.schedule.iter().map(|s| lane(f(s), w)));
    };
    push_slots(&mut lanes, &|s| s.primitive, shape.index_width);
    push_slots(&mut lanes, &|s| s.claimed_decl_digest, shape.digest_width);
    push_slots(&mut lanes, &|s| s.claimed_param_digest, shape.digest_width);
    lanes.push(lane(inputs.required, shape.covers_width));
    lanes
}

/// Derived nodes shared across checks: the equality matrix, the membership prefix
/// predicates, and the per-slot field gathers.
struct Derived {
    /// `present[q]` for `q ∈ 0..M`.
    present: Vec<NodeId>,
    /// `before[i][q]` for `i ∈ 0..K`, `q ∈ 0..M`.
    before: Vec<Vec<NodeId>>,
    /// `upto[i][q]` for `i ∈ 0..K`, `q ∈ 0..M`.
    upto: Vec<Vec<NodeId>>,
    /// Per-slot gathered `D[L[i]]` phase.
    gphase: Vec<NodeId>,
    /// Per-slot gathered `D[L[i]]` prerequisite bitset.
    gprereq: Vec<NodeId>,
    /// Per-slot gathered `D[L[i]]` declaration digest.
    gdecl: Vec<NodeId>,
    /// Per-slot gathered `D[L[i]]` parameter digest.
    gparam: Vec<NodeId>,
}

/// `value` as a constant lane of `width`.
fn konst(builder: &mut CircuitBuilder, value: u64, width: Width) -> NodeId {
    builder.constant(le_bytes(value, width), width)
}

/// `BitsetSubset(1<<index, lane)` — whether bit `index` is set in `lane` (`index <
/// width`, enforced by the caller's loop bounds).
fn bit_set(builder: &mut CircuitBuilder, index: usize, target: NodeId, width: Width) -> NodeId {
    let mask = konst(builder, 1u64 << index, width);
    builder.bitset_subset(mask, target)
}

/// Gather `D[L[i]].field` as a select-chain keyed by `Eq(slot_index, q)` over the
/// declared `field` lanes; an out-of-range slot matches no `q` and yields `0`.
fn gather(
    builder: &mut CircuitBuilder,
    slot_index: NodeId,
    fields: &[NodeId],
    index_width: Width,
    field_width: Width,
) -> NodeId {
    let mut acc = konst(builder, 0, field_width);
    for (q, &field) in fields.iter().enumerate() {
        let q_const = konst(builder, q as u64, index_width);
        let is_q = builder.equal(slot_index, q_const);
        acc = builder.select(is_q, field, acc, field_width);
    }
    acc
}

fn build_derived(builder: &mut CircuitBuilder, lanes: &Lanes, shape: &ScheduleShape) -> Derived {
    let m = shape.universe();
    // eq[i][q] = Eq(slot_index[i], q); present/before/upto are 1-bit reductions of it.
    let mut before = vec![Vec::with_capacity(m); shape.slots];
    let mut upto = vec![Vec::with_capacity(m); shape.slots];
    let mut present = Vec::with_capacity(m);
    for q in 0..m {
        let q_const = konst(builder, q as u64, shape.index_width);
        let mut running_before = konst(builder, 0, Width::one());
        for i in 0..shape.slots {
            let eq = builder.equal(lanes.slot_index[i], q_const);
            before[i].push(running_before);
            let up = builder.or(running_before, eq);
            upto[i].push(up);
            running_before = up;
        }
        present.push(running_before); // OR over all slots = upto[K-1] (or 0 if K==0)
    }

    let gphase = build_gathers(builder, lanes, shape, &lanes.phase, shape.phase_width);
    let gprereq = build_gathers(builder, lanes, shape, &lanes.prereq, shape.universe_width);
    let gdecl = build_gathers(
        builder,
        lanes,
        shape,
        &lanes.decl_digest,
        shape.digest_width,
    );
    let gparam = build_gathers(
        builder,
        lanes,
        shape,
        &lanes.param_digest,
        shape.digest_width,
    );
    Derived {
        present,
        before,
        upto,
        gphase,
        gprereq,
        gdecl,
        gparam,
    }
}

fn build_gathers(
    builder: &mut CircuitBuilder,
    lanes: &Lanes,
    shape: &ScheduleShape,
    fields: &[NodeId],
    field_width: Width,
) -> Vec<NodeId> {
    (0..shape.slots)
        .map(|i| {
            gather(
                builder,
                lanes.slot_index[i],
                fields,
                shape.index_width,
                field_width,
            )
        })
        .collect()
}

// --- the nine checks, in canonical priority order; each returns its 1-bit pass node.

/// (1) every slot indexes a declared primitive: `slot_index[i] < N`.
fn check_in_range(builder: &mut CircuitBuilder, lanes: &Lanes, shape: &ScheduleShape) -> NodeId {
    let n_const = konst(builder, shape.declarations as u64, shape.index_width);
    let bits: Vec<NodeId> = lanes
        .slot_index
        .iter()
        .map(|&idx| builder.compare_ult(idx, n_const))
        .collect();
    builder.and_reduce(&bits)
}

/// (2) no index is scheduled twice: pairwise `slot_index[i] != slot_index[j]`.
fn check_distinct(builder: &mut CircuitBuilder, lanes: &Lanes) -> NodeId {
    let mut bits = Vec::new();
    for i in 0..lanes.slot_index.len() {
        for j in (i + 1)..lanes.slot_index.len() {
            let eq = builder.equal(lanes.slot_index[i], lanes.slot_index[j]);
            bits.push(builder.not(eq));
        }
    }
    builder.and_reduce(&bits)
}

/// (3) each slot's claimed digests match the gathered trusted declaration.
fn check_decl_integrity(builder: &mut CircuitBuilder, lanes: &Lanes, d: &Derived) -> NodeId {
    let bits: Vec<NodeId> = (0..lanes.slot_index.len())
        .map(|i| {
            let decl_ok = builder.equal(d.gdecl[i], lanes.slot_decl[i]);
            let param_ok = builder.equal(d.gparam[i], lanes.slot_param[i]);
            builder.and(decl_ok, param_ok)
        })
        .collect();
    builder.and_reduce(&bits)
}

/// (4) every present primitive's prerequisites are present.
fn check_prereq_closure(
    builder: &mut CircuitBuilder,
    lanes: &Lanes,
    d: &Derived,
    shape: &ScheduleShape,
) -> NodeId {
    let bits: Vec<NodeId> = (0..shape.declarations)
        .map(|q| {
            let satisfied = implies_all_bits(builder, lanes.prereq[q], shape, &d.present);
            let absent = builder.not(d.present[q]);
            builder.or(absent, satisfied)
        })
        .collect();
    builder.and_reduce(&bits)
}

/// `AND_r ( ¬bit_r(bitset) ∨ guard[r] )` over the universe — the shared "every set bit
/// implies its guard" reduction used by closure (4) and readiness (9).
fn implies_all_bits(
    builder: &mut CircuitBuilder,
    bitset: NodeId,
    shape: &ScheduleShape,
    guard: &[NodeId],
) -> NodeId {
    let bits: Vec<NodeId> = (0..shape.universe())
        .map(|r| {
            let has = bit_set(builder, r, bitset, shape.universe_width);
            let not_has = builder.not(has);
            builder.or(not_has, guard[r])
        })
        .collect();
    builder.and_reduce(&bits)
}

/// (5) no present primitive conflicts with another present one.
fn check_conflict_free(
    builder: &mut CircuitBuilder,
    lanes: &Lanes,
    d: &Derived,
    shape: &ScheduleShape,
) -> NodeId {
    let bits: Vec<NodeId> = (0..shape.declarations)
        .map(|q| {
            let no_clash: Vec<NodeId> = (0..shape.declarations)
                .filter(|&other| other != q)
                .map(|other| {
                    let has = bit_set(builder, other, lanes.conflict[q], shape.universe_width);
                    let not_has = builder.not(has);
                    let not_present = builder.not(d.present[other]);
                    builder.or(not_has, not_present)
                })
                .collect();
            let clean = builder.and_reduce(&no_clash);
            let absent = builder.not(d.present[q]);
            builder.or(absent, clean)
        })
        .collect();
    builder.and_reduce(&bits)
}

/// (6) every present prerequisite is placed strictly before its dependent slot.
fn check_prereq_order(builder: &mut CircuitBuilder, d: &Derived, shape: &ScheduleShape) -> NodeId {
    let bits: Vec<NodeId> = (0..shape.slots)
        .map(|i| implies_all_bits(builder, d.gprereq[i], shape, &d.before[i]))
        .collect();
    builder.and_reduce(&bits)
}

/// (7) phases are non-decreasing along the schedule.
fn check_phase_order(builder: &mut CircuitBuilder, d: &Derived) -> NodeId {
    let bits: Vec<NodeId> = d
        .gphase
        .windows(2)
        .map(|w| builder.compare_ule(w[0], w[1]))
        .collect();
    builder.and_reduce(&bits)
}

/// (8) the union of `covers` over the scheduled set ⊇ `R(S)`.
fn check_coverage(
    builder: &mut CircuitBuilder,
    lanes: &Lanes,
    d: &Derived,
    shape: &ScheduleShape,
) -> NodeId {
    let bits: Vec<NodeId> = (0..shape.covers())
        .map(|c| {
            let required_c = bit_set(builder, c, lanes.required, shape.covers_width);
            let covered_by: Vec<NodeId> = (0..shape.declarations)
                .map(|q| {
                    let covers_c = bit_set(builder, c, lanes.covers[q], shape.covers_width);
                    builder.and(d.present[q], covers_c)
                })
                .collect();
            let covered = builder.or_reduce(&covered_by);
            let not_required = builder.not(required_c);
            builder.or(not_required, covered)
        })
        .collect();
    builder.and_reduce(&bits)
}

/// (9) the lexicographically-canonical Kahn order: at step `i`, no unselected `q` with
/// `key(q) < key(L[i])` is already ready. Pass = no such violation exists.
fn check_canonical(
    builder: &mut CircuitBuilder,
    lanes: &Lanes,
    d: &Derived,
    shape: &ScheduleShape,
) -> NodeId {
    let mut violations = Vec::new();
    for i in 0..shape.slots {
        for q in 0..shape.declarations {
            let unselected = builder.not(d.upto[i][q]);
            let ready = implies_all_bits(builder, lanes.prereq[q], shape, &d.before[i]);
            let key_lt = key_less(builder, lanes, d, shape, q, i);
            let pu = builder.and(d.present[q], unselected);
            let rk = builder.and(ready, key_lt);
            violations.push(builder.and(pu, rk));
        }
    }
    let any = builder.or_reduce(&violations);
    builder.not(any)
}

/// `key(q) < key(L[i])` with `key = (phase, index)`: phase first, the constant index
/// `q` breaking ties against the slot's index.
fn key_less(
    builder: &mut CircuitBuilder,
    lanes: &Lanes,
    d: &Derived,
    shape: &ScheduleShape,
    q: usize,
    i: usize,
) -> NodeId {
    let phase_lt = builder.compare_ult(lanes.phase[q], d.gphase[i]);
    let phase_eq = builder.equal(lanes.phase[q], d.gphase[i]);
    let q_const = konst(builder, q as u64, shape.index_width);
    let index_lt = builder.compare_ult(q_const, lanes.slot_index[i]);
    let tie = builder.and(phase_eq, index_lt);
    builder.or(phase_lt, tie)
}

/// Compile the schedule membrane: admit iff the supplied schedule passes all nine
/// checks; the refusal code is the first-failing check's [`ScheduleRefusal::code`].
///
/// # Errors
/// [`ProgramError`] only on `u32` node-index overflow, which a membrane never hits.
pub fn compile_schedule_membrane(shape: &ScheduleShape) -> Result<AdmissionProgram, ProgramError> {
    let mut builder = CircuitBuilder::new();
    let lanes = declare_inputs(&mut builder, shape);
    let derived = build_derived(&mut builder, &lanes, shape);
    let checks = [
        check_in_range(&mut builder, &lanes, shape),
        check_distinct(&mut builder, &lanes),
        check_decl_integrity(&mut builder, &lanes, &derived),
        check_prereq_closure(&mut builder, &lanes, &derived, shape),
        check_conflict_free(&mut builder, &lanes, &derived, shape),
        check_prereq_order(&mut builder, &derived, shape),
        check_phase_order(&mut builder, &derived),
        check_coverage(&mut builder, &lanes, &derived, shape),
        check_canonical(&mut builder, &lanes, &derived, shape),
    ];
    let (admit, refusal_code) = compose_membranes(&mut builder, &checks);
    builder.finish(Outputs {
        admit,
        refusal_code,
        membranes: checks.to_vec(),
    })
}

/// A typed schedule-membrane shadow disagreement — a hard gauntlet finding.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScheduleDivergence {
    /// The two paths produced different outcomes.
    OutcomeMismatch {
        /// The authoritative imperative outcome.
        reference: ScheduleOutcome,
        /// The shadow circuit outcome.
        circuit: ScheduleOutcome,
    },
    /// The shadow circuit failed to compile/evaluate where the reference did not.
    CircuitError {
        /// The authoritative imperative outcome.
        reference: ScheduleOutcome,
        /// Why the circuit path failed.
        reason: &'static str,
    },
}

impl std::fmt::Display for ScheduleDivergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutcomeMismatch { reference, circuit } => write!(
                f,
                "schedule divergence: reference {reference:?} != circuit {circuit:?}"
            ),
            Self::CircuitError { reference, reason } => write!(
                f,
                "schedule divergence: circuit failed ({reason}) where reference was {reference:?}"
            ),
        }
    }
}

impl std::error::Error for ScheduleDivergence {}

/// The shadow circuit's schedule decision: compile for the derived shape, evaluate the
/// encoded lanes, and map the refusal code back to a [`ScheduleOutcome`].
fn circuit_schedule_admission(inputs: &ScheduleInputs) -> Result<ScheduleOutcome, &'static str> {
    let shape = shape_of(inputs);
    let program = compile_schedule_membrane(&shape).map_err(|_| "circuit compilation failed")?;
    let decision =
        evaluate(&program, &encode(inputs, &shape)).map_err(|_| "circuit evaluation failed")?;
    if decision.admit {
        return Ok(ScheduleOutcome::Admitted);
    }
    let code = u8::try_from(decision.refusal_code).unwrap_or(0);
    match ScheduleRefusal::from_code(code) {
        Some(reason) => Ok(ScheduleOutcome::Refused { reason }),
        None => Err("circuit refused with an unknown code"),
    }
}

/// Compare the authoritative reference against the shadow circuit. The pure comparison
/// core of [`schedule_shadow_check`], factored so the detector can be proven to fire.
fn decide(
    reference: ScheduleOutcome,
    circuit: Result<ScheduleOutcome, &'static str>,
) -> Result<ScheduleOutcome, ScheduleDivergence> {
    match circuit {
        Ok(circuit) if circuit == reference => Ok(reference),
        Ok(circuit) => Err(ScheduleDivergence::OutcomeMismatch { reference, circuit }),
        Err(reason) => Err(ScheduleDivergence::CircuitError { reference, reason }),
    }
}

/// Run both paths over the identical normalized inputs and return the authoritative
/// outcome — or a typed [`ScheduleDivergence`] if the shadow circuit disagrees.
///
/// # Errors
/// [`ScheduleDivergence`] on any outcome mismatch, or if the circuit fails to
/// compile/evaluate where the reference did not.
pub fn schedule_shadow_check(
    inputs: &ScheduleInputs,
) -> Result<ScheduleOutcome, ScheduleDivergence> {
    decide(
        reference_schedule_admission(inputs),
        circuit_schedule_admission(inputs),
    )
}

#[cfg(test)]
#[path = "schedule_circuit_tests.rs"]
mod schedule_circuit_tests;
