//! PROVES: INV-DOCS-CATALOG-VIEW-CURRENT, INV-INVARIANT-WITNESS-TEST
//! CATCHES: a stale INVARIANTS.md catalog block; a witness_test that names a
//!          missing file, a missing fn, or a non-`#[test]` fn.
//! SEEDED: synthetic in-memory catalog + a tempdir source tree.

use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_root(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "batpak-docs-catalog-{name}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("create temp root");
    path
}

fn inv(id: &str, statement: &str, witness: Option<&str>) -> CatalogInvariant {
    CatalogInvariant {
        id: id.to_string(),
        statement: statement.to_string(),
        witness_test: witness.map(ToString::to_string),
    }
}

#[test]
fn render_block_is_sorted_and_pipe_safe() {
    let block = render_catalog_block(&[
        inv("INV-ZED", "later one with | a pipe inside it here", None),
        inv(
            "INV-ALPHA",
            "first   alpha   statement collapses whitespace",
            None,
        ),
    ]);
    let alpha = block.find("INV-ALPHA").expect("alpha present");
    let zed = block.find("INV-ZED").expect("zed present");
    assert!(alpha < zed, "ids must be sorted");
    assert!(block.contains("\\|"), "pipe must be escaped");
    assert!(
        block.contains("first alpha statement collapses whitespace"),
        "whitespace must collapse"
    );
}

#[test]
fn splice_replaces_only_between_markers() {
    let doc = format!("prose above\n{BEGIN_MARKER}\nOLD\n{END_MARKER}\nprose below\n");
    let next = splice_catalog_block(&doc, "NEW BLOCK\n").expect("splice");
    assert!(next.contains("prose above"));
    assert!(next.contains("prose below"));
    assert!(next.contains("NEW BLOCK"));
    assert!(!next.contains("OLD"), "old block must be replaced");
    // Round-trip stability: splicing the same content twice is idempotent.
    let again = splice_catalog_block(&next, "NEW BLOCK\n").expect("splice again");
    assert_eq!(next, again, "splice must be idempotent");
}

#[test]
fn splice_fails_without_markers() {
    let err = splice_catalog_block("no markers here", "X").expect_err("missing markers must Err");
    assert!(err.to_string().contains("missing"), "got: {err}");
}

fn write_file(dir: &Path, rel: &str, body: &str) {
    let full = dir.join(rel);
    std::fs::create_dir_all(full.parent().expect("rel path has a parent"))
        .expect("create parent dirs");
    std::fs::write(&full, body).expect("write test fixture file");
}

#[test]
fn witness_gate_accepts_test_fn_and_proptest() {
    let root = temp_root("accepts");
    let root = root.as_path();
    write_file(
        root,
        "tests/ok.rs",
        "#[test]\nfn witnesses_it() {}\nproptest!{ fn prop_witnesses(x in 0u8..3) { let _ = x; } }\n",
    );
    let invs = vec![
        inv(
            "INV-A",
            "statement words for the gate to accept here",
            Some("tests/ok.rs::witnesses_it"),
        ),
        inv(
            "INV-B",
            "statement words for the gate to accept here",
            Some("tests/ok.rs::prop_witnesses"),
        ),
    ];
    let mut cache = SourceCache::new(root);
    check_witness_tests(root, &invs, &mut cache).expect("both witnesses resolve");
}

#[test]
fn witness_gate_rejects_non_test_fn() {
    let root = temp_root("rejects-nontest");
    let root = root.as_path();
    write_file(root, "tests/bad.rs", "fn not_a_test() {}\n");
    let invs = vec![inv(
        "INV-C",
        "statement words for the gate to reject here ok",
        Some("tests/bad.rs::not_a_test"),
    )];
    let mut cache = SourceCache::new(root);
    let err =
        check_witness_tests(root, &invs, &mut cache).expect_err("non-test fn must be rejected");
    assert!(err.to_string().contains("names no"), "got: {err}");
}

#[test]
fn witness_gate_rejects_missing_file() {
    let root = temp_root("rejects-missing");
    let root = root.as_path();
    let invs = vec![inv(
        "INV-D",
        "statement words for the gate to reject here ok",
        Some("tests/nope.rs::ghost"),
    )];
    let mut cache = SourceCache::new(root);
    let err =
        check_witness_tests(root, &invs, &mut cache).expect_err("missing file must be rejected");
    assert!(err.to_string().contains("missing file"), "got: {err}");
}

#[test]
fn live_catalog_block_matches_committed_invariants_md() {
    // Red-fixture wiring: this is the in-process mirror of `--check`. If a new
    // INV lands in invariants.yaml without regenerating INVARIANTS.md, this
    // fails (alongside the structural-check gate).
    let repo_root = crate::repo_surface::repo_root().expect("repo root");
    let invariants = load_catalog(&repo_root).expect("load catalog");
    let block = render_catalog_block(&invariants);
    let md_path = crate::repo_surface::project_root(&repo_root).join("INVARIANTS.md");
    let md = std::fs::read_to_string(md_path).expect("read md");
    let next = splice_catalog_block(&md, &block).expect("splice");
    assert_eq!(
        md, next,
        "INVARIANTS.md catalog block is stale; run `cargo xtask docs`"
    );
}
