//! The admission compiler `C` (kernel plan §1–§2): build [`AdmissionProgram`]s.
//!
//! `C : (Spec, Profile) -> AdmissionProgram` is **total, deterministic, bounded,
//! and canonically emitting** — but it is NOT the NC¹ object. NC¹ is a property of
//! the *emitted* circuit `A` and its [`super::evaluate`]or `E`; the balanced-tree
//! construction `C` performs here is an ordinary `O(n)` build.
//!
//! [`CircuitBuilder`] is the reusable primitive every membrane compiler shares: it
//! appends nodes in canonical (topological) order so the result always satisfies
//! the IR invariant, and it lowers variable-width reductions into **balanced binary
//! trees** ([`CircuitBuilder::and_reduce`]) so depth is `⌈log₂ n⌉`, not `n`.
//!
//! [`compile_budget_membrane`] is the budget membrane: admit iff every dimension
//! passes the two-phase admission `D_d ≤ L_d ∧ L_d ≤ A_d ∧ G_d ≤ E_d ∧ Q_d ⊆ C_d`
//! (intrinsic derived-minimum, then capacity, guarantee, evidence). Its equivalence
//! to the imperative reference is proven exhaustively over the small-width domain
//! (the discrete half of the step-5 equivalence pipeline; the QF_BV solver half
//! lands separately). The per-dimension reason selector is a later step.

use super::program::{
    AdmissionProgram, InputDecl, InputSlot, Node, NodeId, NodeOp, Outputs, ProgramError, Width,
};

/// Builds an [`AdmissionProgram`] by appending nodes in canonical order. Every
/// constructor returns the [`NodeId`] of the node just appended; because nodes are
/// only ever appended, operands always reference strictly-earlier nodes — the
/// canonical-topological invariant holds by construction.
#[derive(Debug, Default)]
pub struct CircuitBuilder {
    inputs: Vec<InputDecl>,
    nodes: Vec<Node>,
}

impl CircuitBuilder {
    /// A fresh, empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn push(&mut self, op: NodeOp, operands: Vec<NodeId>, width: Width) -> NodeId {
        let id = NodeId(u32::try_from(self.nodes.len()).unwrap_or(u32::MAX));
        self.nodes.push(Node {
            op,
            operands,
            width,
        });
        id
    }

    /// Declare a fresh input lane of `width` and return a node that reads it.
    pub fn input(&mut self, width: Width) -> NodeId {
        let slot = u16::try_from(self.inputs.len()).unwrap_or(u16::MAX);
        self.inputs.push(InputDecl { width });
        self.push(
            NodeOp::Input {
                slot: InputSlot(slot),
            },
            vec![],
            width,
        )
    }

    /// A frozen constant lane (little-endian `bytes`, `⌈width/8⌉` long).
    pub fn constant(&mut self, bytes: Vec<u8>, width: Width) -> NodeId {
        self.push(NodeOp::Constant { bytes }, vec![], width)
    }

    /// Unsigned `a ≤ b` over two equal-width lanes → 1-bit predicate.
    pub fn compare_ule(&mut self, a: NodeId, b: NodeId) -> NodeId {
        self.push(
            NodeOp::Compare {
                rel: super::program::CompareRel::Ule,
            },
            vec![a, b],
            Width::one(),
        )
    }

    /// Boolean AND of two 1-bit lanes.
    pub fn and(&mut self, a: NodeId, b: NodeId) -> NodeId {
        self.push(NodeOp::And, vec![a, b], Width::one())
    }

    /// Boolean OR of two 1-bit lanes.
    pub fn or(&mut self, a: NodeId, b: NodeId) -> NodeId {
        self.push(NodeOp::Or, vec![a, b], Width::one())
    }

    /// Boolean NOT of a 1-bit lane.
    pub fn not(&mut self, a: NodeId) -> NodeId {
        self.push(NodeOp::Not, vec![a], Width::one())
    }

    /// `SELECT(cond, a, b)` over width-`width` arms.
    pub fn select(&mut self, cond: NodeId, a: NodeId, b: NodeId, width: Width) -> NodeId {
        self.push(NodeOp::Select, vec![cond, a, b], width)
    }

    /// Bitset subset `a ⊆ b` over two equal-width lanes → 1-bit predicate.
    pub fn bitset_subset(&mut self, a: NodeId, b: NodeId) -> NodeId {
        self.push(NodeOp::BitsetSubset, vec![a, b], Width::one())
    }

    /// Bitwise intersection `a & b` over two equal-width lanes → a width-`width` lane.
    pub fn bitset_intersection(&mut self, a: NodeId, b: NodeId, width: Width) -> NodeId {
        self.push(NodeOp::BitsetIntersection, vec![a, b], width)
    }

    /// Equality of two equal-width lanes → 1-bit predicate.
    pub fn equal(&mut self, a: NodeId, b: NodeId) -> NodeId {
        self.push(NodeOp::Eq, vec![a, b], Width::one())
    }

    /// AND-reduce 1-bit lanes into a **balanced** tree (depth `⌈log₂ n⌉`). An empty
    /// slice reduces to the constant `1` (the identity of AND).
    pub fn and_reduce(&mut self, items: &[NodeId]) -> NodeId {
        match items {
            [] => self.constant(vec![1], Width::one()),
            [only] => *only,
            _ => {
                let mid = items.len() / 2;
                let (left, right) = items.split_at(mid);
                let l = self.and_reduce(left);
                let r = self.and_reduce(right);
                self.and(l, r)
            }
        }
    }

    /// Finish the circuit with its declared outputs, enforcing the IR invariant.
    ///
    /// # Errors
    /// [`ProgramError`] if an output references an out-of-range node (operand
    /// edges are canonical by construction).
    pub fn finish(self, outputs: Outputs) -> Result<AdmissionProgram, ProgramError> {
        AdmissionProgram::new(self.inputs, self.nodes, outputs)
    }
}

