//! QF_BV equivalence authoring (kernel plan §4) — the THIRD witness.
//!
//! Differential testing (reference vs evaluator) and exhaustive small-width
//! enumeration are two witnesses; neither is a proof over the FULL `u64` input space.
//! This module emits an SMT-LIB2 `QF_BV` query that a pinned external solver
//! discharges to turn "differentially tested" into "equivalence-proven":
//!
//! 1. Translate the COMPILED admission circuit (the `NodeOp` graph) to SMT-LIB2 —
//!    so the SOLVER checks the compiler's output, not a re-statement of the spec.
//! 2. Build the imperative reference predicate over the SAME symbolic inputs.
//! 3. Assert `(distinct circuit_admit reference_admit)` and `(check-sat)`: **UNSAT**
//!    means the circuit equals the reference for EVERY input (equivalence proven).
//! 4. NON-VACUITY: separately assert the circuit can admit (SAT) and can refuse
//!    (SAT) — so an UNSAT equivalence result is not vacuously true over a dead
//!    formula. INDEPENDENT-UNSAT: a second pinned solver re-confirms the UNSAT.
//!
//! The solver is an EXTERNAL, PINNED, CLOUD-ONLY test tool — never a runtime
//! dependency and never run on the local potato. The SMT GENERATOR here is pure and
//! is unit-tested locally (structural assertions on the emitted text); the solver
//! harness is gated behind the `qf-bv` cargo feature and runs only in CI.

use super::compile::compile_budget_membrane;
use super::program::{AdmissionProgram, CompareRel, NodeId, NodeOp, Width};
use serde::{Deserialize, Serialize};

/// Whether the compiled admission circuit has passed its full-width equivalence proof
/// and may be treated as authoritative, or is still a candidate behind the
/// authoritative imperative reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProofStatus {
    /// The circuit is wired + load-bearing as a SHADOW, but the imperative reference
    /// is authoritative and the circuit bears NO durable identity (`H_A` unbound). It
    /// may not be promoted to authority until the QF_BV qualification gate passes.
    PromotionCandidate,
    /// The qualification gate passed — genuine `UNSAT` on two independent pinned
    /// solvers, planted-disagreement `SAT`, and non-vacuity — with archived proof.
    ProvenAuthority,
}

/// The admission circuit's source-declared proof status. It STAYS
/// `PromotionCandidate`: promoting the circuit to authority (binding `H_A`) requires
/// the QF_BV qualification gate, and a release that treats the circuit as authority is
/// blocked until a valid [`ProofReceipt`] covering the current circuit exists. The
/// dual path stays fail-closed meanwhile — the imperative reference decides.
pub const ADMISSION_CIRCUIT_PROOF: ProofStatus = ProofStatus::PromotionCandidate;

/// Archived evidence that the QF_BV qualification gate ran and PASSED for a specific
/// circuit (identified by the canonical SMT input digest). Retained as gate evidence;
/// a release that treats the circuit as authority must carry a receipt whose
/// `smt_digest` matches the current circuit and whose verdicts are all correct.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofReceipt {
    /// Hex blake3 digest of the canonical genuine-equivalence SMT-LIB2 input.
    pub smt_digest: String,
    /// Primary pinned solver identity (e.g. `"z3 4.13.4"`).
    pub primary_solver: String,
    /// Independent pinned solver identity (e.g. `"cvc5 1.2.0"`).
    pub independent_solver: String,
    /// Primary solver's genuine verdicts: must be `[unsat, sat, sat]` (equivalence,
    /// admit-reachable, refusal-reachable).
    pub primary_verdicts: Vec<String>,
    /// Independent solver's genuine equivalence verdict: must be `unsat`.
    pub independent_verdict: String,
    /// Planted-disagreement verdict: must be `sat` (the method detects a real bug).
    pub planted_verdict: String,
}

