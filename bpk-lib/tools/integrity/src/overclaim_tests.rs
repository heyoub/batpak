//! Unit tests for the over-claim detector: synthetic claim pools, planted temp
//! trees, and the live-tree gate (gate registry + triangulation).

use super::{
    check_overclaim_oracles, overclaim_findings, aspirational_pub_fn_subjects,
    PREDICATE_ASSERTION_TEST, PREDICATE_WITNESS_DELIVERED,
};
use crate::source_cache::SourceCache;
use crate::triangulation::{Claim, TriangulationEngine};

fn claim(oracle: &str, subject: &str, predicate: &str, value: &str) -> Claim {
    Claim {
        subject: subject.to_owned(),
        predicate: predicate.to_owned(),
        value: value.to_owned(),
        oracle: oracle.to_owned(),
    }
}

#[test]
fn overclaim_engine_flags_claim_yes_reality_no() {
    let pool = vec![
        claim("claim-oracle", "INV-PLANTED", PREDICATE_WITNESS_DELIVERED, "yes"),
        claim(
            "reality-oracle",
            "INV-PLANTED",
            PREDICATE_WITNESS_DELIVERED,
            "no",
        ),
    ];
    let findings = overclaim_findings(&TriangulationEngine::disagreements(&pool));
    assert_eq!(findings.len(), 1, "exactly one over-claim");
    assert_eq!(findings[0].subject, "INV-PLANTED");
}

#[test]
fn overclaim_engine_ignores_agreement() {
    let pool = vec![
        claim("claim-oracle", "INV-OK", PREDICATE_WITNESS_DELIVERED, "yes"),
        claim("reality-oracle", "INV-OK", PREDICATE_WITNESS_DELIVERED, "yes"),
    ];
    assert!(overclaim_findings(&TriangulationEngine::disagreements(&pool)).is_empty());
}

#[test]
fn gate_negative_path_flags_non_test_witness_overclaim() -> Result<(), Box<dyn std::error::Error>> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let root = std::env::temp_dir().join(format!(
        "batpak-overclaim-neg-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root)?;
    std::fs::create_dir_all(root.join("traceability"))?;
    std::fs::create_dir_all(root.join("crates/core/tests"))?;
    std::fs::write(
        root.join("traceability/invariants.yaml"),
        r#"- id: INV-NEG-PATH
  statement: witness names a plain fn
  witness_test: crates/core/tests/neg_path_witness.rs::not_a_test_fn
"#,
    )?;
    std::fs::write(
        root.join("crates/core/tests/neg_path_witness.rs"),
        "pub fn not_a_test_fn() {}\n",
    )?;

    let err = match check_overclaim_oracles(&root) {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: planted non-test witness must be rejected",
            )
            .into())
        }
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("over-claim"),
        "wrong error: {err:#}"
    );
    assert!(
        err.to_string().contains("INV-NEG-PATH"),
        "must name the invariant: {err:#}"
    );
    Ok(())
}

#[test]
fn aspirational_fn_scan_finds_evidence_suffix_on_live_tree() -> Result<(), Box<dyn std::error::Error>> {
    let root = crate::repo_surface::repo_root()?;
    let mut cache = SourceCache::new(&root);
    let subjects = aspirational_pub_fn_subjects(&root, &mut cache)?;
    assert!(
        subjects.iter().any(|s| s.contains("fork_with_evidence")),
        "expected fork_with_evidence in aspirational set, got {subjects:?}"
    );
    Ok(())
}

#[test]
fn name_behavior_overclaim_flags_unwitnessed_aspirational_fn() -> Result<(), Box<dyn std::error::Error>> {
    let pool = vec![
        claim(
            "claim-oracle",
            "crates/core/src/example.rs::mystery_evidence",
            PREDICATE_ASSERTION_TEST,
            "yes",
        ),
        claim(
            "reality-oracle",
            "crates/core/src/example.rs::mystery_evidence",
            PREDICATE_ASSERTION_TEST,
            "no",
        ),
    ];
    let findings = overclaim_findings(&TriangulationEngine::disagreements(&pool));
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].predicate, PREDICATE_ASSERTION_TEST);
    Ok(())
}
