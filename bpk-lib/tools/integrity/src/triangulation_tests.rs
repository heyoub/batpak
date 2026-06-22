//! Unit tests for the triangulation harness: the disagreement engine over
//! synthetic claims, Tarjan acyclicity (incl. RED fixtures that plant a cycle),
//! the manifest path-dependency scanner, and the live-tree gate.
//!
//! justifies: INV-TEST-PANIC-AS-ASSERTION; these are integrity-tool unit tests where setup panics signal fixture breakage, see tools/integrity/src/triangulation.rs

use super::{scan_path_dependencies, Claim, CrateGraph, TriangulationEngine};

fn claim(oracle: &str, subject: &str, predicate: &str, value: &str) -> Claim {
    Claim {
        subject: subject.to_owned(),
        predicate: predicate.to_owned(),
        value: value.to_owned(),
        oracle: oracle.to_owned(),
    }
}

#[test]
fn engine_reports_no_disagreement_when_oracles_agree() {
    let pool = vec![
        claim("a", "graph", "acyclic", "true"),
        claim("b", "graph", "acyclic", "true"),
    ];
    assert!(TriangulationEngine::disagreements(&pool).is_empty());
}

#[test]
fn engine_flags_disagreement_with_both_oracle_names_and_values() {
    // RED fixture: two oracles derive different acyclicity verdicts for the same
    // subject/predicate. The engine must surface BOTH names + values and never
    // silently pick one — the "no single source of truth" property.
    let pool = vec![
        claim("syn", "store/open.rs", "fs-contact-count", "3"),
        claim("allowlist", "store/open.rs", "fs-contact-count", "2"),
    ];
    let findings = TriangulationEngine::disagreements(&pool);
    assert_eq!(findings.len(), 1, "exactly one disagreeing group");
    let d = &findings[0];
    assert_eq!(d.subject, "store/open.rs");
    assert_eq!(d.predicate, "fs-contact-count");
    assert_eq!(
        d.votes,
        vec![
            ("allowlist".to_owned(), "2".to_owned()),
            ("syn".to_owned(), "3".to_owned()),
        ]
    );
    let rendered = d.render();
    assert!(rendered.contains("allowlist=2"), "render: {rendered}");
    assert!(rendered.contains("syn=3"), "render: {rendered}");
}

#[test]
fn engine_ignores_single_oracle_groups() {
    // One oracle making a claim no one else covers is not a disagreement.
    let pool = vec![claim("only", "graph", "acyclic", "true")];
    assert!(TriangulationEngine::disagreements(&pool).is_empty());
}

#[test]
fn tarjan_clean_dag_has_no_cycles() {
    let mut g = CrateGraph::default();
    g.add_edge("refbat", "syncbat");
    g.add_edge("refbat", "core");
    g.add_edge("syncbat", "core");
    g.add_edge("netbat", "syncbat");
    assert!(g.cycles().is_empty(), "a DAG must report zero cycles");
}

#[test]
fn tarjan_detects_two_node_cycle_red_fixture() {
    // RED fixture: inject a reverse edge so `core -> syncbat -> core` forms a
    // 2-cycle. Acyclicity must fail, naming both crates.
    let mut g = CrateGraph::default();
    g.add_edge("syncbat", "core");
    g.add_edge("core", "syncbat");
    let cycles = g.cycles();
    assert_eq!(cycles.len(), 1, "one strongly-connected cycle");
    assert_eq!(cycles[0], vec!["core".to_owned(), "syncbat".to_owned()]);
}

#[test]
fn tarjan_detects_self_edge_cycle() {
    let mut g = CrateGraph::default();
    g.add_edge("core", "core");
    let cycles = g.cycles();
    assert_eq!(cycles.len(), 1);
    assert_eq!(cycles[0], vec!["core".to_owned()]);
}

#[test]
fn tarjan_detects_three_node_cycle() {
    let mut g = CrateGraph::default();
    g.add_edge("a", "b");
    g.add_edge("b", "c");
    g.add_edge("c", "a");
    let cycles = g.cycles();
    assert_eq!(cycles.len(), 1);
    assert_eq!(
        cycles[0],
        vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]
    );
}

#[test]
fn graph_oracles_disagree_when_edge_sets_differ() {
    // Two graphs that disagree on a single edge must surface as a disagreement
    // on the edge-signature predicate — the whole-graph cross-check, not just
    // the boolean verdict.
    let mut a = CrateGraph::default();
    a.add_edge("refbat", "syncbat");
    let mut b = CrateGraph::default();
    b.add_edge("refbat", "syncbat");
    b.add_edge("refbat", "core");
    let pool = vec![
        claim(
            "cargo-metadata",
            "workspace-crate-graph",
            "edge-signature",
            &a.edge_signature(),
        ),
        claim(
            "manifest-scan",
            "workspace-crate-graph",
            "edge-signature",
            &b.edge_signature(),
        ),
    ];
    let findings = TriangulationEngine::disagreements(&pool);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].predicate, "edge-signature");
}

#[test]
fn manifest_scanner_picks_up_path_deps_only_in_normal_dependency_sections() {
    // Only normal `[dependencies]` (and target-scoped `*.dependencies`) edges
    // count toward the workspace build-graph DAG. `[dev-dependencies]` and
    // `[build-dependencies]` are excluded: Cargo permits cycles through them
    // (e.g. a `*-testkit` crate that path-depends on the crate it supports,
    // which dev-depends on the testkit), and such a cycle cannot break the
    // library build graph or INV-WORKSPACE-DAG-ACYCLIC.
    let manifest = r#"
[package]
name = "syncbat"
path = "should-not-be-read-here"

[dependencies]
batpak = { path = "../core", version = "0.8.2" }
serde = "1"
syncbat-macros = { path = "../syncbat-macros" }

[target.'cfg(unix)'.dependencies]
unix-only = { path = "../unix-only" }

[dev-dependencies]
proptest = "1"
test-helper = { path = "../test-helper" }

[build-dependencies]
build-only = { path = "../build-only" }
"#;
    let mut paths = scan_path_dependencies(manifest);
    paths.sort();
    assert_eq!(
        paths,
        vec![
            "../core".to_owned(),
            "../syncbat-macros".to_owned(),
            "../unix-only".to_owned(),
        ]
    );
}

#[test]
fn live_workspace_crate_graph_is_acyclic_and_oracles_agree() {
    // GREEN: the real batpak workspace is a DAG and both oracles agree.
    let repo_root = crate::repo_surface::repo_root().expect("repo root");
    super::check(&repo_root).expect("live workspace triangulation gate must pass");
}
