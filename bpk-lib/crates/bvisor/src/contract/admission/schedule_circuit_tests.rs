//! Equivalence + shadow-parity proof for the schedule membrane circuit: every input
//! the imperative reference judges, the NC¹ circuit must judge identically.

use super::super::schedule::{
    reference_schedule_admission, PrimitiveDeclInputs, ScheduleInputs, ScheduleOutcome,
    ScheduleRefusal, ScheduleSlotInputs,
};
use super::{circuit_schedule_admission, decide, schedule_shadow_check, ScheduleDivergence};

const NS: u8 = 0; // NamespaceCreate
const FS: u8 = 1; // FsSetup

fn decl(
    phase: u8,
    covers: u64,
    prereq: u64,
    conflict: u64,
    dd: u64,
    pd: u64,
) -> PrimitiveDeclInputs {
    PrimitiveDeclInputs {
        phase,
        covers,
        prerequisites: prereq,
        conflicts: conflict,
        decl_digest: dd,
        param_digest: pd,
    }
}

fn slot(primitive: u64, dd: u64, pd: u64) -> ScheduleSlotInputs {
    ScheduleSlotInputs {
        primitive,
        claimed_decl_digest: dd,
        claimed_param_digest: pd,
    }
}

/// The canonical 3-primitive universe + admitted order `[p0, p1, p2]` (p1 needs p0).
fn canonical() -> ScheduleInputs {
    ScheduleInputs {
        declarations: vec![
            decl(NS, 0b001, 0, 0, 0xA0, 0xB0),
            decl(FS, 0b010, 0b001, 0, 0xA1, 0xB1),
            decl(FS, 0b100, 0, 0, 0xA2, 0xB2),
        ],
        schedule: vec![
            slot(0, 0xA0, 0xB0),
            slot(1, 0xA1, 0xB1),
            slot(2, 0xA2, 0xB2),
        ],
        required: 0b111,
    }
}

/// The circuit and reference agree on these inputs (shadow returns the authoritative
/// outcome only when both paths match). Asserts the OUTCOME too, so a "both wrong
/// identically" can't hide. `expect` surfaces the typed [`ScheduleDivergence`] (Debug).
fn assert_parity(inputs: &ScheduleInputs, expected: ScheduleOutcome) {
    let outcome = schedule_shadow_check(inputs).expect("circuit diverged from reference");
    assert_eq!(outcome, expected, "shadow outcome");
    assert_eq!(
        reference_schedule_admission(inputs),
        expected,
        "reference outcome"
    );
}

fn refused(reason: ScheduleRefusal) -> ScheduleOutcome {
    ScheduleOutcome::Refused { reason }
}

#[test]
fn circuit_admits_the_canonical_schedule() {
    assert_parity(&canonical(), ScheduleOutcome::Admitted);
    // The empty schedule with no requirements also admits in both paths.
    assert_parity(
        &ScheduleInputs {
            declarations: vec![],
            schedule: vec![],
            required: 0,
        },
        ScheduleOutcome::Admitted,
    );
}