/// Why a proof receipt does NOT qualify the circuit as authority.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProofGateError {
    /// The receipt covers a different circuit than the one being released.
    DigestMismatch {
        /// The current circuit's digest.
        expected: String,
        /// The receipt's digest.
        found: String,
    },
    /// The primary solver did not return `[unsat, sat, sat]`.
    EquivalenceNotProven {
        /// The primary verdicts seen.
        verdicts: Vec<String>,
    },
    /// The independent solver did not return `unsat`.
    IndependentNotConfirmed {
        /// The independent verdict seen.
        verdict: String,
    },
    /// The planted disagreement was not detected (`sat` expected).
    PlantedNotDetected {
        /// The planted verdict seen.
        verdict: String,
    },
}

/// The canonical digest of an SMT input — the circuit's proof identity. A receipt must
/// carry this exact digest (over the genuine equivalence script) to qualify the
/// circuit.
#[must_use]
pub fn smt_digest(smt: &str) -> String {
    batpak::event::hash::compute_hash(smt.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Verify a receipt qualifies the circuit identified by `expected_digest` as
/// authority: matching digest, primary `[unsat, sat, sat]`, independent `unsat`,
/// planted `sat`. Any failure leaves the circuit a [`ProofStatus::PromotionCandidate`].
///
/// # Errors
/// [`ProofGateError`] on digest mismatch or any missing/incorrect verdict.
pub fn verify_receipt(receipt: &ProofReceipt, expected_digest: &str) -> Result<(), ProofGateError> {
    if receipt.smt_digest != expected_digest {
        return Err(ProofGateError::DigestMismatch {
            expected: expected_digest.to_string(),
            found: receipt.smt_digest.clone(),
        });
    }
    let expected_primary = ["unsat", "sat", "sat"];
    if receipt.primary_verdicts.len() != expected_primary.len()
        || receipt
            .primary_verdicts
            .iter()
            .zip(expected_primary)
            .any(|(seen, want)| seen != want)
    {
        return Err(ProofGateError::EquivalenceNotProven {
            verdicts: receipt.primary_verdicts.clone(),
        });
    }
    if receipt.independent_verdict != "unsat" {
        return Err(ProofGateError::IndependentNotConfirmed {
            verdict: receipt.independent_verdict.clone(),
        });
    }
    if receipt.planted_verdict != "sat" {
        return Err(ProofGateError::PlantedNotDetected {
            verdict: receipt.planted_verdict.clone(),
        });
    }
    Ok(())
}

/// Why an admission circuit could not be translated to SMT-LIB2.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum QfBvError {
    /// A node referenced an operand index that does not exist (a malformed,
    /// unvalidated program — `translate` expects validated circuits).
    MissingOperand {
        /// The node whose operand is missing.
        node: u32,
        /// The operand position that was absent.
        operand: usize,
    },
    /// A constant lane wider than 128 bits — beyond what the generator encodes
    /// (admission constants are small: codes, packed selectors, `0`/`1`).
    ConstantTooWide {
        /// The byte length that exceeded the 16-byte ceiling.
        bytes: usize,
    },
    /// An op with no `QF_BV` translation yet (e.g. `BoundedLookup`, used by the
    /// primitive-lowering circuit, not by admission). Extend when a circuit needs it.
    UnsupportedOp {
        /// The op name.
        op: &'static str,
    },
    /// The compiler failed to emit the membrane circuit being verified.
    Compile,
}

impl std::fmt::Display for QfBvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingOperand { node, operand } => {
                write!(f, "node n{node} is missing operand {operand}")
            }
            Self::ConstantTooWide { bytes } => {
                write!(
                    f,
                    "constant of {bytes} bytes exceeds the 16-byte QF_BV ceiling"
                )
            }
            Self::UnsupportedOp { op } => write!(f, "op {op} has no QF_BV translation"),
            Self::Compile => write!(f, "membrane circuit failed to compile"),
        }
    }
}

impl std::error::Error for QfBvError {}

/// A circuit translated to SMT-LIB2: the input declarations + node definitions, and
/// the SMT name of the `admit` output (a 1-bit lane).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslatedCircuit {
    /// The `(declare-fun ...)` + `(define-fun ...)` block, one node per line.
    pub body: String,
    /// The SMT identifier of the `admit` output node (e.g. `n42`).
    pub admit: String,
}

