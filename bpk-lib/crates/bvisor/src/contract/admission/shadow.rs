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

use super::compile::{compile_admission, compile_budget_detail, AdmissionShape};
use super::eval::{evaluate, Lane};
use super::program::Width;

/// Lane width (bits) of the evidence / conflict / budget / hash lanes in the shadow
/// encoding. Enforcement is the 2-bit code the circuit uses internally.
const FIELD_BITS: u32 = 8;
const ENFORCEMENT_BITS: u32 = 2;

/// The 1-based index of the budget membrane in the canonical order (drift, support,
/// evidence, BUDGET, conflict). The budget dimension/reason selectors are meaningful
/// only when the budget membrane is the first-failing one.
const BUDGET_MEMBRANE: u64 = 4;

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

/// One budget dimension's full admission inputs (the per-dimension `B` slice): the
/// request `(limit, derived-minimum, guarantee, evidence)` vs the backend's
/// availability `(available, guarantee, evidence)`. The membrane passes the
/// dimension iff `D ≤ L ∧ L ≤ A ∧ G_req ≤ G_avail ∧ E_req ⊆ E_avail` (the two-phase
/// admission of [`crate::contract::budget`], flattened to a single pass bit here —
/// the per-dimension reason selector is a later step).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetInputs {
    /// Requested limit `L_d`.
    pub limit: u64,
    /// Backend-available limit `A_d`.
    pub available: u64,
    /// Derived structural minimum `D_d` (the intrinsic floor, `L_d ≥ D_d`).
    pub derived_min: u64,
    /// Required guarantee code `G_d`: `1` Mediated, `2` Enforced.
    pub guarantee_required: u8,
    /// Backend guarantee code `E_d`: `0` Unsupported, `1` Mediated, `2` Enforced.
    pub guarantee_available: u8,
    /// Required evidence bitset `Q_d`.
    pub evidence_required: u64,
    /// Backend-available evidence bitset `C_d`.
    pub evidence_available: u64,
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
    pub budget: Vec<BudgetInputs>,
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
        /// First-failing budget dimension `1..=7` (`0` unless the refusal is at the
        /// budget membrane). Wall, cpu, resident, process, handle, storage, network.
        budget_dimension: u64,
        /// First-failing budget reason `1..=4` (`0` unless a budget refusal):
        /// `1` BelowDerivedMinimum, `2` CapacityExceeded, `3` GuaranteeInsufficient,
        /// `4` EvidenceMissing.
        budget_reason: u64,
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
    // The budget membrane passes iff every dimension has no failing reason.
    let budget = inputs.budget.iter().all(|d| budget_dim_reason(d) == 0);
    let conflict = inputs.requirements.iter().all(|r| {
        (mask(r.conflict_present, FIELD_BITS) & mask(r.conflict_forbidden, FIELD_BITS)) == 0
    });
    vec![drift, support, evidence, budget, conflict]
}

/// One budget dimension's first-failing reason in canonical order (`1`
/// BelowDerivedMinimum, `2` CapacityExceeded, `3` GuaranteeInsufficient, `4`
/// EvidenceMissing), or `0` if the dimension passes. Masked to the same lane widths
/// the circuit sees, so the imperative twin agrees on out-of-range inputs.
fn budget_dim_reason(d: &BudgetInputs) -> u8 {
    let limit = mask(d.limit, FIELD_BITS);
    let available = mask(d.available, FIELD_BITS);
    let derived = mask(d.derived_min, FIELD_BITS);
    let g_req = mask(u64::from(d.guarantee_required), ENFORCEMENT_BITS);
    let g_avail = mask(u64::from(d.guarantee_available), ENFORCEMENT_BITS);
    let e_req = mask(d.evidence_required, FIELD_BITS);
    let e_avail = mask(d.evidence_available, FIELD_BITS);
    if derived > limit {
        1
    } else if limit > available {
        2
    } else if g_req > g_avail {
        3
    } else if (e_req & !e_avail) != 0 {
        4
    } else {
        0
    }
}