/// An 8-bit width, for the small refusal-code lane.
fn refusal_width() -> Width {
    Width::new(8).expect("8 is within 1..=MAX_WIDTH")
}

/// The 2-bit enforcement codes: `Unsupported < Mediated < Enforced`.
const MEDIATED_CODE: u8 = 1;

/// Finish a single-membrane circuit: admit is the membrane bit, refusal code is
/// `0` on admit else `1` (this membrane's index).
fn finish_single_membrane(
    mut builder: CircuitBuilder,
    admit: NodeId,
) -> Result<AdmissionProgram, ProgramError> {
    let rw = refusal_width();
    let admitted = builder.constant(vec![0], rw);
    let refused = builder.constant(vec![1], rw);
    let refusal_code = builder.select(admit, admitted, refused, rw);
    builder.finish(Outputs {
        admit,
        refusal_code,
        membranes: vec![admit],
    })
}

/// Compose ordered membrane bits into `(admit, refusal_code)`: `admit` is their
/// conjunction (a balanced AND tree); `refusal_code` is the 1-based index of the
/// FIRST failing membrane (`0` when all pass) — a bounded priority encoder built
/// right-to-left so an earlier failure overrides a later one.
#[must_use]
pub fn compose_membranes(builder: &mut CircuitBuilder, membranes: &[NodeId]) -> (NodeId, NodeId) {
    let admit = builder.and_reduce(membranes);
    let refusal = priority_encode(builder, membranes);
    (admit, refusal)
}

fn priority_encode(builder: &mut CircuitBuilder, membranes: &[NodeId]) -> NodeId {
    let rw = refusal_width();
    let mut code = builder.constant(vec![0], rw);
    for (i, membrane) in membranes.iter().enumerate().rev() {
        let index = u8::try_from(i + 1).unwrap_or(u8::MAX);
        let fail_code = builder.constant(vec![index], rw);
        // pass -> keep the accumulated (later) code; fail -> this membrane's index.
        code = builder.select(*membrane, code, fail_code, rw);
    }
    code
}

/// The 2-bit enforcement-code width.
fn enforcement_width() -> Width {
    Width::new(2).expect("2 is within 1..=MAX_WIDTH")
}

// --- membrane CHECKS: the decision logic, building into an existing builder and
// returning the membrane's 1-bit pass node. The `compile_*_membrane` wrappers add
// inputs around a check; `compile_admission` shares inputs across all of them.