/// The little-endian value of a constant lane (≤ 16 bytes → `u128`).
fn constant_value(bytes: &[u8]) -> Result<u128, QfBvError> {
    if bytes.len() > 16 {
        return Err(QfBvError::ConstantTooWide { bytes: bytes.len() });
    }
    let mut value = 0u128;
    for (index, byte) in bytes.iter().enumerate() {
        value |= u128::from(*byte) << (8 * index);
    }
    Ok(value)
}

/// The SMT name of node `id`.
fn node_name(id: NodeId) -> String {
    format!("n{}", id.0)
}

/// The output width (bits) of a node referenced as an operand.
fn operand_width(program: &AdmissionProgram, id: NodeId) -> Result<u16, QfBvError> {
    let index = usize::try_from(id.0).map_err(|_| QfBvError::MissingOperand {
        node: id.0,
        operand: 0,
    })?;
    program
        .nodes()
        .get(index)
        .map(|node| node.width.get())
        .ok_or(QfBvError::MissingOperand {
            node: id.0,
            operand: 0,
        })
}

/// The SMT expression for one node, referencing earlier nodes by name. Nodes are in
/// canonical topological order, so every operand is already defined.
fn node_expr(
    program: &AdmissionProgram,
    node_id: u32,
    op: &NodeOp,
    operands: &[NodeId],
    width: Width,
) -> Result<String, QfBvError> {
    let operand = |position: usize| -> Result<String, QfBvError> {
        operands
            .get(position)
            .map(|id| node_name(*id))
            .ok_or(QfBvError::MissingOperand {
                node: node_id,
                operand: position,
            })
    };
    match op {
        NodeOp::Input { slot } => Ok(format!("in{}", slot.0)),
        NodeOp::Constant { bytes } => {
            Ok(format!("(_ bv{} {})", constant_value(bytes)?, width.get()))
        }
        NodeOp::Eq => Ok(format!(
            "(ite (= {} {}) (_ bv1 1) (_ bv0 1))",
            operand(0)?,
            operand(1)?
        )),
        NodeOp::Compare { rel } => {
            let relation = match rel {
                CompareRel::Ule => "bvule",
                CompareRel::Ult => "bvult",
            };
            Ok(format!(
                "(ite ({} {} {}) (_ bv1 1) (_ bv0 1))",
                relation,
                operand(0)?,
                operand(1)?
            ))
        }
        NodeOp::BitsetSubset => {
            let bits = operands
                .first()
                .map(|id| operand_width(program, *id))
                .ok_or(QfBvError::MissingOperand {
                    node: node_id,
                    operand: 0,
                })??;
            // a ⊆ b ⟺ a & ~b == 0.
            Ok(format!(
                "(ite (= (bvand {} (bvnot {})) (_ bv0 {})) (_ bv1 1) (_ bv0 1))",
                operand(0)?,
                operand(1)?,
                bits
            ))
        }
        NodeOp::BitsetIntersection => Ok(format!("(bvand {} {})", operand(0)?, operand(1)?)),
        NodeOp::And => Ok(format!("(bvand {} {})", operand(0)?, operand(1)?)),
        NodeOp::Or => Ok(format!("(bvor {} {})", operand(0)?, operand(1)?)),
        NodeOp::Not => Ok(format!("(bvnot {})", operand(0)?)),
        NodeOp::Select => Ok(format!(
            "(ite (= {} (_ bv1 1)) {} {})",
            operand(0)?,
            operand(1)?,
            operand(2)?
        )),
        NodeOp::BoundedLookup { .. } => Err(QfBvError::UnsupportedOp {
            op: "BoundedLookup",
        }),
    }
}

