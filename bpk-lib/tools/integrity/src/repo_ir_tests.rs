//! Metacircular tests for the repo-IR fold-fusion runner (item 6.2).
//!
//! The headline law [`run_fused_equals_run_separate`] is the metacircular tie:
//! the gauntlet's own fitness fold-fusion obeys the SAME equivalence law that
//! `project_fused2/3` is tested against for event projections — `run_fused(ir)`
//! produces the identical finding SET as running each fitness in its own pass.
//! This holds for ≥2 fact families (Gate + DocInvariant columns).

use super::{
    build, check, run_fitness, run_fitness_separately, BlockingGateNamesRedFixture,
    DocInvariantFact, Fitness, GateFact, MutationSeamFact, NodeKind, RepoIr, SeamLevelIsRecognized,
    WitnessTestIsPathFnShaped,
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

/// A synthetic IR carrying ONE seam row with an unrecognized assurance level, so
/// the `SeamLevelIsRecognized` fitness has a planted violation to bite on.
fn synthetic_ir_with_bad_seam_level() -> RepoIr {
    RepoIr {
        schema_version: 1,
        generated_by: "test",
        al_assignments: Vec::new(),
        gates: Vec::new(),
        waivers: Vec::new(),
        public_surface: Vec::new(),
        mutation_seams: vec![
            MutationSeamFact {
                slug: "good-seam".to_owned(),
                glob: "crates/core/src/store/mod.rs".to_owned(),
                assurance_level: "L4".to_owned(),
            },
            MutationSeamFact {
                slug: "bad-seam".to_owned(),
                glob: "crates/core/src/store/x.rs".to_owned(),
                assurance_level: "L9".to_owned(),
            },
        ],
        doc_invariants: Vec::new(),
    }
}

/// RED FIXTURE for the `repo-ir-fitness` gate (GateNegativePath): the seam-level
/// fitness flags exactly the planted unrecognized-tier row and nothing else. This
/// is the anti-vacuous proof that the blocking seam fitness bites a real
/// violation — named by `gate_registry::GATES` as the gate's red fixture.
#[test]
fn detector_rejects_planted_bad_seam_level() {
    let ir = synthetic_ir_with_bad_seam_level();
    let s = SeamLevelIsRecognized;
    let fitnesses: Vec<&dyn Fitness> = vec![&s];
    let findings = run_fitness(&ir, &fitnesses);
    // The required finding-set must NOT be empty: the gate bites the planted
    // unrecognized-tier row (`is_empty` is the recognized negative-path token).
    assert!(
        !findings.is_empty(),
        "seam-level gate must flag the planted bad-level seam"
    );
    assert_eq!(
        findings.len(),
        1,
        "expected exactly the one planted bad-level finding"
    );
    assert_eq!(findings[0].fitness, "seam-level-is-recognized");
    assert!(
        findings[0].message.contains("bad-seam"),
        "finding must name the offending seam; got {}",
        findings[0].message
    );
}

/// The seam-level fitness does NOT false-flag a recognized tier (the `good-seam`
/// row above declares `L4`).
#[test]
fn seam_level_fitness_passes_recognized_tier() {
    let ir = synthetic_ir_with_bad_seam_level();
    let s = SeamLevelIsRecognized;
    let fitnesses: Vec<&dyn Fitness> = vec![&s];
    let findings = run_fitness(&ir, &fitnesses);
    assert!(
        findings.iter().all(|f| !f.message.contains("good-seam")),
        "recognized L4 seam must not be flagged"
    );
}

/// The BLOCKING gate is clean on the live tree: every seam parsed from
/// `seam_registry.yaml` declares a recognized tier AND its glob matches a tracked
/// file. This is the green half — `check` returns `Ok` with a non-vacuous receipt.
#[test]
fn live_repo_ir_fitness_gate_is_clean() {
    let work = check(&repo()).expect("repo-ir-fitness gate must pass on the live tree");
    assert!(
        work.files_examined > 0 && work.assertions_run > 0,
        "gate receipt must be non-vacuous; got files={} assertions={}",
        work.files_examined,
        work.assertions_run
    );
}

/// The seam column is PARSED from `seam_registry.yaml` (D9 parse-not-mirror): the
/// live IR's seam slugs equal the registry's slug set, and each row carries the
/// registry's declared assurance level — proving the column is no longer mirrored
/// from the in-code `CRITICAL_SEAM_MUTANT_GLOBS` array.
#[test]
fn seam_column_is_parsed_from_registry() {
    let ir = build(&repo()).expect("build repo-ir");
    let registry = crate::assurance::load_seam_registry(&repo()).expect("load seam registry");
    let ir_slugs: std::collections::BTreeSet<&str> =
        ir.mutation_seams.iter().map(|s| s.slug.as_str()).collect();
    let reg_slugs: std::collections::BTreeSet<&str> =
        registry.iter().map(|s| s.slug.as_str()).collect();
    assert_eq!(
        ir_slugs, reg_slugs,
        "seam column slugs must equal registry slugs"
    );
    // Every IR seam row's level matches the registry entry's level (parse, not mirror).
    for fact in &ir.mutation_seams {
        let entry = registry
            .iter()
            .find(|e| e.slug == fact.slug)
            .expect("registry has the seam");
        assert_eq!(
            fact.assurance_level, entry.assurance_level,
            "seam {} level must come from the registry",
            fact.slug
        );
    }
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