#[test]
fn circuit_matches_reference_on_every_refusal_reason() {
    // (1) out of range.
    let mut a = canonical();
    a.schedule[2].primitive = 9;
    assert_parity(&a, refused(ScheduleRefusal::IndexOutOfRange));

    // (2) duplicate.
    let mut a = canonical();
    a.schedule[2] = slot(0, 0xA0, 0xB0);
    assert_parity(&a, refused(ScheduleRefusal::DuplicatePrimitive));

    // (3) decl integrity (stale decl digest, then stale param digest).
    let mut a = canonical();
    a.schedule[0].claimed_decl_digest = 0xFF;
    assert_parity(&a, refused(ScheduleRefusal::DeclIntegrity));
    let mut a = canonical();
    a.schedule[1].claimed_param_digest = 0xFF;
    assert_parity(&a, refused(ScheduleRefusal::DeclIntegrity));

    // (4) missing prerequisite (p1 needs an unscheduled index 3).
    let mut a = canonical();
    a.declarations[1].prerequisites = 0b001 | (1 << 3);
    assert_parity(&a, refused(ScheduleRefusal::MissingPrerequisite));

    // (5) conflict co-present.
    let mut a = canonical();
    a.declarations[0].conflicts = 0b100;
    assert_parity(&a, refused(ScheduleRefusal::ConflictCoPresent));

    // (6) prerequisite after dependent.
    let mut a = canonical();
    a.schedule = vec![a.schedule[1], a.schedule[0], a.schedule[2]];
    assert_parity(&a, refused(ScheduleRefusal::PrereqOutOfOrder));

    // (6) smuggled 2-cycle (both FS so phase order cannot pre-empt).
    let mut a = canonical();
    a.declarations[0].phase = FS;
    a.declarations[0].prerequisites = 0b010;
    assert_parity(&a, refused(ScheduleRefusal::PrereqOutOfOrder));

    // (7) phase inversion: [p2(FS), p0(NS), p1(FS)].
    let mut a = canonical();
    a.schedule = vec![a.schedule[2], a.schedule[0], a.schedule[1]];
    assert_parity(&a, refused(ScheduleRefusal::PhaseOutOfOrder));

    // (8) uncovered requirement.
    let mut a = canonical();
    a.required = 0b1000;
    assert_parity(&a, refused(ScheduleRefusal::RequirementUncovered));

    // (9) valid but non-canonical: [p0, p2, p1].
    let mut a = canonical();
    a.schedule = vec![a.schedule[0], a.schedule[2], a.schedule[1]];
    assert_parity(&a, refused(ScheduleRefusal::NonCanonical));
}

#[test]
fn priority_order_agrees_under_simultaneous_violations() {
    // Conflict (5) AND non-canonical (9): both paths must report the higher-priority
    // conflict, and they must agree.
    let mut a = canonical();
    a.declarations[0].conflicts = 0b100;
    a.schedule = vec![a.schedule[0], a.schedule[2], a.schedule[1]];
    assert_parity(&a, refused(ScheduleRefusal::ConflictCoPresent));
}

/// Brute-force equivalence: over a fixed universe, drive EVERY slot-index assignment in
/// `{0,1,2,3}^3` (3 = an out-of-range index) through both paths and assert they never
/// diverge — exercising in-range, duplicate, prereq-order, phase-order, and canonicality
/// across the whole order space.
fn sweep_universe(universe: &[PrimitiveDeclInputs], required: u64) {
    let digest_of = |idx: u64| -> (u64, u64) {
        universe
            .get(usize::try_from(idx).unwrap_or(usize::MAX))
            .map_or((0, 0), |d| (d.decl_digest, d.param_digest))
    };
    let span = u64::try_from(universe.len() + 1).unwrap_or(u64::MAX); // include one OOR index
    for a in 0..span {
        for b in 0..span {
            for c in 0..span {
                let mk = |i: u64| {
                    let (dd, pd) = digest_of(i);
                    slot(i, dd, pd)
                };
                let inputs = ScheduleInputs {
                    declarations: universe.to_vec(),
                    schedule: vec![mk(a), mk(b), mk(c)],
                    required,
                };
                let reference = reference_schedule_admission(&inputs);
                let outcome = schedule_shadow_check(&inputs)
                    .expect("circuit diverged from reference under the sweep");
                assert_eq!(outcome, reference, "parity at [{a},{b},{c}]");
            }
        }
    }
}

#[test]
fn exhaustive_equivalence_over_the_order_space() {
    // Universe A: p1 needs p0; phases NS,FS,FS; full coverage required.
    sweep_universe(
        &[
            decl(NS, 0b001, 0, 0, 0xA0, 0xB0),
            decl(FS, 0b010, 0b001, 0, 0xA1, 0xB1),
            decl(FS, 0b100, 0, 0, 0xA2, 0xB2),
        ],
        0b111,
    );
    // Universe B: p0 and p2 conflict; p2 needs p1; tighter phases — exercises conflict,
    // a cross-phase prereq, and partial coverage.
    sweep_universe(
        &[
            decl(NS, 0b001, 0, 0b100, 0xC0, 0xD0),
            decl(NS, 0b010, 0, 0, 0xC1, 0xD1),
            decl(FS, 0b100, 0b010, 0b001, 0xC2, 0xD2),
        ],
        0b010,
    );
    // Universe C: no edges, all same phase — pure canonicality/duplicate space.
    sweep_universe(
        &[
            decl(FS, 0b001, 0, 0, 0xE0, 0xF0),
            decl(FS, 0b010, 0, 0, 0xE1, 0xF1),
            decl(FS, 0b100, 0, 0, 0xE2, 0xF2),
        ],
        0,
    );
}