/// Translate a validated [`AdmissionProgram`] into SMT-LIB2 declarations + node
/// definitions. Every input lane becomes a free `BitVec`; every node becomes a
/// `define-fun` over earlier nodes; the `admit` output is named for assertion.
///
/// # Errors
/// [`QfBvError`] if the program is malformed (missing operand) or uses an op with no
/// translation yet.
pub fn translate(program: &AdmissionProgram) -> Result<TranslatedCircuit, QfBvError> {
    let mut body = String::new();
    for (slot, decl) in program.inputs().iter().enumerate() {
        body.push_str(&format!(
            "(declare-fun in{slot} () (_ BitVec {}))\n",
            decl.width.get()
        ));
    }
    for (index, node) in program.nodes().iter().enumerate() {
        let node_id = u32::try_from(index).unwrap_or(u32::MAX);
        let expr = node_expr(program, node_id, &node.op, &node.operands, node.width)?;
        body.push_str(&format!(
            "(define-fun n{node_id} () (_ BitVec {}) {expr})\n",
            node.width.get()
        ));
    }
    Ok(TranslatedCircuit {
        body,
        admit: node_name(program.outputs().admit),
    })
}

/// The slot index of dimension `d`'s field, given the canonical
/// `compile_budget_membrane` input layout (seven groups, each `dims` wide).
fn slot(group: usize, dims: usize, d: usize) -> usize {
    group * dims + d
}

/// The imperative reference predicate as a 1-bit SMT expression over the budget
/// membrane's input lanes — the independent twin the circuit is asserted equal to.
///
/// When `planted_bug` is set, dimension 0's capacity check is DELIBERATELY weakened
/// from `≤` to `<` (strict): the circuit (`≤`) and this mutated reference then differ
/// at `limit == available`. The planted-disagreement query asserts those differ and a
/// conforming solver must return SAT — proving the equivalence METHOD is non-vacuous
/// (a genuinely wrong circuit would be caught, so the genuine UNSAT is meaningful).
fn reference_admit_expr(dims: usize, evidence_width: Width, planted_bug: bool) -> String {
    if dims == 0 {
        return "(_ bv1 1)".to_string();
    }
    let ew = evidence_width.get();
    let per_dim: Vec<String> = (0..dims)
        .map(|d| {
            let limit = format!("in{}", slot(0, dims, d));
            let available = format!("in{}", slot(1, dims, d));
            let derived = format!("in{}", slot(2, dims, d));
            let g_req = format!("in{}", slot(3, dims, d));
            let g_avail = format!("in{}", slot(4, dims, d));
            let e_req = format!("in{}", slot(5, dims, d));
            let e_avail = format!("in{}", slot(6, dims, d));
            let capacity = if planted_bug && d == 0 {
                "bvult"
            } else {
                "bvule"
            };
            format!(
                "(and (bvule {derived} {limit}) ({capacity} {limit} {available}) \
                 (bvule {g_req} {g_avail}) (= (bvand {e_req} (bvnot {e_avail})) (_ bv0 {ew})))"
            )
        })
        .collect();
    format!("(ite (and {}) (_ bv1 1) (_ bv0 1))", per_dim.join(" "))
}

/// Emit the full SMT-LIB2 script proving the compiled budget membrane equivalent to
/// the imperative reference over the WHOLE input space, plus non-vacuity. The three
/// `(check-sat)` results a conforming solver must return, in order, are:
/// `unsat` (equivalence), `sat` (admit reachable), `sat` (refusal reachable).
///
/// # Errors
/// [`QfBvError`] if the membrane fails to compile or translate.
pub fn budget_membrane_equivalence_smt(
    dims: usize,
    budget_width: Width,
    evidence_width: Width,
) -> Result<String, QfBvError> {
    let program = compile_budget_membrane(dims, budget_width, evidence_width)
        .map_err(|_| QfBvError::Compile)?;
    let circuit = translate(&program)?;
    let reference = reference_admit_expr(dims, evidence_width, false);

    let mut smt = String::new();
    smt.push_str("(set-logic QF_BV)\n");
    smt.push_str("; --- compiled budget-membrane circuit ---\n");
    smt.push_str(&circuit.body);
    smt.push_str("; --- imperative reference predicate ---\n");
    smt.push_str(&format!(
        "(define-fun ref_admit () (_ BitVec 1) {reference})\n"
    ));
    smt.push_str("; --- equivalence: UNSAT proves circuit == reference for all inputs ---\n");
    smt.push_str("(push 1)\n");
    smt.push_str(&format!(
        "(assert (distinct {} ref_admit))\n",
        circuit.admit
    ));
    smt.push_str("(check-sat)\n");
    smt.push_str("(pop 1)\n");
    smt.push_str("; --- non-vacuity: an admitting model exists (SAT) ---\n");
    smt.push_str("(push 1)\n");
    smt.push_str(&format!("(assert (= {} (_ bv1 1)))\n", circuit.admit));
    smt.push_str("(check-sat)\n");
    smt.push_str("(pop 1)\n");
    smt.push_str("; --- non-vacuity: a refusing model exists (SAT) ---\n");
    smt.push_str("(push 1)\n");
    smt.push_str(&format!("(assert (= {} (_ bv0 1)))\n", circuit.admit));
    smt.push_str("(check-sat)\n");
    smt.push_str("(pop 1)\n");
    Ok(smt)
}

