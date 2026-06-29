use super::{
    check, contains_identifier, extract_exclusion_anchors, parse_anchor, validate_anchors,
    validate_registry, ExclusionAnchor,
};
use crate::repo_surface::{relative, tracked_repo_files};
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // tools/integrity/ -> bpk-lib/
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("bpk-lib root")
        .to_path_buf()
}

fn tracked_rel(root: &Path) -> Vec<String> {
    tracked_repo_files(root)
        .expect("tracked files")
        .iter()
        .map(|p| relative(root, p))
        .collect()
}

#[test]
fn parse_anchor_extracts_file_and_symbol_for_in_form() {
    let anchor =
        parse_anchor(r"crates/core/src/store/import\.rs:.*replace < with == in import_events")
            .expect("anchor");
    assert_eq!(anchor.file_suffix, "crates/core/src/store/import.rs");
    assert_eq!(anchor.symbol.as_deref(), Some("import_events"));
}

#[test]
fn parse_anchor_extracts_method_for_return_type_form() {
    let anchor = parse_anchor(
        r"crates/core/src/store/config/types\.rs:.*replace IndexTopology::aos -> Self with Default::default",
    )
    .expect("anchor");
    assert_eq!(anchor.file_suffix, "crates/core/src/store/config/types.rs");
    assert_eq!(anchor.symbol.as_deref(), Some("aos"));
}

#[test]
fn parse_anchor_extracts_trailing_ident_for_bare_form() {
    let anchor = parse_anchor(r"fs\.rs:2[3-6][0-9]:.*reflink_impl").expect("anchor");
    assert_eq!(anchor.file_suffix, "fs.rs");
    assert_eq!(anchor.symbol.as_deref(), Some("reflink_impl"));
}

#[test]
fn parse_anchor_rejects_non_source_anchor() {
    assert!(parse_anchor(r"not-a-regex-without-colon").is_none());
    assert!(parse_anchor(r"some.yaml:thing").is_none());
}

#[test]
fn contains_identifier_respects_word_boundaries() {
    assert!(contains_identifier("fn aos() -> Self {}", "aos"));
    assert!(contains_identifier("Self::aos()", "aos"));
    // `aos` must NOT match inside `chaos`.
    assert!(!contains_identifier("let chaos = 1;", "aos"));
    assert!(!contains_identifier("aosoa64", "aos"));
}

#[test]
fn extract_pulls_anchors_from_marked_consts() {
    let src = r#"
const SEGMENT_SCAN_MUTANT_EXCLUDE_RES: &[&str] = &[];
pub(super) const SAMPLE_MUTANT_EXCLUDE_RES: &[&str] = &[
    // a comment with r"not extracted" inside it
    r"crates/core/src/store/import\.rs:.*replace < with == in import_events",
];
pub(super) const X_EQUIVALENT_MUTANT: &str = r"crates/core/src/store/config/types\.rs:.*replace IndexTopology::aos -> Self with Default::default";
const UNRELATED: &[&str] = &[r"crates/core/src/store/other\.rs:.*replace foo"];
"#;
    let anchors = extract_exclusion_anchors(src);
    let symbols: Vec<_> = anchors.iter().filter_map(|a| a.symbol.clone()).collect();
    assert!(symbols.contains(&"import_events".to_string()));
    assert!(symbols.contains(&"aos".to_string()));
    // The `UNRELATED` const carries no watched marker, so `foo` is not pulled.
    assert!(!symbols.contains(&"foo".to_string()));
    // Comment-embedded raw strings are stripped before extraction.
    assert!(anchors.iter().all(|a| a.regex != "not extracted"));
}

#[test]
fn live_lanes_exclusions_all_anchor_real_sites() {
    // GREEN: the committed lanes.rs exclusion set must validate against the tree.
    check(&repo_root()).expect("live mutation-exclusion registry must anchor real sites");
}

#[test]
fn red_stale_path_anchor_is_rejected() {
    // The historical bug: anchored to `config.rs` for `aos`, which lives in
    // `config/types.rs`. `config.rs` exists but does not contain `aos`.
    let root = repo_root();
    let tracked = tracked_rel(&root);
    let stale = vec![ExclusionAnchor {
        regex: r"crates/core/src/store/config\.rs:.*replace IndexTopology::aos -> Self with Default::default".to_string(),
        file_suffix: "crates/core/src/store/config.rs".to_string(),
        symbol: Some("aos".to_string()),
    }];
    let result = validate_anchors(&root, &stale, &tracked);
    assert!(
        result.is_err(),
        "an exclusion anchored to a file that does not contain its mutated symbol must fail"
    );
}

#[test]
fn red_nonexistent_file_anchor_is_rejected() {
    let root = repo_root();
    let tracked = tracked_rel(&root);
    let ghost = vec![ExclusionAnchor {
        regex: r"crates/core/src/store/does_not_exist\.rs:.*replace foo".to_string(),
        file_suffix: "crates/core/src/store/does_not_exist.rs".to_string(),
        symbol: Some("foo".to_string()),
    }];
    assert!(
        validate_anchors(&root, &ghost, &tracked).is_err(),
        "an exclusion anchored to a nonexistent file must fail"
    );
}

#[test]
fn red_unregistered_lane_exclusion_is_rejected() {
    // An exclusion present in lanes.rs but absent from the witnessed REGISTRY is
    // rejected — every exclusion must be categorized + (for equivalents)
    // witnessed, so meta-gate can trust the registry instead of a human stamp.
    let root = repo_root();
    let anchors = vec![ExclusionAnchor {
        regex: r"crates/core/src/store/import\.rs:.*replace + with - in totally_unregistered_fn"
            .to_string(),
        file_suffix: "crates/core/src/store/import.rs".to_string(),
        symbol: Some("totally_unregistered_fn".to_string()),
    }];
    assert!(
        validate_registry(&root, &anchors).is_err(),
        "an exclusion with no categorized/witnessed registry entry must be rejected"
    );
}

#[test]
fn green_live_registry_lockstep_and_witnesses_resolve() {
    // The live lanes.rs exclusions lockstep with REGISTRY and every equivalent
    // entry's witness resolves to a real #[test].
    let root = repo_root();
    let lanes = std::fs::read_to_string(root.join("tools/xtask/src/commands/mutants/lanes.rs"))
        .expect("read lanes.rs");
    let anchors = extract_exclusion_anchors(&lanes);
    validate_registry(&root, &anchors).expect("live registry must lockstep and witness-resolve");
}

#[test]
fn green_correct_anchor_passes() {
    let root = repo_root();
    let tracked = tracked_rel(&root);
    let ok = vec![ExclusionAnchor {
        regex: r"crates/core/src/store/config/types\.rs:.*replace IndexTopology::aos -> Self with Default::default".to_string(),
        file_suffix: "crates/core/src/store/config/types.rs".to_string(),
        symbol: Some("aos".to_string()),
    }];
    validate_anchors(&root, &ok, &tracked)
        .expect("a correctly anchored exclusion (real file + real symbol) must pass");
}
