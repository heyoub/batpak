//! Tests for the 02_MODEL.md → exported-symbol binding gate: the committed bindings
//! are clean on the live tree, and planted drift (missing doc phrase, missing
//! symbol, bad kind) is rejected.
//!
//! justifies: INV-TEST-PANIC-AS-ASSERTION; integrity-tool unit tests where setup
//! panics signal fixture breakage, see tools/integrity/src/model_bindings.rs

use super::{check, check_bindings, ModelBindings};

fn model(yaml: &str) -> ModelBindings {
    yaml_serde::from_str(yaml).expect("parse synthetic model_bindings")
}

const GOOD: &str = r#"
bindings:
  - concept: Store
    doc_phrase: "the Store"
    symbol: "pub struct Store"
    kind: type
"#;

/// GREEN: doc_phrase in 02_MODEL.md and symbol in the seal.
#[test]
fn passes_when_phrase_and_symbol_resolve() {
    let m = model(GOOD);
    check_bindings(&m, "intro the Store outro", "pub struct Store {}")
        .expect("clean binding passes");
}

/// RED: the doc_phrase is absent from 02_MODEL.md (the concept drifted out of the doc).
#[test]
fn rejects_missing_doc_phrase() {
    let m = model(GOOD);
    let err = check_bindings(&m, "no concept here", "pub struct Store")
        .expect_err("missing doc phrase must fail");
    assert!(format!("{err:#}").contains("NOT present in 02_MODEL.md"));
}

/// RED: the symbol is absent from the seal (renamed/removed export — the refbat-style rot).
#[test]
fn rejects_missing_symbol() {
    let m = model(GOOD);
    let err = check_bindings(&m, "the Store", "pub struct SomethingElse")
        .expect_err("missing symbol must fail");
    assert!(format!("{err:#}").contains("NOT in the public-API"));
}

/// RED: an unrecognized `kind`.
#[test]
fn rejects_bad_kind() {
    let m = model(
        r#"
bindings:
  - concept: X
    doc_phrase: "the Store"
    symbol: "pub struct Store"
    kind: gizmo
"#,
    );
    let err = check_bindings(&m, "the Store", "pub struct Store").expect_err("bad kind must fail");
    assert!(format!("{err:#}").contains("unrecognized kind"));
}

/// RED: an empty binding set is vacuous.
#[test]
fn rejects_empty_bindings() {
    let m = model("bindings: []\n");
    let err = check_bindings(&m, "", "").expect_err("empty bindings must fail");
    assert!(format!("{err:#}").contains("vacuous"));
}

/// The committed model_bindings.yaml is clean: every doc_phrase is in 02_MODEL.md
/// and every symbol is in the live public-API seal.
#[test]
fn live_model_bindings_are_clean() {
    let repo_root = crate::repo_surface::repo_root().expect("repo root");
    check(&repo_root).expect("committed model_bindings.yaml must pass on the live tree");
}