/// Emit the PLANTED-DISAGREEMENT script: the same compiled circuit against a
/// reference with a deliberately weakened dimension-0 capacity check (`<` not `≤`).
/// A conforming solver MUST return `sat` — a model where they differ. This is the
/// negative control: it proves the equivalence query can DETECT a real disagreement,
/// so the genuine `unsat` is not vacuously true over an always-equal encoding.
///
/// # Errors
/// [`QfBvError`] if the membrane fails to compile or translate.
pub fn budget_planted_disagreement_smt(
    dims: usize,
    budget_width: Width,
    evidence_width: Width,
) -> Result<String, QfBvError> {
    let program = compile_budget_membrane(dims, budget_width, evidence_width)
        .map_err(|_| QfBvError::Compile)?;
    let circuit = translate(&program)?;
    let mutated = reference_admit_expr(dims, evidence_width, true);

    let mut smt = String::new();
    smt.push_str("(set-logic QF_BV)\n");
    smt.push_str("; --- compiled budget-membrane circuit ---\n");
    smt.push_str(&circuit.body);
    smt.push_str("; --- PLANTED-BUG reference (dim-0 capacity weakened < not <=) ---\n");
    smt.push_str(&format!(
        "(define-fun bad_admit () (_ BitVec 1) {mutated})\n"
    ));
    smt.push_str("; --- SAT proves the method detects a genuine disagreement ---\n");
    smt.push_str(&format!(
        "(assert (distinct {} bad_admit))\n",
        circuit.admit
    ));
    smt.push_str("(check-sat)\n");
    Ok(smt)
}

#[cfg(test)]
mod generator_tests {
    use super::super::program::Width;
    use super::{
        budget_membrane_equivalence_smt, budget_planted_disagreement_smt, smt_digest, translate,
        verify_receipt, ProofGateError, ProofReceipt, QfBvError,
    };
    use crate::contract::admission::compile_budget_membrane;

    fn w(bits: u16) -> Width {
        Width::new(bits).expect("valid width")
    }

    /// A receipt with all-correct verdicts for `digest` — the qualifying shape.
    fn passing_receipt(digest: &str) -> ProofReceipt {
        ProofReceipt {
            smt_digest: digest.to_string(),
            primary_solver: "z3 4.13.4".to_string(),
            independent_solver: "cvc5 1.2.0".to_string(),
            primary_verdicts: vec!["unsat".to_string(), "sat".to_string(), "sat".to_string()],
            independent_verdict: "unsat".to_string(),
            planted_verdict: "sat".to_string(),
        }
    }

    #[test]
    fn planted_disagreement_is_a_single_sat_query_over_a_weakened_reference() {
        let smt = budget_planted_disagreement_smt(1, w(8), w(8)).expect("emit");
        assert!(
            smt.contains("bad_admit"),
            "the mutated reference is defined"
        );
        assert!(
            smt.contains("bvult"),
            "dim-0 capacity is weakened to strict <"
        );
        assert_eq!(smt.matches("(check-sat)").count(), 1);
        assert_eq!(smt.matches("(distinct").count(), 1);
    }

    #[test]
    fn a_valid_receipt_qualifies_the_matching_circuit() {
        let smt = budget_membrane_equivalence_smt(3, w(64), w(16)).expect("emit");
        let digest = smt_digest(&smt);
        assert!(verify_receipt(&passing_receipt(&digest), &digest).is_ok());
    }

