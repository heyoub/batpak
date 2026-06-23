//! Unit tests for the admission compiler (`super::compile`), split out to keep
//! `compile.rs` under the file-size ceiling (split, don't bump).

use super::super::eval::{evaluate, Lane};
use super::super::limits::FROZEN_LIMITS;
use super::super::program::{Outputs, Width};
use super::super::schedule_circuit::ScheduleShape;
use super::super::validate::validate;
use super::{
    compile_admission, compile_budget_detail, compile_budget_membrane, compile_conflict_membrane,
    compile_evidence_membrane, compile_profile_drift_membrane, compile_support_membrane,
    compose_membranes, AdmissionShape, CircuitBuilder,
};

fn w(bits: u16) -> Width {
    Width::new(bits).expect("valid width")
}

/// Encode an unsigned value into a `width`-bit lane (low `width` bits, LSB-first).
fn lane(value: u64, width: Width) -> Lane {
    Lane::from_le_bytes(&value.to_le_bytes(), width)
}

/// One budget dimension as `(limit, available, derived, g_req, g_avail, e_req,
/// e_avail)` — the tuple the test helpers build lanes / a reference from.
type Dim = (u64, u64, u64, u64, u64, u64, u64);

/// The imperative reference for ONE dimension the circuit must match: the
/// two-phase admission `D ≤ L ∧ L ≤ A ∧ G_req ≤ G_avail ∧ Q ⊆ C`.
fn budget_dim_reference(d: Dim) -> bool {
    let (limit, available, derived, g_req, g_avail, e_req, e_avail) = d;
    derived <= limit && limit <= available && g_req <= g_avail && (e_req & !e_avail) == 0
}

/// Build the `7·dims` budget lane vector in canonical order from per-dim tuples.
fn budget_lanes(dims: &[Dim], vw: Width, ew: Width) -> Vec<Lane> {
    let enf = w(2);
    let mut lanes = Vec::new();
    lanes.extend(dims.iter().map(|d| lane(d.0, vw))); // limit
    lanes.extend(dims.iter().map(|d| lane(d.1, vw))); // available
    lanes.extend(dims.iter().map(|d| lane(d.2, vw))); // derived
    lanes.extend(dims.iter().map(|d| lane(d.3, enf))); // guarantee-required
    lanes.extend(dims.iter().map(|d| lane(d.4, enf))); // guarantee-available
    lanes.extend(dims.iter().map(|d| lane(d.5, ew))); // evidence-required
    lanes.extend(dims.iter().map(|d| lane(d.6, ew))); // evidence-available
    lanes
}

#[test]
fn compiler_emits_a_program_the_validator_accepts() {
    let program = compile_budget_membrane(7, w(64), w(8)).expect("compile");
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
    // The discrete half of step-5 equivalence: exhaustive over the FULL per-
    // dimension domain of a 1-dim instance with 2-bit value + 2-bit evidence
    // lanes — all seven inputs, 4^7 = 16384 points. Flattened to one counter to
    // keep nesting shallow.
    let vw = w(2);
    let ew = w(2);
    let program = compile_budget_membrane(1, vw, ew).expect("compile");
    validate(&program, &FROZEN_LIMITS).expect("valid");
    for code in 0..4u64.pow(7) {
        let dim: Dim = (
            code % 4,
            (code / 4) % 4,
            (code / 16) % 4,
            (code / 64) % 4,
            (code / 256) % 4,
            (code / 1024) % 4,
            (code / 4096) % 4,
        );
        let decision = evaluate(&program, &budget_lanes(&[dim], vw, ew)).expect("eval");
        assert_eq!(
            decision.admit,
            budget_dim_reference(dim),
            "mismatch at dim={dim:?}"
        );
    }
}

/// The imperative reference for the packed budget detail `(dim<<3)|reason`.
fn budget_detail_reference(dims: &[Dim]) -> u64 {
    for (index, d) in dims.iter().enumerate() {
        let (limit, available, derived, g_req, g_avail, e_req, e_avail) = *d;
        let reason: u64 = if derived > limit {
            1
        } else if limit > available {
            2
        } else if g_req > g_avail {
            3
        } else if (e_req & !e_avail) != 0 {
            4
        } else {
            0
        };
        if reason != 0 {
            return (u64::try_from(index + 1).unwrap_or(0) << 3) | reason;
        }
    }
    0
}