/// The per-dimension budget lanes the membrane reads, in canonical order.
struct BudgetLanes<'a> {
    /// Requested limits `L_d`.
    limit: &'a [NodeId],
    /// Backend-available limits `A_d`.
    available: &'a [NodeId],
    /// Derived structural minimums `D_d`.
    derived_min: &'a [NodeId],
    /// Required guarantee codes `G_d` (2-bit).
    guarantee_req: &'a [NodeId],
    /// Backend guarantee codes `E_d` (2-bit).
    guarantee_avail: &'a [NodeId],
    /// Required evidence bitsets `Q_d`.
    evidence_req: &'a [NodeId],
    /// Backend evidence bitsets `C_d`.
    evidence_avail: &'a [NodeId],
}

/// One dimension's four check pass bits, in canonical reason order:
/// `[intrinsic (D ≤ L), capacity (L ≤ A), guarantee (G ≤ E), evidence (Q ⊆ C)]`.
/// Shared by the budget membrane (which AND-reduces them) and the budget detail
/// selector (which priority-encodes them into a reason).
fn dim_checks(builder: &mut CircuitBuilder, lanes: &BudgetLanes, d: usize) -> [NodeId; 4] {
    [
        builder.compare_ule(lanes.derived_min[d], lanes.limit[d]),
        builder.compare_ule(lanes.limit[d], lanes.available[d]),
        builder.compare_ule(lanes.guarantee_req[d], lanes.guarantee_avail[d]),
        builder.bitset_subset(lanes.evidence_req[d], lanes.evidence_avail[d]),
    ]
}

/// The two-phase budget admission flattened to one pass bit:
/// `∀d : D_d ≤ L_d ∧ L_d ≤ A_d ∧ G_d ≤ E_d ∧ Q_d ⊆ C_d` (intrinsic derived-minimum,
/// then capacity, guarantee, evidence). The per-dimension reason selector lives in
/// [`compile_budget_detail`]; here every dimension contributes a single pass bit.
fn budget_check(builder: &mut CircuitBuilder, lanes: &BudgetLanes) -> NodeId {
    let per_dim: Vec<NodeId> = (0..lanes.limit.len())
        .map(|d| {
            let checks = dim_checks(builder, lanes, d);
            builder.and_reduce(&checks)
        })
        .collect();
    builder.and_reduce(&per_dim)
}

/// `∀i : required[i] ⊆ available[i]`.
fn evidence_check(
    builder: &mut CircuitBuilder,
    required: &[NodeId],
    available: &[NodeId],
) -> NodeId {
    let checks: Vec<NodeId> = required
        .iter()
        .zip(available)
        .map(|(req, avail)| builder.bitset_subset(*req, *avail))
        .collect();
    builder.and_reduce(&checks)
}

/// `∀i : Mediated ≤ enforcement[i]`.
pub(crate) fn support_check(
    builder: &mut CircuitBuilder,
    enforcements: &[NodeId],
    enf_width: Width,
) -> NodeId {
    let checks: Vec<NodeId> = enforcements
        .iter()
        .map(|enf| {
            let threshold = builder.constant(vec![MEDIATED_CODE], enf_width);
            builder.compare_ule(threshold, *enf)
        })
        .collect();
    builder.and_reduce(&checks)
}

/// `∀i : present[i] ∩ forbidden[i] = ∅`.
fn conflict_check(
    builder: &mut CircuitBuilder,
    present: &[NodeId],
    forbidden: &[NodeId],
    width: Width,
) -> NodeId {
    let zero = builder.constant(vec![0u8; usize::from(width.get()).div_ceil(8)], width);
    let checks: Vec<NodeId> = present
        .iter()
        .zip(forbidden)
        .map(|(p, f)| {
            let intersection = builder.bitset_intersection(*p, *f, width);
            builder.equal(intersection, zero)
        })
        .collect();
    builder.and_reduce(&checks)
}

/// `planned == live`.
fn profile_drift_check(builder: &mut CircuitBuilder, planned: NodeId, live: NodeId) -> NodeId {
    builder.equal(planned, live)
}