    #[test]
    fn a_receipt_for_a_different_circuit_is_rejected() {
        let receipt = passing_receipt("deadbeef");
        assert!(matches!(
            verify_receipt(&receipt, "cafef00d"),
            Err(ProofGateError::DigestMismatch { .. })
        ));
    }

    #[test]
    fn a_circuit_whose_planted_bug_went_undetected_does_not_qualify() {
        // If the planted disagreement did NOT return sat, the proof METHOD failed to
        // catch a known bug — so even a genuine unsat must NOT qualify the circuit.
        let digest = "abc123";
        let mut receipt = passing_receipt(digest);
        receipt.planted_verdict = "unsat".to_string();
        assert!(matches!(
            verify_receipt(&receipt, digest),
            Err(ProofGateError::PlantedNotDetected { .. })
        ));
    }

    #[test]
    fn a_circuit_without_independent_confirmation_does_not_qualify() {
        let digest = "abc123";
        let mut receipt = passing_receipt(digest);
        receipt.independent_verdict = "unknown".to_string();
        assert!(matches!(
            verify_receipt(&receipt, digest),
            Err(ProofGateError::IndependentNotConfirmed { .. })
        ));
    }

    #[test]
    fn translation_declares_one_input_per_lane() {
        // 1 dim => 7 budget lanes; each lane is a free BitVec.
        let program = compile_budget_membrane(1, w(8), w(8)).expect("compile");
        let translated = translate(&program).expect("translate");
        let declared = translated.body.matches("(declare-fun in").count();
        assert_eq!(declared, program.inputs().len());
        assert_eq!(declared, 7);
        assert!(translated.admit.starts_with('n'));
    }

    #[test]
    fn equivalence_script_is_well_formed_qf_bv() {
        let smt = budget_membrane_equivalence_smt(2, w(8), w(8)).expect("emit");
        assert!(smt.starts_with("(set-logic QF_BV)"));
        // The compiled circuit's two membrane checks per dimension show up.
        assert!(smt.contains("bvule"), "capacity/guarantee comparisons");
        assert!(smt.contains("bvand"), "evidence subset via and/not");
        assert!(smt.contains("ref_admit"), "the reference twin is defined");
        // Exactly three queries: equivalence + two non-vacuity.
        assert_eq!(smt.matches("(check-sat)").count(), 3);
        assert_eq!(smt.matches("(distinct").count(), 1);
        // Balanced push/pop scoping.
        assert_eq!(
            smt.matches("(push 1)").count(),
            smt.matches("(pop 1)").count()
        );
    }

    #[test]
    fn zero_dimension_membrane_translates_to_a_vacuous_admit() {
        let smt = budget_membrane_equivalence_smt(0, w(8), w(8)).expect("emit");
        assert!(smt.contains("(define-fun ref_admit () (_ BitVec 1) (_ bv1 1))"));
    }

    #[test]
    fn bounded_lookup_is_a_typed_translation_error() {
        // The admission membranes never use BoundedLookup; assert the generator
        // refuses it rather than emitting a silently-wrong translation.
        use super::super::program::{
            AdmissionProgram, InputDecl, LookupTable, Node, NodeId, NodeOp, Outputs,
        };
        let table = LookupTable {
            key_width: w(2),
            entries: vec![vec![0u8], vec![1u8]],
        };
        let nodes = vec![
            Node {
                op: NodeOp::Input {
                    slot: super::super::program::InputSlot(0),
                },
                operands: vec![],
                width: w(2),
            },
            Node {
                op: NodeOp::BoundedLookup { table },
                operands: vec![NodeId(0)],
                width: w(8),
            },
        ];
        let program = AdmissionProgram::new(
            vec![InputDecl { width: w(2) }],
            nodes,
            Outputs {
                admit: NodeId(0),
                refusal_code: NodeId(1),
                membranes: vec![NodeId(0)],
            },
        )
        .expect("well-formed");
        assert_eq!(
            translate(&program),
            Err(QfBvError::UnsupportedOp {
                op: "BoundedLookup"
            })
        );
    }
}

