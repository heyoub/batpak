//! Shadow wiring (kernel plan §14 step 5) — the admission circuit as a
//! NON-PERSISTENT oracle beside the authoritative imperative reference.
//!
//! The imperative path is authoritative; the circuit is a shadow. Both receive the
//! **identical, immutable** normalized inputs (`(S,P,V,L,Q,B)`, probed and
//! normalized ONCE — never re-probed per path, or a profile change between calls
//! would manufacture a false disagreement). They are compared on the **full
//! canonical [`AdmissionOutcome`]** — admit/refuse, first failing membrane, stable
//! refusal code, and the per-membrane trace — **not** a bare `is_ok()`, which would
//! miss drift in refusal classification or trace generation.
//!
//! Any mismatch is a typed [`AdmissionDivergence`] hard finding: it fails the
//! gauntlet, never silently logged-and-continued. The circuit bears **no durable
//! identity here** — `H_A` is bound only after the circuit is validated, proven
//! equivalent over the final admission surface, and promoted (later steps).
//!
//! This module is the comparison machinery over normalized [`AdmissionInputs`]; the
//! glue that derives those inputs from a real `BoundarySpec` + `BackendProfile`
//! inside `BoundaryPlanner` is the next increment.

use super::compile::{compile_admission, AdmissionShape};
use super::eval::{evaluate, Lane};
use super::program::Width;

/// Lane width (bits) of the evidence / conflict / budget / hash lanes in the shadow
/// encoding. Enforcement is the 2-bit code the circuit uses internally.
const FIELD_BITS: u32 = 8;
const ENFORCEMENT_BITS: u32 = 2;

/// One requirement's admission inputs (the per-requirement slice of `V`/`Q`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequirementInputs {
    /// Enforcement code: `0` Unsupported, `1` Mediated, `2` Enforced.
    pub enforcement: u8,
    /// Required evidence bitset.
    pub evidence_required: u64,
    /// Backend-available evidence bitset.
    pub evidence_available: u64,
    /// Present-primitive bitset (for conflict freedom).
    pub conflict_present: u64,
    /// Forbidden-primitive bitset.
    pub conflict_forbidden: u64,
}

/// One budget dimension's request vs availability (the `B` slice).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetPair {
    /// Requested limit.
    pub requested: u64,
    /// Backend-available limit.
    pub available: u64,
}

/// The normalized, immutable admission inputs — `(S,P,V,L,Q,B)` reduced to the
/// membrane-relevant encoded form. Probed and normalized ONCE; fed to both paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmissionInputs {
    /// The profile hash bound at plan time (`P` digest).
    pub planned_profile: u64,
    /// The live re-probed profile hash.
    pub live_profile: u64,
    /// Per-requirement inputs.
    pub requirements: Vec<RequirementInputs>,
    /// Per-dimension budget inputs.
    pub budget: Vec<BudgetPair>,
}

/// The canonical admission decision both paths must agree on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmissionOutcome {
    /// Admitted; carries the per-membrane pass trace (all `true`).
    Admitted {
        /// Per-membrane pass bits, in canonical order.
        trace: Vec<bool>,
    },
    /// Refused at the first failing membrane.
    Refused {
        /// 1-based index of the first failing membrane.
        membrane: u64,
        /// The stable refusal code.
        refusal_code: u64,
        /// Per-membrane pass bits, in canonical order.
        trace: Vec<bool>,
    },
}

/// A typed shadow disagreement — a hard gauntlet finding, never silently ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AdmissionDivergence {
    /// The two paths produced different outcomes.
    OutcomeMismatch {
        /// The authoritative imperative outcome.
        reference: AdmissionOutcome,
        /// The shadow circuit outcome.
        circuit: AdmissionOutcome,
    },
    /// The shadow circuit failed to compile or evaluate where the reference did not.
    CircuitError {
        /// The authoritative imperative outcome.
        reference: AdmissionOutcome,
        /// Why the circuit path failed.
        reason: &'static str,
    },
}

impl std::fmt::Display for AdmissionDivergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutcomeMismatch { reference, circuit } => write!(
                f,
                "admission divergence: reference {reference:?} != circuit {circuit:?}"
            ),
            Self::CircuitError { reference, reason } => write!(
                f,
                "admission divergence: circuit failed ({reason}) where reference was {reference:?}"
            ),
        }
    }
}

impl std::error::Error for AdmissionDivergence {}

/// Mask a value to its lane's low `bits`, matching what the circuit's lanes see, so
/// the reference and the circuit agree even on out-of-range inputs.
pub(crate) fn mask(value: u64, bits: u32) -> u64 {
    if bits >= 64 {
        value
    } else {
        value & ((1u64 << bits) - 1)
    }
}