#[test]
fn integrity_violations_are_caught_under_the_sweep() {
    // Flip one slot's claimed digest across the order space: every in-range, distinct,
    // prereq/phase/coverage-clean placement must refuse at decl-integrity in BOTH paths.
    let universe = [
        decl(NS, 0b001, 0, 0, 0xA0, 0xB0),
        decl(FS, 0b010, 0b001, 0, 0xA1, 0xB1),
        decl(FS, 0b100, 0, 0, 0xA2, 0xB2),
    ];
    let mut inputs = ScheduleInputs {
        declarations: universe.to_vec(),
        schedule: vec![
            slot(0, 0xA0, 0xB0),
            slot(1, 0xA1, 0xB1),
            slot(2, 0xA2, 0xB2),
        ],
        required: 0b111,
    };
    inputs.schedule[1].claimed_decl_digest = 0x55; // forged
    let reference = reference_schedule_admission(&inputs);
    assert_eq!(reference, refused(ScheduleRefusal::DeclIntegrity));
    assert_eq!(
        circuit_schedule_admission(&inputs).expect("circuit evaluates"),
        reference
    );
}

#[test]
fn divergence_detector_fires_on_a_planted_mismatch() {
    // The reference admits; a planted circuit that refuses must become a hard finding.
    let reference = reference_schedule_admission(&canonical());
    let planted = refused(ScheduleRefusal::NonCanonical);
    let result = decide(reference, Ok(planted));
    assert_eq!(
        result,
        Err(ScheduleDivergence::OutcomeMismatch {
            reference,
            circuit: planted,
        })
    );
}

#[test]
fn agreement_is_not_a_divergence_and_a_circuit_error_is() {
    let reference = reference_schedule_admission(&canonical());
    assert!(decide(reference, Ok(reference)).is_ok());
    let reference = reference_schedule_admission(&canonical());
    assert!(matches!(
        decide(reference, Err("circuit evaluation failed")),
        Err(ScheduleDivergence::CircuitError {
            reason: "circuit evaluation failed",
            ..
        })
    ));
}

// --- The completeness grid: the 12 named adversarial schedules the membrane must
// independently reject. Each carries a stable id, its frozen refusal code, and is
// asserted on BOTH paths (the imperative reference AND the NC¹ circuit, via shadow
// parity) — so the membrane is provably complete, not merely "green on the happy path."

/// One named red fixture: a stable id, the adversarial input, and the refusal both
/// paths must produce.
struct RedFixture {
    id: &'static str,
    inputs: ScheduleInputs,
    expect: ScheduleRefusal,
}

