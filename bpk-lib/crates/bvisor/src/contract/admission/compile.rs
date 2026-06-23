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
//! [`compile_budget_membrane`] is the first real membrane: admit iff every
//! requested budget dimension is within the available budget
//! (`∀d : req[d] ≤ avail[d]`). Its equivalence to the imperative reference is
//! proven exhaustively over the small-width domain (the discrete half of the
//! step-5 equivalence pipeline; the QF_BV solver half lands separately).

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

/// Compile the budget membrane: admit iff `∀d : req[d] ≤ avail[d]`.
///
/// The circuit reads `2·dims` input lanes of `width` — the `dims` requested values
/// followed by the `dims` available values — compares each pair with unsigned `≤`,
/// and balances the per-dimension passes into a single admit bit. The refusal code
/// is `0` when admitted, else `1` (this membrane's index).
///
/// # Errors
/// [`ProgramError`] only if the circuit somehow exceeds `u32` node indexing, which
/// a budget membrane never does.
pub fn compile_budget_membrane(
    dims: usize,
    width: Width,
) -> Result<AdmissionProgram, ProgramError> {
    let mut builder = CircuitBuilder::new();
    let requested: Vec<NodeId> = (0..dims).map(|_| builder.input(width)).collect();
    let available: Vec<NodeId> = (0..dims).map(|_| builder.input(width)).collect();
    let checks: Vec<NodeId> = requested
        .iter()
        .zip(&available)
        .map(|(req, avail)| builder.compare_ule(*req, *avail))
        .collect();
    let admit = builder.and_reduce(&checks);
    finish_single_membrane(builder, admit)
}

/// Compile the evidence membrane: admit iff each requirement's required evidence
/// claims are a subset of those the backend can produce (`required ⊆ available`).
///
/// Reads `2·reqs` bitset lanes of `evidence_width` — the `reqs` required sets, then
/// the `reqs` available sets.
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
    let checks: Vec<NodeId> = required
        .iter()
        .zip(&available)
        .map(|(req, avail)| builder.bitset_subset(*req, *avail))
        .collect();
    let admit = builder.and_reduce(&checks);
    finish_single_membrane(builder, admit)
}

/// Compile the support membrane: admit iff every requirement's enforcement is at
/// least `Mediated`. Enforcement is a 2-bit code (`0` Unsupported, `1` Mediated,
/// `2` Enforced); the membrane checks `Mediated ≤ enforcement[i]` per requirement.
///
/// # Errors
/// [`ProgramError`] only on `u32` node-index overflow, which a membrane never hits.
pub fn compile_support_membrane(reqs: usize) -> Result<AdmissionProgram, ProgramError> {
    let mut builder = CircuitBuilder::new();
    let enf_width = Width::new(2).expect("2 is within 1..=MAX_WIDTH");
    let enforcements: Vec<NodeId> = (0..reqs).map(|_| builder.input(enf_width)).collect();
    let checks: Vec<NodeId> = enforcements
        .iter()
        .map(|enf| {
            let threshold = builder.constant(vec![MEDIATED_CODE], enf_width);
            builder.compare_ule(threshold, *enf)
        })
        .collect();
    let admit = builder.and_reduce(&checks);
    finish_single_membrane(builder, admit)
}

#[cfg(test)]
mod compile_tests {
    use super::super::eval::{evaluate, Lane};
    use super::super::limits::FROZEN_LIMITS;
    use super::super::program::{Outputs, Width};
    use super::super::validate::validate;
    use super::{
        compile_budget_membrane, compile_evidence_membrane, compile_support_membrane,
        compose_membranes, CircuitBuilder,
    };

    fn w(bits: u16) -> Width {
        Width::new(bits).expect("valid width")
    }

    /// Encode an unsigned value into a `width`-bit lane (low `width` bits, LSB-first).
    fn lane(value: u64, width: Width) -> Lane {
        Lane::from_le_bytes(&value.to_le_bytes(), width)
    }

    /// The imperative reference decision the circuit must match.
    fn budget_reference(req: &[u64], avail: &[u64]) -> bool {
        req.iter().zip(avail).all(|(r, a)| r <= a)
    }

    #[test]
    fn compiler_emits_a_program_the_validator_accepts() {
        let program = compile_budget_membrane(7, w(64)).expect("compile");
        validate(&program, &FROZEN_LIMITS).expect("C emits valid programs");
    }

    #[test]
    fn balanced_and_reduce_is_logarithmic_depth() {
        // 8 inputs AND-reduced → a balanced tree of depth 3 (not 7).
        let mut b = CircuitBuilder::new();
        let bits: Vec<_> = (0..8).map(|_| b.input(Width::one())).collect();
        let root = b.and_reduce(&bits);
        let program = b
            .finish(super::super::program::Outputs {
                admit: root,
                refusal_code: root,
                membranes: vec![root],
            })
            .expect("well-formed");
        // 8 boolean inputs feed a depth-3 AND tree: bit-depth = 3 (each AND = 1).
        assert_eq!(program.bit_depth(), 3);
    }