/// Compile the budget membrane: admit iff every dimension passes the two-phase
/// check `D_d ≤ L_d ∧ L_d ≤ A_d ∧ G_d ≤ E_d ∧ Q_d ⊆ C_d`. Reads `7·dims` input lanes
/// in canonical order: limit, available, derived-min (each `budget_width`),
/// guarantee-required, guarantee-available (each 2-bit), evidence-required,
/// evidence-available (each `evidence_width`).
///
/// # Errors
/// [`ProgramError`] only on `u32` node-index overflow, which a membrane never hits.
pub fn compile_budget_membrane(
    dims: usize,
    budget_width: Width,
    evidence_width: Width,
) -> Result<AdmissionProgram, ProgramError> {
    let mut builder = CircuitBuilder::new();
    let enf_width = enforcement_width();
    let limit: Vec<NodeId> = (0..dims).map(|_| builder.input(budget_width)).collect();
    let available: Vec<NodeId> = (0..dims).map(|_| builder.input(budget_width)).collect();
    let derived_min: Vec<NodeId> = (0..dims).map(|_| builder.input(budget_width)).collect();
    let guarantee_req: Vec<NodeId> = (0..dims).map(|_| builder.input(enf_width)).collect();
    let guarantee_avail: Vec<NodeId> = (0..dims).map(|_| builder.input(enf_width)).collect();
    let evidence_req: Vec<NodeId> = (0..dims).map(|_| builder.input(evidence_width)).collect();
    let evidence_avail: Vec<NodeId> = (0..dims).map(|_| builder.input(evidence_width)).collect();
    let admit = budget_check(
        &mut builder,
        &BudgetLanes {
            limit: &limit,
            available: &available,
            derived_min: &derived_min,
            guarantee_req: &guarantee_req,
            guarantee_avail: &guarantee_avail,
            evidence_req: &evidence_req,
            evidence_avail: &evidence_avail,
        },
    );
    finish_single_membrane(builder, admit)
}

/// Pack a budget `(dimension, reason)` selector into one lane: `(dim << 3) | reason`,
/// with `dim ∈ 1..=7` and `reason ∈ 1..=4` (canonical order — `1` BelowDerivedMinimum,
/// `2` CapacityExceeded, `3` GuaranteeInsufficient, `4` EvidenceMissing). `0` = no
/// failure. The max packed value `(7<<3)|4 = 60` fits the 8-bit refusal lane.
fn pack_detail(dim: usize, reason: u8) -> u8 {
    (u8::try_from(dim).unwrap_or(0) << 3) | reason
}

/// Compile the budget DETAIL selector circuit: its `refusal_code` is the packed
/// `(first-failing dimension, that dimension's first-failing reason)` — `0` if every
/// dimension passes. Reads the same `7·dims` budget lanes as
/// [`compile_budget_membrane`]. A bounded priority encoder: within a dimension the
/// reasons are ordered (intrinsic highest), and across dimensions the lowest index
/// wins — matching the imperative reference's first-failure semantics exactly.
///
/// # Errors
/// [`ProgramError`] only on `u32` node-index overflow, which a membrane never hits.
pub fn compile_budget_detail(
    dims: usize,
    budget_width: Width,
    evidence_width: Width,
) -> Result<AdmissionProgram, ProgramError> {
    let mut builder = CircuitBuilder::new();
    let enf_width = enforcement_width();
    let limit: Vec<NodeId> = (0..dims).map(|_| builder.input(budget_width)).collect();
    let available: Vec<NodeId> = (0..dims).map(|_| builder.input(budget_width)).collect();
    let derived_min: Vec<NodeId> = (0..dims).map(|_| builder.input(budget_width)).collect();
    let guarantee_req: Vec<NodeId> = (0..dims).map(|_| builder.input(enf_width)).collect();
    let guarantee_avail: Vec<NodeId> = (0..dims).map(|_| builder.input(enf_width)).collect();
    let evidence_req: Vec<NodeId> = (0..dims).map(|_| builder.input(evidence_width)).collect();
    let evidence_avail: Vec<NodeId> = (0..dims).map(|_| builder.input(evidence_width)).collect();
    let lanes = BudgetLanes {
        limit: &limit,
        available: &available,
        derived_min: &derived_min,
        guarantee_req: &guarantee_req,
        guarantee_avail: &guarantee_avail,
        evidence_req: &evidence_req,
        evidence_avail: &evidence_avail,
    };

    let cw = refusal_width();
    let zero = builder.constant(vec![0], cw);
    // Per dimension: priority-encode the four reasons into a packed code (intrinsic
    // outermost = highest priority), and the dimension's overall pass bit.
    let entries: Vec<(NodeId, NodeId)> = (0..dims)
        .map(|d| {
            let checks = dim_checks(&mut builder, &lanes, d);
            let mut code = zero;
            for (offset, &pass) in checks.iter().enumerate().rev() {
                let reason = u8::try_from(offset + 1).unwrap_or(0);
                let packed = builder.constant(vec![pack_detail(d + 1, reason)], cw);
                code = builder.select(pass, code, packed, cw);
            }
            let dim_pass = builder.and_reduce(&checks);
            (dim_pass, code)
        })
        .collect();
    // Across dimensions: the lowest-index failing dimension wins (built right-to-left).
    let mut detail = zero;
    for (dim_pass, code) in entries.iter().rev() {
        detail = builder.select(*dim_pass, detail, *code, cw);
    }
    let admit = builder.equal(detail, zero);
    builder.finish(Outputs {
        admit,
        refusal_code: detail,
        membranes: vec![admit],
    })
}