/// The packed `(first-failing dimension, reason)` budget detail `(dim << 3) | reason`,
/// `0` if every dimension passes — the imperative twin of [`compile_budget_detail`].
fn reference_budget_detail(inputs: &AdmissionInputs) -> u64 {
    for (index, d) in inputs.budget.iter().enumerate() {
        let reason = budget_dim_reason(d);
        if reason != 0 {
            let dimension = u64::try_from(index + 1).unwrap_or(0);
            return (dimension << 3) | u64::from(reason);
        }
    }
    0
}

/// Split the budget membrane's first-failing membrane index into the dimension +
/// reason selectors, but ONLY when the budget membrane is the refusing one (`detail`
/// is gated so an earlier membrane's refusal carries no budget detail).
fn budget_selectors(membrane: u64, detail: u64) -> (u64, u64) {
    if membrane == BUDGET_MEMBRANE {
        (detail >> 3, detail & 0b111)
    } else {
        (0, 0)
    }
}

/// Build the outcome from a membrane trace (first failing membrane = refusal),
/// attaching the budget dimension/reason selectors on a budget refusal.
pub(crate) fn outcome_from_trace(trace: Vec<bool>, budget_detail: u64) -> AdmissionOutcome {
    match trace.iter().position(|pass| !pass) {
        None => AdmissionOutcome::Admitted { trace },
        Some(i) => {
            let index = u64::try_from(i + 1).unwrap_or(0);
            let (budget_dimension, budget_reason) = budget_selectors(index, budget_detail);
            AdmissionOutcome::Refused {
                membrane: index,
                refusal_code: index,
                trace,
                budget_dimension,
                budget_reason,
            }
        }
    }
}