/// The five membrane pass bits in canonical order: profile-drift, support,
/// evidence, budget, conflict. The single imperative source of the decision.
fn reference_trace(inputs: &AdmissionInputs) -> Vec<bool> {
    let drift = mask(inputs.planned_profile, FIELD_BITS) == mask(inputs.live_profile, FIELD_BITS);
    let support = inputs
        .requirements
        .iter()
        .all(|r| mask(u64::from(r.enforcement), ENFORCEMENT_BITS) >= 1);
    let evidence = inputs.requirements.iter().all(|r| {
        (mask(r.evidence_required, FIELD_BITS) & !mask(r.evidence_available, FIELD_BITS)) == 0
    });
    let budget = inputs
        .budget
        .iter()
        .all(|d| mask(d.requested, FIELD_BITS) <= mask(d.available, FIELD_BITS));
    let conflict = inputs.requirements.iter().all(|r| {
        (mask(r.conflict_present, FIELD_BITS) & mask(r.conflict_forbidden, FIELD_BITS)) == 0
    });
    vec![drift, support, evidence, budget, conflict]
}

/// Build the outcome from a membrane trace (first failing membrane = refusal).
pub(crate) fn outcome_from_trace(trace: Vec<bool>) -> AdmissionOutcome {
    match trace.iter().position(|pass| !pass) {
        None => AdmissionOutcome::Admitted { trace },
        Some(i) => {
            let index = u64::try_from(i + 1).unwrap_or(0);
            AdmissionOutcome::Refused {
                membrane: index,
                refusal_code: index,
                trace,
            }
        }
    }
}

/// The authoritative imperative admission decision over normalized inputs.
#[must_use]
pub fn reference_admission(inputs: &AdmissionInputs) -> AdmissionOutcome {
    outcome_from_trace(reference_trace(inputs))
}

fn field_width() -> Width {
    Width::new(8).expect("8 is within 1..=MAX_WIDTH")
}

fn enforcement_lane_width() -> Width {
    Width::new(2).expect("2 is within 1..=MAX_WIDTH")
}

fn shape_of(inputs: &AdmissionInputs) -> AdmissionShape {
    AdmissionShape {
        requirements: inputs.requirements.len(),
        budget_dims: inputs.budget.len(),
        budget_width: field_width(),
        evidence_width: field_width(),
        conflict_width: field_width(),
        hash_width: field_width(),
    }
}

fn encode_lane(value: u64, width: Width) -> Lane {
    Lane::from_le_bytes(&value.to_le_bytes(), width)
}

/// Encode the inputs into the lane vector `compile_admission` reads (its documented
/// order): planned, live, enforcement×reqs, required×reqs, available×reqs,
/// budget_req×dims, budget_avail×dims, present×reqs, forbidden×reqs.
fn encode(inputs: &AdmissionInputs) -> Vec<Lane> {
    let field = field_width();
    let enf = enforcement_lane_width();
    let mut lanes = vec![
        encode_lane(inputs.planned_profile, field),
        encode_lane(inputs.live_profile, field),
    ];
    lanes.extend(
        inputs
            .requirements
            .iter()
            .map(|r| encode_lane(u64::from(r.enforcement), enf)),
    );
    lanes.extend(
        inputs
            .requirements
            .iter()
            .map(|r| encode_lane(r.evidence_required, field)),
    );
    lanes.extend(
        inputs
            .requirements
            .iter()
            .map(|r| encode_lane(r.evidence_available, field)),
    );
    lanes.extend(
        inputs
            .budget
            .iter()
            .map(|d| encode_lane(d.requested, field)),
    );
    lanes.extend(
        inputs
            .budget
            .iter()
            .map(|d| encode_lane(d.available, field)),
    );
    lanes.extend(
        inputs
            .requirements
            .iter()
            .map(|r| encode_lane(r.conflict_present, field)),
    );
    lanes.extend(
        inputs
            .requirements
            .iter()
            .map(|r| encode_lane(r.conflict_forbidden, field)),
    );
    lanes
}

/// The shadow circuit's admission decision over the same normalized inputs.
fn circuit_admission(inputs: &AdmissionInputs) -> Result<AdmissionOutcome, &'static str> {
    let shape = shape_of(inputs);
    let program = compile_admission(&shape).map_err(|_| "circuit compilation failed")?;
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
        }
    })
}