/// Compile the evidence membrane: admit iff each requirement's required evidence ⊆
/// the backend's available evidence. Reads `2·reqs` bitset lanes of `evidence_width`.
///
/// # Errors
/// [`ProgramError`] only on `u32` node-index overflow, which a membrane never hits.
pub fn compile_evidence_membrane(
    reqs: usize,
    evidence_width: Width,
) -> Result<AdmissionProgram, ProgramError> {
    let mut builder = CircuitBuilder::new();
    let required: Vec<NodeId> = (0..reqs).map(|_| builder.input(evidence_width)).collect();
    let available: Vec<NodeId> = (0..reqs).map(|_| builder.input(evidence_width)).collect();
    let admit = evidence_check(&mut builder, &required, &available);
    finish_single_membrane(builder, admit)
}

/// Compile the support membrane: admit iff every requirement's enforcement is at
/// least `Mediated` (2-bit codes `0` Unsupported, `1` Mediated, `2` Enforced).
///
/// # Errors
/// [`ProgramError`] only on `u32` node-index overflow, which a membrane never hits.
pub fn compile_support_membrane(reqs: usize) -> Result<AdmissionProgram, ProgramError> {
    let mut builder = CircuitBuilder::new();
    let enf_width = enforcement_width();
    let enforcements: Vec<NodeId> = (0..reqs).map(|_| builder.input(enf_width)).collect();
    let admit = support_check(&mut builder, &enforcements, enf_width);
    finish_single_membrane(builder, admit)
}

/// Compile the conflict-freedom membrane: admit iff no present set intersects its
/// forbidden set. Reads `2·reqs` bitset lanes of `width`.
///
/// # Errors
/// [`ProgramError`] only on `u32` node-index overflow, which a membrane never hits.
pub fn compile_conflict_membrane(
    reqs: usize,
    width: Width,
) -> Result<AdmissionProgram, ProgramError> {
    let mut builder = CircuitBuilder::new();
    let present: Vec<NodeId> = (0..reqs).map(|_| builder.input(width)).collect();
    let forbidden: Vec<NodeId> = (0..reqs).map(|_| builder.input(width)).collect();
    let admit = conflict_check(&mut builder, &present, &forbidden, width);
    finish_single_membrane(builder, admit)
}

/// Compile the profile-drift membrane: admit iff the planned profile hash equals the
/// live re-probed one — fails closed if the machine changed.
///
/// # Errors
/// [`ProgramError`] only on `u32` node-index overflow, which a membrane never hits.
pub fn compile_profile_drift_membrane(hash_width: Width) -> Result<AdmissionProgram, ProgramError> {
    let mut builder = CircuitBuilder::new();
    let planned = builder.input(hash_width);
    let live = builder.input(hash_width);
    let same = profile_drift_check(&mut builder, planned, live);
    finish_single_membrane(builder, same)
}

/// The shape of a full admission instance — how many requirements / budget
/// dimensions and the per-aspect lane widths the compiled circuit reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionShape {
    /// Number of requirements (drives support / evidence / conflict lanes).
    pub requirements: usize,
    /// Number of budget dimensions.
    pub budget_dims: usize,
    /// Width of each budget value lane.
    pub budget_width: Width,
    /// Width of each evidence bitset lane.
    pub evidence_width: Width,
    /// Width of each conflict bitset lane.
    pub conflict_width: Width,
    /// Width of the profile-hash lanes.
    pub hash_width: Width,
}