/// The authoritative imperative admission decision over normalized inputs.
#[must_use]
pub fn reference_admission(inputs: &AdmissionInputs) -> AdmissionOutcome {
    outcome_from_trace(reference_trace(inputs), reference_budget_detail(inputs))
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
    lanes.extend(encode_budget(inputs));
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

/// Encode JUST the budget lanes, in the canonical order both `compile_admission` and
/// `compile_budget_detail` read: limit, available, derived-min (field width),
/// guarantee-required, guarantee-available (enforcement width), evidence-required,
/// evidence-available (field width) — each × dims.
fn encode_budget(inputs: &AdmissionInputs) -> Vec<Lane> {
    let field = field_width();
    let enf = enforcement_lane_width();
    let mut lanes = Vec::new();
    lanes.extend(inputs.budget.iter().map(|d| encode_lane(d.limit, field)));
    lanes.extend(
        inputs
            .budget
            .iter()
            .map(|d| encode_lane(d.available, field)),
    );
    lanes.extend(
        inputs
            .budget
            .iter()
            .map(|d| encode_lane(d.derived_min, field)),
    );
    lanes.extend(
        inputs
            .budget
            .iter()
            .map(|d| encode_lane(u64::from(d.guarantee_required), enf)),
    );
    lanes.extend(
        inputs
            .budget
            .iter()
            .map(|d| encode_lane(u64::from(d.guarantee_available), enf)),
    );
    lanes.extend(
        inputs
            .budget
            .iter()
            .map(|d| encode_lane(d.evidence_required, field)),
    );
    lanes.extend(
        inputs
            .budget
            .iter()
            .map(|d| encode_lane(d.evidence_available, field)),
    );
    lanes
}

/// The shadow circuit's admission decision over the same normalized inputs. On a
/// refusal, a SECOND NC¹ circuit ([`compile_budget_detail`]) yields the packed budget
/// dimension/reason selector — so the circuit produces the SAME full outcome the
/// reference does (the selectors are circuit-computed, not reference-only).
fn circuit_admission(inputs: &AdmissionInputs) -> Result<AdmissionOutcome, &'static str> {
    let shape = shape_of(inputs);
    let program = compile_admission(&shape).map_err(|_| "circuit compilation failed")?;
    let decision = evaluate(&program, &encode(inputs)).map_err(|_| "circuit evaluation failed")?;
    if decision.admit {
        return Ok(AdmissionOutcome::Admitted {
            trace: decision.membranes,
        });
    }
    let detail_program = compile_budget_detail(inputs.budget.len(), field_width(), field_width())
        .map_err(|_| "budget detail compilation failed")?;
    let detail = evaluate(&detail_program, &encode_budget(inputs))
        .map_err(|_| "budget detail evaluation failed")?
        .refusal_code;
    let (budget_dimension, budget_reason) = budget_selectors(decision.refusal_code, detail);
    Ok(AdmissionOutcome::Refused {
        membrane: decision.refusal_code,
        refusal_code: decision.refusal_code,
        trace: decision.membranes,
        budget_dimension,
        budget_reason,
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
        AdmissionOutcome, BudgetInputs, RequirementInputs,
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
            budget: vec![BudgetInputs {
                limit: 10,
                available: 20,
                derived_min: 0,
                guarantee_required: 1,  // Mediated
                guarantee_available: 2, // Enforced (>= required)
                evidence_required: 0b0001,
                evidence_available: 0b1111, // superset
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
        budget.budget[0].limit = 99;
        budget.budget[0].available = 1; // 99 > 1 -> capacity fail
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
            budget_dimension: 0,
            budget_reason: 0,
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

    /// A budget refusal must name BOTH the dimension and the reason, with the
    /// circuit-computed selectors agreeing with the reference (shadow_check only
    /// returns Ok when the FULL outcome — including the selectors — matches).
    fn assert_budget_refusal(inputs: &AdmissionInputs, dimension: u64, reason: u64) {
        let got = match shadow_check(inputs).expect("no divergence (reference == circuit)") {
            AdmissionOutcome::Refused {
                membrane,
                budget_dimension,
                budget_reason,
                ..
            } => Some((membrane, budget_dimension, budget_reason)),
            AdmissionOutcome::Admitted { .. } => None,
        };
        assert_eq!(
            got,
            Some((4, dimension, reason)),
            "budget refusal must name dimension {dimension}, reason {reason}"
        );
    }

    #[test]
    fn budget_refusal_names_the_dimension_and_reason_with_circuit_parity() {
        // Two budget dimensions so the dimension-2 selector is reachable.
        let two_dim = || {
            let mut inputs = all_pass();
            let dim0 = inputs.budget[0];
            inputs.budget.push(dim0); // dimension 2 = a passing copy of dimension 1
            inputs
        };

        // Dimension 1, each reason in canonical order.
        let mut below = two_dim();
        below.budget[0].limit = 2;
        below.budget[0].derived_min = 5; // 5 > 2 -> BelowDerivedMinimum
        assert_budget_refusal(&below, 1, 1);

        let mut capacity = two_dim();
        capacity.budget[0].limit = 99;
        capacity.budget[0].available = 1; // 99 > 1 -> CapacityExceeded
        assert_budget_refusal(&capacity, 1, 2);

        let mut guarantee = two_dim();
        guarantee.budget[0].guarantee_required = 2; // Enforced
        guarantee.budget[0].guarantee_available = 1; // Mediated < Enforced
        assert_budget_refusal(&guarantee, 1, 3);

        let mut evidence = two_dim();
        evidence.budget[0].evidence_required = 0b1000;
        evidence.budget[0].evidence_available = 0b0001; // not a subset
        assert_budget_refusal(&evidence, 1, 4);

        // Dimension 1 passes, dimension 2 fails -> the dimension selector is 2.
        let mut second = two_dim();
        second.budget[1].limit = 99;
        second.budget[1].available = 1;
        assert_budget_refusal(&second, 2, 2);
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
