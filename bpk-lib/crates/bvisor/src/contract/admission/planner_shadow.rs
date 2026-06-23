//! The planner-matching shadow: a circuit that models the CURRENT
//! `BoundaryPlanner::plan()` admission contract exactly, so the circuit shadows
//! *the real planner*, not a parallel idealization.
//!
//! `plan()` today admits in two ordered membranes:
//! 1. **support** — every requirement's enforcement is `Enforced` or `Mediated`
//!    (it refuses on the first `Unsupported`);
//! 2. **evidence** — the caller's required claims are a subset of the UNION of the
//!    admitted requirements' available evidence (`required ⊆ ⋃ available`).
//!
//! Budget / conflict / profile-drift membranes are NOT part of the current
//! contract (they land with budget widening, the lowering membrane, and
//! execution-time revalidation respectively — the corrected build order); the
//! shadow grows to match as they do.
//!
//! The evidence union is computed ONCE in the imperative normalize pass and fed as
//! a single lane to both paths — the node vocabulary has wide AND
//! ([`super::compile::CircuitBuilder::bitset_intersection`]) but no wide OR, so the
//! circuit checks `required ⊆ available_union` against the pre-unioned input rather
//! than unioning in-circuit. That is exactly "normalize once."

use super::compile::{compose_membranes, support_check, CircuitBuilder};
use super::eval::{evaluate, Lane};
use super::program::{AdmissionProgram, Outputs, ProgramError, Width};
use super::shadow::{
    decide, mask, outcome_from_trace, AdmissionDivergence, AdmissionOutcome, MembraneDetails,
};

const ENFORCEMENT_BITS: u32 = 2;
const EVIDENCE_BITS: u32 = 16;

/// The normalized inputs for the current planner contract — produced by a single
/// probe/classify pass and fed byte-for-byte to both the imperative reference and
/// the shadow circuit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannerInputs {
    /// Per-requirement enforcement codes (`0` Unsupported, `1` Mediated, `2`
    /// Enforced), in the planner's iteration order (capabilities then controls).
    pub enforcement: Vec<u8>,
    /// The caller's required-evidence bitset (one bit per `EvidenceClaim`).
    pub evidence_required: u16,
    /// The UNION of the admitted requirements' available-evidence bitsets,
    /// pre-computed in the normalize pass.
    pub evidence_available: u16,
}

/// The authoritative imperative decision for the current planner contract.
#[must_use]
pub fn planner_reference(inputs: &PlannerInputs) -> AdmissionOutcome {
    let support = inputs
        .enforcement
        .iter()
        .all(|e| mask(u64::from(*e), ENFORCEMENT_BITS) >= 1);
    let evidence = (mask(u64::from(inputs.evidence_required), EVIDENCE_BITS)
        & !mask(u64::from(inputs.evidence_available), EVIDENCE_BITS))
        == 0;
    // The planner has neither a budget nor a schedule membrane (no details).
    outcome_from_trace(
        vec![support, evidence],
        &MembraneDetails {
            budget: 0,
            schedule: 0,
        },
    )
}

fn enforcement_width() -> Width {
    Width::new(2).expect("2 is within 1..=MAX_WIDTH")
}

fn evidence_width() -> Width {
    Width::new(16).expect("16 is within 1..=MAX_WIDTH")
}

/// Compile the planner circuit: support membrane (per-requirement enforcement ≥
/// Mediated) then evidence membrane (`required ⊆ available_union`), composed with
/// the ordered refusal encoder. Input lane order: enforcement×reqs, required,
/// available_union.
fn compile_planner_circuit(reqs: usize) -> Result<AdmissionProgram, ProgramError> {
    let mut builder = CircuitBuilder::new();
    let enf_width = enforcement_width();
    let evid_width = evidence_width();
    let enforcements: Vec<_> = (0..reqs).map(|_| builder.input(enf_width)).collect();
    let required = builder.input(evid_width);
    let available = builder.input(evid_width);

    let support = support_check(&mut builder, &enforcements, enf_width);
    let evidence = builder.bitset_subset(required, available);
    let (admit, refusal_code) = compose_membranes(&mut builder, &[support, evidence]);
    builder.finish(Outputs {
        admit,
        refusal_code,
        membranes: vec![support, evidence],
    })
}

