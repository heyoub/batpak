//! Tests for the `fitness_functions.yaml` lockstep: the committed surface is
//! clean on the live tree, and planted drift (a missing/extra oracle, a
//! non-catalog invariant, a missing description) is rejected.
//!
//! justifies: INV-TEST-PANIC-AS-ASSERTION; integrity-tool unit tests where setup
//! panics signal fixture breakage, see tools/integrity/src/fitness_functions.rs

use super::{check, check_against, FitnessFunctions};
use std::collections::BTreeSet;

fn model(yaml: &str) -> FitnessFunctions {
    yaml_serde::from_str(yaml).expect("parse synthetic fitness_functions model")
}

fn catalog(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| (*s).to_owned()).collect()
}

const GOOD_YAML: &str = r#"
oracles:
  - name: a
    predicate: p
    source: s
  - name: b
    predicate: p
    source: s
invariants:
  - id: INV-X
    enforced_by: e
"#;

/// GREEN: YAML oracles == code oracles, the invariant is enforced + in catalog.
#[test]
fn lockstep_passes_when_surface_matches_code() {
    let m = model(GOOD_YAML);
    check_against(
        &m,
        &["a".to_owned(), "b".to_owned()],
        &["INV-X"],
        &catalog(&["INV-X", "INV-OTHER"]),
    )
    .expect("matching surface must pass");
}

/// RED: the code roster gained an oracle the YAML does not list.
#[test]
fn lockstep_rejects_oracle_only_in_code() {
    let m = model(GOOD_YAML);
    let err = check_against(
        &m,
        &["a".to_owned(), "b".to_owned(), "c".to_owned()],
        &["INV-X"],
        &catalog(&["INV-X"]),
    )
    .expect_err("oracle drift must fail");
    assert!(format!("{err:#}").contains("oracle roster drift"));
}

/// RED: the YAML lists an oracle the code roster does not emit.
#[test]
fn lockstep_rejects_oracle_only_in_yaml() {
    let m = model(GOOD_YAML);
    let err = check_against(&m, &["a".to_owned()], &["INV-X"], &catalog(&["INV-X"]))
        .expect_err("oracle drift must fail");
    assert!(format!("{err:#}").contains("oracle roster drift"));
}

/// RED: an enforced invariant declared in YAML is absent from the catalog.
#[test]
fn lockstep_rejects_noncatalog_invariant() {
    let m = model(GOOD_YAML);
    let err = check_against(
        &m,
        &["a".to_owned(), "b".to_owned()],
        &["INV-X"],
        &catalog(&["INV-OTHER"]),
    )
    .expect_err("non-catalog invariant must fail");
    assert!(format!("{err:#}").contains("not in the"));
}

/// RED: an oracle row missing its `source` description fails the completeness check.
#[test]
fn lockstep_rejects_missing_description() {
    let m = model(
        r#"
oracles:
  - name: a
    predicate: p
    source: ""
invariants:
  - id: INV-X
    enforced_by: e
"#,
    );
    let err = check_against(&m, &["a".to_owned()], &["INV-X"], &catalog(&["INV-X"]))
        .expect_err("missing description must fail");
    assert!(format!("{err:#}").contains("missing its"));
}

/// The committed `fitness_functions.yaml` is clean against the live tree.
#[test]
fn live_fitness_functions_surface_is_clean() {
    let repo_root = crate::repo_surface::repo_root().expect("repo root");
    check(&repo_root).expect("committed fitness_functions.yaml must pass lockstep");
}