#[test]
fn budget_detail_selector_equivalent_to_reference_exhaustively() {
    // The selector's packed (dimension, reason) must match the reference over the
    // FULL per-dimension domain (1 dim, 2-bit lanes, 4^7 = 16384 points).
    let vw = w(2);
    let ew = w(2);
    let program = compile_budget_detail(1, vw, ew).expect("compile");
    validate(&program, &FROZEN_LIMITS).expect("valid");
    for code in 0..4u64.pow(7) {
        let dim: Dim = (
            code % 4,
            (code / 4) % 4,
            (code / 16) % 4,
            (code / 64) % 4,
            (code / 256) % 4,
            (code / 1024) % 4,
            (code / 4096) % 4,
        );
        let decision = evaluate(&program, &budget_lanes(&[dim], vw, ew)).expect("eval");
        assert_eq!(
            decision.refusal_code,
            budget_detail_reference(&[dim]),
            "mismatch at dim={dim:?}"
        );
    }
}

#[test]
fn budget_detail_selects_the_lowest_index_failing_dimension() {
    let vw = w(8);
    let ew = w(8);
    let program = compile_budget_detail(3, vw, ew).expect("compile");
    // Dim 1 passes; dim 2 fails capacity (reason 2); dim 3 fails guarantee
    // (reason 3). The lowest-index failure (dim 2, reason 2) wins.
    let lanes = budget_lanes(
        &[
            (1, 10, 0, 1, 2, 0, 0),
            (99, 1, 0, 1, 2, 0, 0),
            (1, 10, 0, 2, 1, 0, 0),
        ],
        vw,
        ew,
    );
    let decision = evaluate(&program, &lanes).expect("eval");
    assert_eq!(decision.refusal_code, (2 << 3) | 2, "dimension 2, reason 2");
}

#[test]
fn refusal_code_is_zero_on_admit_and_one_on_refuse() {
    let vw = w(64);
    let ew = w(8);
    let program = compile_budget_membrane(3, vw, ew).expect("compile");
    // All three dimensions admit: derived <= limit <= available, guarantee met,
    // evidence a subset.
    let within = budget_lanes(
        &[
            (1, 10, 0, 1, 2, 0, 0),
            (2, 10, 0, 1, 2, 0, 0),
            (3, 10, 0, 1, 2, 0, 0),
        ],
        vw,
        ew,
    );
    let admitted = evaluate(&program, &within).expect("eval");
    assert!(admitted.admit);
    assert_eq!(admitted.refusal_code, 0);

    // A dimension requests more than available -> the single membrane fails
    // (refusal code 1 = this membrane's index).
    let over = budget_lanes(
        &[
            (99, 1, 0, 1, 2, 0, 0),
            (2, 10, 0, 1, 2, 0, 0),
            (3, 10, 0, 1, 2, 0, 0),
        ],
        vw,
        ew,
    );
    let refused = evaluate(&program, &over).expect("eval");
    assert!(!refused.admit);
    assert_eq!(refused.refusal_code, 1);
}