/// Compare the authoritative reference against the shadow circuit outcome. The
/// pure comparison core of [`shadow_check`], factored so the divergence detector
/// can be proven to fire on a planted mismatch.
pub(crate) fn decide(
    reference: AdmissionOutcome,
    circuit: Result<AdmissionOutcome, &'static str>,
) -> Result<AdmissionOutcome, AdmissionDivergence> {
    match circuit {
        Ok(circuit) if circuit == reference => Ok(reference),
        Ok(circuit) => Err(AdmissionDivergence::OutcomeMismatch { reference, circuit }),
        Err(reason) => Err(AdmissionDivergence::CircuitError { reference, reason }),
    }
}

/// Run both paths over the identical normalized inputs and return the authoritative
/// outcome — or a typed [`AdmissionDivergence`] if the shadow circuit disagrees.
///
/// # Errors
/// [`AdmissionDivergence`] on any full-outcome mismatch, or if the circuit fails to
/// compile/evaluate where the reference did not.
pub fn shadow_check(inputs: &AdmissionInputs) -> Result<AdmissionOutcome, AdmissionDivergence> {
    decide(reference_admission(inputs), circuit_admission(inputs))
}

#[cfg(test)]
mod shadow_tests {
    use super::{
        decide, reference_admission, shadow_check, AdmissionDivergence, AdmissionInputs,
        AdmissionOutcome, BudgetPair, RequirementInputs,
    };

    /// One requirement, one budget dim, all membranes passing.
    fn all_pass() -> AdmissionInputs {
        AdmissionInputs {
            planned_profile: 7,
            live_profile: 7, // == planned -> drift passes
            requirements: vec![RequirementInputs {
                enforcement: 2, // Enforced
                evidence_required: 0b0101,
                evidence_available: 0b1111, // superset
                conflict_present: 0b0001,
                conflict_forbidden: 0b0010, // disjoint
            }],
            budget: vec![BudgetPair {
                requested: 10,
                available: 20,
            }],
        }
    }

    #[test]
    fn shadow_admits_and_both_paths_agree() {
        let outcome = shadow_check(&all_pass()).expect("no divergence");
        assert_eq!(
            outcome,
            AdmissionOutcome::Admitted {
                trace: vec![true, true, true, true, true],
            }
        );
    }

    #[test]
    fn shadow_refuses_at_first_failing_membrane_and_both_paths_agree() {
        // Break each membrane in turn; reference and circuit must agree on the
        // refusal index (canonical order: 1 drift .. 5 conflict).
        let mut drift = all_pass();
        drift.live_profile = 1;
        assert_refused_at(&drift, 1);

        let mut support = all_pass();
        support.requirements[0].enforcement = 0;
        assert_refused_at(&support, 2);

        let mut evidence = all_pass();
        evidence.requirements[0].evidence_required = 0b1000;
        evidence.requirements[0].evidence_available = 0b0001;
        assert_refused_at(&evidence, 3);

        let mut budget = all_pass();
        budget.budget[0].requested = 99;
        budget.budget[0].available = 1;
        assert_refused_at(&budget, 4);

        let mut conflict = all_pass();
        conflict.requirements[0].conflict_present = 0b0011;
        conflict.requirements[0].conflict_forbidden = 0b0011;
        assert_refused_at(&conflict, 5);
    }

    fn assert_refused_at(inputs: &AdmissionInputs, expected: u64) {
        let refusal = match shadow_check(inputs).expect("no divergence") {
            AdmissionOutcome::Refused {
                membrane,
                refusal_code,
                ..
            } => Some((membrane, refusal_code)),
            AdmissionOutcome::Admitted { .. } => None,
        };
        assert_eq!(
            refusal,
            Some((expected, expected)),
            "must refuse at the first failing membrane ({expected})"
        );
    }

    #[test]
    fn divergence_detector_fires_on_a_planted_outcome_mismatch() {
        // The reference admits but a (hypothetical) circuit refuses — the detector
        // must turn this into a typed hard finding, not swallow it.
        let reference = reference_admission(&all_pass());
        let wrong_circuit = AdmissionOutcome::Refused {
            membrane: 2,
            refusal_code: 2,
            trace: vec![true, false, true, true, true],
        };
        let result = decide(reference.clone(), Ok(wrong_circuit.clone()));
        assert_eq!(
            result,
            Err(AdmissionDivergence::OutcomeMismatch {
                reference,
                circuit: wrong_circuit,
            })
        );
    }

    #[test]
    fn agreement_is_not_a_divergence() {
        let reference = reference_admission(&all_pass());
        assert!(decide(reference.clone(), Ok(reference)).is_ok());
    }

    #[test]
    fn a_circuit_error_is_a_divergence() {
        let reference = reference_admission(&all_pass());
        let result = decide(reference, Err("circuit evaluation failed"));
        assert!(matches!(
            result,
            Err(AdmissionDivergence::CircuitError {
                reason: "circuit evaluation failed",
                ..
            })
        ));
    }
}
