//! Metacircular tests for the repo-IR fold-fusion runner (item 6.2).
//!
//! The headline law [`run_fused_equals_run_separate`] is the metacircular tie:
//! the gauntlet's own fitness fold-fusion obeys the SAME equivalence law that
//! `project_fused2/3` is tested against for event projections — `run_fused(ir)`
//! produces the identical finding SET as running each fitness in its own pass.
//! This holds for ≥2 fact families (Gate + DocInvariant columns).

use super::{
    build, run_fitness, run_fitness_separately, BlockingGateNamesRedFixture, DocInvariantFact,
    Fitness, GateFact, NodeKind, RepoIr, WitnessTestIsPathFnShaped,
};
use crate::repo_surface::repo_root;

fn repo() -> std::path::PathBuf {
    repo_root().expect("repo root resolves from tools/integrity")
}

/// A small synthetic IR with planted violations so the fitnesses are NON-vacuous:
/// one blocking gate with no red fixture, one invariant with a malformed witness.
fn synthetic_ir_with_violations() -> RepoIr {
    RepoIr {
        schema_version: 1,
        generated_by: "test",
        al_assignments: Vec::new(),
        gates: vec![
            GateFact {
                slug: "good-gate".to_owned(),
                has_blocking_authority: true,
                red_fixture_test: Some("tools/integrity/src/x.rs::t".to_owned()),
            },
            GateFact {
                slug: "bad-gate".to_owned(),
                has_blocking_authority: true,
                red_fixture_test: None,
            },
        ],
        waivers: Vec::new(),
        public_surface: Vec::new(),
        mutation_seams: Vec::new(),
        doc_invariants: vec![
            DocInvariantFact {
                id: "INV-GOOD".to_owned(),
                statement: "ok".to_owned(),
                witness_test: Some("tests/foo.rs::bar".to_owned()),
            },
            DocInvariantFact {
                id: "INV-BAD".to_owned(),
                statement: "ok".to_owned(),
                witness_test: Some("not-path-fn-shaped".to_owned()),
            },
        ],
    }
}

/// THE METACIRCULAR LAW: fused fold == separate folds (as a set), over ≥2 facts.
#[test]
fn run_fused_equals_run_separate() {
    let ir = synthetic_ir_with_violations();
    let g = BlockingGateNamesRedFixture;
    let w = WitnessTestIsPathFnShaped;
    let fitnesses: Vec<&dyn Fitness> = vec![&g, &w];

    let fused = run_fitness(&ir, &fitnesses);
    let separate = run_fitness_separately(&ir, &fitnesses);

    assert_eq!(
        fused, separate,
        "fold-fusion law violated: run_fused != run_separate"
    );
}

/// Anti-vacuity: the fitnesses actually FLAG the planted violations (so the
/// equality law above is not comparing two empty vecs).
#[test]
fn skeleton_fitnesses_flag_planted_violations() {
    let ir = synthetic_ir_with_violations();
    let g = BlockingGateNamesRedFixture;
    let w = WitnessTestIsPathFnShaped;
    let fitnesses: Vec<&dyn Fitness> = vec![&g, &w];

    let findings = run_fitness(&ir, &fitnesses);
    assert_eq!(
        findings.len(),
        2,
        "expected exactly the two planted findings"
    );
    assert!(findings
        .iter()
        .any(|f| f.fitness == "blocking-gate-names-red-fixture"));
    assert!(findings
        .iter()
        .any(|f| f.fitness == "witness-test-is-path-fn-shaped"));
}

/// The live tree's IR is clean for the skeleton fitnesses (no false positives):
/// every blocking gate names a red fixture and every witness_test is `path::fn`.
#[test]
fn live_repo_ir_is_clean_for_skeleton_fitnesses() {
    let ir = build(&repo()).expect("build repo-ir from live tree");
    let g = BlockingGateNamesRedFixture;
    let w = WitnessTestIsPathFnShaped;
    let fitnesses: Vec<&dyn Fitness> = vec![&g, &w];

    let findings = run_fitness(&ir, &fitnesses);
    assert!(
        findings.is_empty(),
        "live repo-IR should be clean for skeleton fitnesses; got {findings:?}"
    );
}

/// The IR binds all six fact families non-vacuously on the live tree.
#[test]
fn live_repo_ir_binds_all_fact_families() {
    let ir = build(&repo()).expect("build repo-ir from live tree");
    assert!(!ir.gates.is_empty(), "gate ownership column empty");
    assert!(!ir.mutation_seams.is_empty(), "mutation-seam column empty");
    assert!(
        !ir.doc_invariants.is_empty(),
        "docs-traceability column empty"
    );
    assert!(!ir.al_assignments.is_empty(), "AL-assignment column empty");
    assert!(!ir.public_surface.is_empty(), "public-surface column empty");
}

/// Sanity: the registered fitness set covers ≥2 distinct node-kind columns (the
/// fold-fusion property is only meaningful across multiple facts).
#[test]
fn registered_fitnesses_span_multiple_columns() {
    let kinds: std::collections::BTreeSet<NodeKind> = super::registered_fitnesses()
        .iter()
        .map(|f| f.over())
        .collect();
    assert!(
        kinds.len() >= 2,
        "fold-fusion needs ≥2 fact families; got {kinds:?}"
    );
}