    #[test]
    fn budget_membrane_equivalent_to_reference_exhaustively() {
        // The discrete half of step-5 equivalence: exhaustive over the full domain
        // of a small instance (2 dims, 3-bit lanes => 8^4 = 4096 inputs).
        let width = w(3);
        let program = compile_budget_membrane(2, width).expect("compile");
        validate(&program, &FROZEN_LIMITS).expect("valid");
        for r0 in 0..8u64 {
            for r1 in 0..8u64 {
                for a0 in 0..8u64 {
                    for a1 in 0..8u64 {
                        let inputs = [
                            lane(r0, width),
                            lane(r1, width),
                            lane(a0, width),
                            lane(a1, width),
                        ];
                        let decision = evaluate(&program, &inputs).expect("eval");
                        let reference = budget_reference(&[r0, r1], &[a0, a1]);
                        assert_eq!(
                            decision.admit, reference,
                            "mismatch at req=[{r0},{r1}] avail=[{a0},{a1}]"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn refusal_code_is_zero_on_admit_and_one_on_refuse() {
        let width = w(64);
        let program = compile_budget_membrane(3, width).expect("compile");
        let within = [
            lane(1, width),
            lane(2, width),
            lane(3, width),
            lane(10, width),
            lane(10, width),
            lane(10, width),
        ];
        let admitted = evaluate(&program, &within).expect("eval");
        assert!(admitted.admit);
        assert_eq!(admitted.refusal_code, 0);

        // Dimension 1 requests more than available.
        let over = [
            lane(1, width),
            lane(99, width),
            lane(3, width),
            lane(10, width),
            lane(10, width),
            lane(10, width),
        ];
        let refused = evaluate(&program, &over).expect("eval");
        assert!(!refused.admit);
        assert_eq!(refused.refusal_code, 1);
    }

    #[test]
    fn zero_dimension_membrane_admits_vacuously() {
        let program = compile_budget_membrane(0, w(8)).expect("compile");
        let decision = evaluate(&program, &[]).expect("eval");
        assert!(decision.admit, "an empty conjunction admits");
    }

    #[test]
    fn evidence_membrane_equivalent_to_reference_exhaustively() {
        // admit iff required ⊆ available, per requirement. 2 reqs, 3-bit sets => 8^4.
        let width = w(3);
        let program = compile_evidence_membrane(2, width).expect("compile");
        validate(&program, &FROZEN_LIMITS).expect("valid");
        for r0 in 0..8u64 {
            for r1 in 0..8u64 {
                for a0 in 0..8u64 {
                    for a1 in 0..8u64 {
                        let inputs = [
                            lane(r0, width),
                            lane(r1, width),
                            lane(a0, width),
                            lane(a1, width),
                        ];
                        let decision = evaluate(&program, &inputs).expect("eval");
                        // required ⊆ available ⟺ (required & !available) == 0.
                        let reference = (r0 & !a0) == 0 && (r1 & !a1) == 0;
                        assert_eq!(
                            decision.admit, reference,
                            "mismatch req=[{r0:03b},{r1:03b}] avail=[{a0:03b},{a1:03b}]"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn support_membrane_equivalent_to_reference_exhaustively() {
        // admit iff every enforcement >= Mediated(1). 3 reqs, 2-bit codes => 4^3.
        let program = compile_support_membrane(3).expect("compile");
        validate(&program, &FROZEN_LIMITS).expect("valid");
        let enf_width = w(2);
        for e0 in 0..4u64 {
            for e1 in 0..4u64 {
                for e2 in 0..4u64 {
                    let inputs = [
                        lane(e0, enf_width),
                        lane(e1, enf_width),
                        lane(e2, enf_width),
                    ];
                    let decision = evaluate(&program, &inputs).expect("eval");
                    let reference = e0 >= 1 && e1 >= 1 && e2 >= 1;
                    assert_eq!(decision.admit, reference, "enf=[{e0},{e1},{e2}]");
                }
            }
        }
    }

    #[test]
    fn priority_encoder_reports_the_first_failing_membrane_exhaustively() {
        // Build a circuit whose membranes ARE three 1-bit inputs, compose them, and
        // verify admit + the first-failing-index refusal code over all 2^3 inputs.
        let mut builder = CircuitBuilder::new();
        let membranes: Vec<_> = (0..3).map(|_| builder.input(Width::one())).collect();
        let (admit, refusal) = compose_membranes(&mut builder, &membranes);
        let program = builder
            .finish(Outputs {
                admit,
                refusal_code: refusal,
                membranes: membranes.clone(),
            })
            .expect("well-formed");
        validate(&program, &FROZEN_LIMITS).expect("valid");

        for bits in 0..8u8 {
            let m = [(bits & 1) == 1, (bits & 2) == 2, (bits & 4) == 4];
            let inputs = [Lane::bit(m[0]), Lane::bit(m[1]), Lane::bit(m[2])];
            let decision = evaluate(&program, &inputs).expect("eval");
            let expected_admit = m[0] && m[1] && m[2];
            let expected_code = m
                .iter()
                .position(|pass| !pass)
                .map_or(0, |i| u64::try_from(i + 1).unwrap_or(0));
            assert_eq!(decision.admit, expected_admit, "admit at {bits:03b}");
            assert_eq!(decision.refusal_code, expected_code, "code at {bits:03b}");
        }
    }
}