#[test]
fn zero_dimension_membrane_admits_vacuously() {
    let program = compile_budget_membrane(0, w(8), w(8)).expect("compile");
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
fn conflict_membrane_equivalent_to_reference_exhaustively() {
    // admit iff present_i ∩ forbidden_i == 0, per requirement. 2 reqs, 3-bit.
    let width = w(3);
    let program = compile_conflict_membrane(2, width).expect("compile");
    validate(&program, &FROZEN_LIMITS).expect("valid");
    for p0 in 0..8u64 {
        for p1 in 0..8u64 {
            for f0 in 0..8u64 {
                for f1 in 0..8u64 {
                    let inputs = [
                        lane(p0, width),
                        lane(p1, width),
                        lane(f0, width),
                        lane(f1, width),
                    ];
                    let decision = evaluate(&program, &inputs).expect("eval");
                    let reference = (p0 & f0) == 0 && (p1 & f1) == 0;
                    assert_eq!(
                        decision.admit, reference,
                        "mismatch present=[{p0:03b},{p1:03b}] forbidden=[{f0:03b},{f1:03b}]"
                    );
                }
            }
        }
    }
}

#[test]
fn profile_drift_membrane_admits_iff_hashes_match_exhaustively() {
    // admit iff planned == live. 4-bit hash lane => 16^2 = 256.
    let width = w(4);
    let program = compile_profile_drift_membrane(width).expect("compile");
    validate(&program, &FROZEN_LIMITS).expect("valid");
    for planned in 0..16u64 {
        for live in 0..16u64 {
            let inputs = [lane(planned, width), lane(live, width)];
            let decision = evaluate(&program, &inputs).expect("eval");
            assert_eq!(
                decision.admit,
                planned == live,
                "drift mismatch planned={planned} live={live}"
            );
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

/// An EMPTY schedule membrane (no declarations, no slots): every check passes
/// vacuously, so the 6th membrane admits and contributes only its single `required`
/// lane (set to `0` by [`admission_inputs`]). Lets these tests drive the other five
/// membranes by hand without modeling a full schedule.
fn empty_schedule_shape() -> ScheduleShape {
    ScheduleShape {
        declarations: 0,
        slots: 0,
        index_width: w(1),
        phase_width: w(1),
        digest_width: w(1),
        universe_width: w(1),
        covers_width: w(1),
    }
}

/// One requirement, one budget dim, all 2-bit lanes — small enough to drive the
/// full composed circuit by hand.
fn small_shape() -> AdmissionShape {
    AdmissionShape {
        requirements: 1,
        budget_dims: 1,
        budget_width: w(2),
        evidence_width: w(2),
        conflict_width: w(2),
        hash_width: w(2),
        schedule: empty_schedule_shape(),
    }
}

/// The per-aspect input values, in `compile_admission`'s lane order (one
/// requirement, one budget dimension).
struct Aspects {
    planned: u64,
    live: u64,
    enforcement: u64,
    required: u64,
    available: u64,
    budget_limit: u64,
    budget_available: u64,
    budget_derived: u64,
    budget_g_req: u64,
    budget_g_avail: u64,
    budget_e_req: u64,
    budget_e_avail: u64,
    present: u64,
    forbidden: u64,
}

/// An all-membranes-pass baseline for `small_shape`.
fn all_pass() -> Aspects {
    Aspects {
        planned: 1,
        live: 1,        // == planned -> drift passes
        enforcement: 2, // Enforced >= Mediated -> support passes
        required: 1,
        available: 3,    // 0b01 ⊆ 0b11 -> evidence passes
        budget_limit: 1, // 0 <= 1 <= 3 -> intrinsic + capacity pass
        budget_available: 3,
        budget_derived: 0,
        budget_g_req: 1,   // Mediated
        budget_g_avail: 2, // Enforced >= Mediated -> guarantee passes
        budget_e_req: 1,
        budget_e_avail: 3, // 0b01 ⊆ 0b11 -> budget evidence passes
        present: 1,
        forbidden: 2, // 0b01 ∩ 0b10 = 0 -> conflict passes
    }
}

fn admission_inputs(a: &Aspects) -> Vec<Lane> {
    vec![
        lane(a.planned, w(2)),
        lane(a.live, w(2)),
        lane(a.enforcement, w(2)),
        lane(a.required, w(2)),
        lane(a.available, w(2)),
        lane(a.budget_limit, w(2)),
        lane(a.budget_available, w(2)),
        lane(a.budget_derived, w(2)),
        lane(a.budget_g_req, w(2)),
        lane(a.budget_g_avail, w(2)),
        lane(a.budget_e_req, w(2)),
        lane(a.budget_e_avail, w(2)),
        lane(a.present, w(2)),
        lane(a.forbidden, w(2)),
        // The empty schedule membrane's only lane: `required = 0` (covers nothing, so
        // an empty schedule satisfies it) — see `empty_schedule_shape`.
        lane(0, w(1)),
    ]
}

#[test]
fn full_admission_admits_when_every_membrane_passes() {
    let program = compile_admission(&small_shape()).expect("compile");
    validate(&program, &FROZEN_LIMITS).expect("C emits a valid admission circuit");
    let decision = evaluate(&program, &admission_inputs(&all_pass())).expect("eval");
    assert!(decision.admit);
    assert_eq!(decision.refusal_code, 0);
}

#[test]
fn full_admission_refuses_at_the_first_failing_membrane() {
    let program = compile_admission(&small_shape()).expect("compile");
    // Break only one membrane (earlier ones still pass) and assert the refusal
    // code names it, in canonical order 1..=5.
    let assert_refuses = |index: u64, aspects: &Aspects| {
        let decision = evaluate(&program, &admission_inputs(aspects)).expect("eval");
        assert!(!decision.admit, "membrane {index} should refuse");
        assert_eq!(
            decision.refusal_code, index,
            "refusal must name the FIRST failing membrane ({index})"
        );
    };

    let mut drift = all_pass();
    drift.live = 2; // != planned
    assert_refuses(1, &drift);

    let mut support = all_pass();
    support.enforcement = 0; // Unsupported < Mediated
    assert_refuses(2, &support);

    let mut evidence = all_pass();
    evidence.required = 2;
    evidence.available = 1; // 0b10 ⊄ 0b01
    assert_refuses(3, &evidence);

    let mut budget = all_pass();
    budget.budget_limit = 3;
    budget.budget_available = 1; // 3 > 1 -> capacity fail
    assert_refuses(4, &budget);

    let mut conflict = all_pass();
    conflict.present = 3;
    conflict.forbidden = 3; // 0b11 ∩ 0b11 != 0
    assert_refuses(5, &conflict);
}