fn red_fixtures() -> Vec<RedFixture> {
    let f = |id, inputs, expect| RedFixture { id, inputs, expect };

    // 1. unknown primitive id — a slot names an index outside the universe.
    let unknown = {
        let mut a = canonical();
        a.schedule[2].primitive = 9;
        a
    };
    // 2. wrong primitive version — the slot claims the digest of a different version
    //    (version is bound into decl_digest), so it fails authentication.
    let wrong_version = {
        let mut a = canonical();
        a.schedule[1].claimed_decl_digest = 0x1234; // "version 2" digest
        a
    };
    // 3. duplicate primitive — the same index occupies two slots.
    let duplicate = {
        let mut a = canonical();
        a.schedule[2] = slot(0, 0xA0, 0xB0);
        a
    };
    // 4. missing prerequisite — a present primitive needs an unscheduled one (index 3).
    let missing = {
        let mut a = canonical();
        a.declarations[1].prerequisites = 0b001 | (1 << 3);
        a
    };
    // 5. prerequisite appears later — p1 (needs p0) is placed before p0.
    let prereq_later = {
        let mut a = canonical();
        a.schedule = vec![a.schedule[1], a.schedule[0], a.schedule[2]];
        a
    };
    // 6. cycle smuggled through declarations — p0 ⇄ p1, same phase (no phase pre-empt).
    let cycle = {
        let mut a = canonical();
        a.declarations[0].phase = FS;
        a.declarations[0].prerequisites = 0b010;
        a
    };
    // 7. conflicting primitives co-present — p0 conflicts with the scheduled p2.
    let conflict = {
        let mut a = canonical();
        a.declarations[0].conflicts = 0b100;
        a
    };
    // 8. phase inversion — [p2(FS), p0(NS), p1(FS)] descends FS→NS.
    let phase_inv = {
        let mut a = canonical();
        a.schedule = vec![a.schedule[2], a.schedule[0], a.schedule[1]];
        a
    };
    // 9. uncovered boundary requirement — a required kind no primitive covers.
    let uncovered = {
        let mut a = canonical();
        a.required = 0b1000;
        a
    };
    // 10. altered parameters with a stale digest — claimed param digest != trusted.
    let stale_params = {
        let mut a = canonical();
        a.schedule[0].claimed_param_digest = 0x99;
        a
    };
    // 11. valid but non-canonical ordering — [p0, p2, p1] passes validity, fails Kahn.
    let noncanonical = {
        let mut a = canonical();
        a.schedule = vec![a.schedule[0], a.schedule[2], a.schedule[1]];
        a
    };
    // 12. schedule built from a different backend profile — a different profile yields
    //     different declarations, so the claimed digests no longer authenticate.
    let other_profile = {
        let mut a = canonical();
        a.schedule[2].claimed_decl_digest = 0xDEAD; // declaration from profile P'
        a
    };

    use ScheduleRefusal as R;
    vec![
        f("unknown-primitive-id", unknown, R::IndexOutOfRange),
        f("wrong-primitive-version", wrong_version, R::DeclIntegrity),
        f("duplicate-primitive", duplicate, R::DuplicatePrimitive),
        f("missing-prerequisite", missing, R::MissingPrerequisite),
        f(
            "prerequisite-appears-later",
            prereq_later,
            R::PrereqOutOfOrder,
        ),
        f("cycle-smuggled", cycle, R::PrereqOutOfOrder),
        f("conflicting-primitives", conflict, R::ConflictCoPresent),
        f("phase-inversion", phase_inv, R::PhaseOutOfOrder),
        f("uncovered-requirement", uncovered, R::RequirementUncovered),
        f("altered-parameters", stale_params, R::DeclIntegrity),
        f("valid-noncanonical", noncanonical, R::NonCanonical),
        f("different-backend-profile", other_profile, R::DeclIntegrity),
    ]
}

#[test]
fn the_twelve_red_fixtures_are_each_rejected_on_both_paths() {
    let fixtures = red_fixtures();
    assert_eq!(
        fixtures.len(),
        12,
        "the completeness grid has all 12 fixtures"
    );
    for RedFixture { id, inputs, expect } in &fixtures {
        // The authoritative imperative reference rejects with the expected reason...
        assert_eq!(
            reference_schedule_admission(inputs),
            refused(*expect),
            "reference must reject `{id}` as {expect} (code {})",
            expect.code()
        );
        // ...and the NC¹ circuit agrees (shadow parity returns Ok only on a full match).
        match schedule_shadow_check(inputs) {
            Ok(outcome) => {
                assert_eq!(
                    outcome,
                    refused(*expect),
                    "circuit must reject `{id}` as {expect}"
                );
            }
            Err(divergence) => {
                // Test-only intentional failure with dynamic context (no `panic!` macro).
                let message = format!("fixture `{id}` diverged: {divergence}");
                std::hint::black_box(Option::<()>::None).expect(&message);
            }
        }
    }
}