/// Compile the FULL admission circuit: the five membranes composed in the fixed
/// canonical order with the ordered priority-encoder refusal (kernel plan §6).
///
/// Input lane order (so a caller can encode `x`): `planned_hash, live_hash`, then
/// `enforcement × requirements`, then `required × requirements`,
/// `available × requirements`, then the budget section in canonical order —
/// `budget_limit × dims`, `budget_available × dims`, `budget_derived × dims`,
/// `budget_guarantee_req × dims`, `budget_guarantee_avail × dims`,
/// `budget_evidence_req × dims`, `budget_evidence_avail × dims` — then
/// `present × requirements`, `forbidden × requirements`. Membrane order (the refusal
/// index): `1` profile-drift, `2` support, `3` evidence, `4` budget, `5` conflict.
///
/// # Errors
/// [`ProgramError`] only on `u32` node-index overflow, which admission never hits.
pub fn compile_admission(shape: &AdmissionShape) -> Result<AdmissionProgram, ProgramError> {
    let mut builder = CircuitBuilder::new();
    let reqs = shape.requirements;
    let dims = shape.budget_dims;

    let planned = builder.input(shape.hash_width);
    let live = builder.input(shape.hash_width);
    let drift = profile_drift_check(&mut builder, planned, live);

    let enf_width = enforcement_width();
    let enforcements: Vec<NodeId> = (0..reqs).map(|_| builder.input(enf_width)).collect();
    let support = support_check(&mut builder, &enforcements, enf_width);

    let required: Vec<NodeId> = (0..reqs)
        .map(|_| builder.input(shape.evidence_width))
        .collect();
    let available: Vec<NodeId> = (0..reqs)
        .map(|_| builder.input(shape.evidence_width))
        .collect();
    let evidence = evidence_check(&mut builder, &required, &available);

    // Budget section, canonical lane order (must match shadow `encode`): limit,
    // available, derived-min (budget_width); guarantee-req, guarantee-avail
    // (enforcement width); evidence-req, evidence-avail (evidence width) — each × dims.
    let budget_limit: Vec<NodeId> = (0..dims)
        .map(|_| builder.input(shape.budget_width))
        .collect();
    let budget_avail: Vec<NodeId> = (0..dims)
        .map(|_| builder.input(shape.budget_width))
        .collect();
    let budget_derived: Vec<NodeId> = (0..dims)
        .map(|_| builder.input(shape.budget_width))
        .collect();
    let budget_g_req: Vec<NodeId> = (0..dims).map(|_| builder.input(enf_width)).collect();
    let budget_g_avail: Vec<NodeId> = (0..dims).map(|_| builder.input(enf_width)).collect();
    let budget_e_req: Vec<NodeId> = (0..dims)
        .map(|_| builder.input(shape.evidence_width))
        .collect();
    let budget_e_avail: Vec<NodeId> = (0..dims)
        .map(|_| builder.input(shape.evidence_width))
        .collect();
    let budget = budget_check(
        &mut builder,
        &BudgetLanes {
            limit: &budget_limit,
            available: &budget_avail,
            derived_min: &budget_derived,
            guarantee_req: &budget_g_req,
            guarantee_avail: &budget_g_avail,
            evidence_req: &budget_e_req,
            evidence_avail: &budget_e_avail,
        },
    );

    let present: Vec<NodeId> = (0..reqs)
        .map(|_| builder.input(shape.conflict_width))
        .collect();
    let forbidden: Vec<NodeId> = (0..reqs)
        .map(|_| builder.input(shape.conflict_width))
        .collect();
    let conflict = conflict_check(&mut builder, &present, &forbidden, shape.conflict_width);

    let membranes = [drift, support, evidence, budget, conflict];
    let (admit, refusal_code) = compose_membranes(&mut builder, &membranes);
    builder.finish(Outputs {
        admit,
        refusal_code,
        membranes: membranes.to_vec(),
    })
}

#[cfg(test)]
#[path = "compile_tests.rs"]
mod compile_tests;