/// The pinned-solver QUALIFICATION harness — CLOUD-ONLY, behind the `qf-bv` feature so
/// it never runs on the local potato. It discharges the full gate: genuine UNSAT on
/// TWO independent pinned solvers (z3 + cvc5, both HARD-required), the planted
/// disagreement returning SAT (the method is non-vacuous), and admit/refuse
/// reachability. It archives a [`ProofReceipt`] and asserts [`verify_receipt`]
/// qualifies THIS circuit. CI: `BVISOR_Z3` + `BVISOR_CVC5` pin the versions,
/// `BVISOR_PROOF_RECEIPT` names the archive path. Absent any pin, it fails CLOSED.
#[cfg(all(test, feature = "qf-bv"))]
mod solver_harness {
    use super::super::program::Width;
    use super::{
        budget_membrane_equivalence_smt, budget_planted_disagreement_smt, smt_digest,
        verify_receipt, ProofReceipt,
    };
    use std::process::Command;

    fn w(bits: u16) -> Width {
        Width::new(bits).expect("valid width")
    }

    /// Run `solver` over `smt` on stdin and return its `(check-sat)` verdicts in order.
    fn run_solver(solver: &str, smt: &str) -> Vec<String> {
        use std::io::Write;
        let mut child = Command::new(solver)
            .arg("-in")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("the pinned solver must be present in CI (BVISOR_Z3 / BVISOR_CVC5)");
        child
            .stdin
            .take()
            .expect("solver stdin")
            .write_all(smt.as_bytes())
            .expect("feed smt");
        let output = child.wait_with_output().expect("solver runs");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| line == "sat" || line == "unsat" || line == "unknown")
            .collect()
    }

    /// The solver's self-reported version line (pinned identity for the receipt).
    fn solver_version(solver: &str) -> String {
        let output = Command::new(solver)
            .arg("--version")
            .output()
            .expect("solver --version");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .unwrap_or("unknown")
            .trim()
            .to_string()
    }

    #[test]
    #[ignore = "QF_BV qualification gate — CLOUD-ONLY; CI runs it with pinned z3+cvc5 via \
                `cargo test -p bvisor --features qf-bv -- --ignored`"]
    fn budget_membrane_qualification_gate() {
        let z3 = std::env::var("BVISOR_Z3").unwrap_or_else(|_| "z3".to_string());
        // cvc5 is HARD-required — independent confirmation is not optional.
        let cvc5 = std::env::var("BVISOR_CVC5")
            .expect("BVISOR_CVC5 must pin an INDEPENDENT solver — confirmation is hard-required");

        // Full-width lanes: 64-bit budget values, 16-bit evidence bitsets, 3 dims.
        let genuine = budget_membrane_equivalence_smt(3, w(64), w(16)).expect("emit genuine");
        let planted = budget_planted_disagreement_smt(3, w(64), w(16)).expect("emit planted");

        let primary = run_solver(&z3, &genuine);
        let independent = run_solver(&cvc5, &genuine);
        let planted_verdict = run_solver(&z3, &planted);

        let receipt = ProofReceipt {
            smt_digest: smt_digest(&genuine),
            primary_solver: solver_version(&z3),
            independent_solver: solver_version(&cvc5),
            primary_verdicts: primary,
            independent_verdict: independent.first().cloned().unwrap_or_default(),
            planted_verdict: planted_verdict.first().cloned().unwrap_or_default(),
        };

        // Archive the evidence (canonical hex) for the release gate to re-verify.
        if let Ok(path) = std::env::var("BVISOR_PROOF_RECEIPT") {
            let bytes = batpak::canonical::to_bytes(&receipt).expect("encode receipt");
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            std::fs::write(&path, hex).expect("write proof receipt");
        }

        // The gate: the receipt must qualify THIS circuit — genuine UNSAT (z3),
        // independent UNSAT (cvc5), planted SAT, admit + refuse reachable.
        verify_receipt(&receipt, &smt_digest(&genuine))
            .expect("QF_BV qualification: two-solver UNSAT + planted SAT + non-vacuity must hold");
    }
}