fn encode(inputs: &PlannerInputs) -> Vec<Lane> {
    let enf_width = enforcement_width();
    let evid_width = evidence_width();
    let mut lanes: Vec<Lane> = inputs
        .enforcement
        .iter()
        .map(|e| Lane::from_le_bytes(&u64::from(*e).to_le_bytes(), enf_width))
        .collect();
    lanes.push(Lane::from_le_bytes(
        &u64::from(inputs.evidence_required).to_le_bytes(),
        evid_width,
    ));
    lanes.push(Lane::from_le_bytes(
        &u64::from(inputs.evidence_available).to_le_bytes(),
        evid_width,
    ));
    lanes
}

fn planner_circuit(inputs: &PlannerInputs) -> Result<AdmissionOutcome, &'static str> {
    let program = compile_planner_circuit(inputs.enforcement.len())
        .map_err(|_| "circuit compilation failed")?;
    let decision = evaluate(&program, &encode(inputs)).map_err(|_| "circuit evaluation failed")?;
    Ok(if decision.admit {
        AdmissionOutcome::Admitted {
            trace: decision.membranes,
        }
    } else {
        AdmissionOutcome::Refused {
            membrane: decision.refusal_code,
            refusal_code: decision.refusal_code,
            trace: decision.membranes,
            budget_dimension: 0,
            budget_reason: 0,
            schedule_reason: 0,
        }
    })
}

/// Run both paths over the same normalized planner inputs and return the
/// authoritative outcome, or a typed [`AdmissionDivergence`] on disagreement.
///
/// # Errors
/// [`AdmissionDivergence`] if the shadow circuit disagrees with the reference, or
/// fails to compile/evaluate.
pub fn planner_shadow_check(
    inputs: &PlannerInputs,
) -> Result<AdmissionOutcome, AdmissionDivergence> {
    decide(planner_reference(inputs), planner_circuit(inputs))
}

#[cfg(test)]
mod planner_shadow_tests {
    use super::super::shadow::{decide, AdmissionDivergence, AdmissionOutcome};
    use super::{planner_reference, planner_shadow_check, PlannerInputs};

    fn admitted() -> AdmissionOutcome {
        AdmissionOutcome::Admitted {
            trace: vec![true, true],
        }
    }

    fn refused_at(membrane: u64, trace: Vec<bool>) -> AdmissionOutcome {
        AdmissionOutcome::Refused {
            membrane,
            refusal_code: membrane,
            trace,
            budget_dimension: 0,
            budget_reason: 0,
            schedule_reason: 0,
        }
    }

    #[test]
    fn planner_circuit_matches_the_reference_exhaustively() {
        // Two requirements (2-bit enforcement) × required/available evidence over a
        // 4-bit subdomain => 4·4·16·16 = 4096 cases. Both paths must agree on every
        // one (no divergence) AND produce the planner-correct outcome.
        for e0 in 0..4u8 {
            for e1 in 0..4u8 {
                for required in 0..16u16 {
                    for available in 0..16u16 {
                        let inputs = PlannerInputs {
                            enforcement: vec![e0, e1],
                            evidence_required: required,
                            evidence_available: available,
                        };
                        let outcome = planner_shadow_check(&inputs).expect("no divergence");

                        let support = e0 >= 1 && e1 >= 1;
                        let evidence = (required & !available) == 0;
                        let expected = match (support, evidence) {
                            (true, true) => admitted(),
                            (false, _) => refused_at(1, vec![false, evidence]),
                            (true, false) => refused_at(2, vec![true, false]),
                        };
                        assert_eq!(
                            outcome, expected,
                            "enf=[{e0},{e1}] req={required:04b} avail={available:04b}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn reference_orders_support_before_evidence() {
        // A requirement is unsupported AND evidence is unsatisfiable: support (the
        // first membrane) must win the refusal.
        let inputs = PlannerInputs {
            enforcement: vec![0],
            evidence_required: 0b1,
            evidence_available: 0b0,
        };
        assert_eq!(
            planner_reference(&inputs),
            refused_at(1, vec![false, false])
        );
    }

    #[test]
    fn divergence_detector_fires_on_a_planted_planner_mismatch() {
        let reference = planner_reference(&PlannerInputs {
            enforcement: vec![2],
            evidence_required: 0,
            evidence_available: 0,
        });
        let wrong = AdmissionOutcome::Refused {
            membrane: 2,
            refusal_code: 2,
            trace: vec![true, false],
            budget_dimension: 0,
            budget_reason: 0,
            schedule_reason: 0,
        };
        assert_eq!(
            decide(reference.clone(), Ok(wrong.clone())),
            Err(AdmissionDivergence::OutcomeMismatch {
                reference: Box::new(reference),
                circuit: Box::new(wrong),
            })
        );
    }
}
