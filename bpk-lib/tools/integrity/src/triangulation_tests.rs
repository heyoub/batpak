//! Unit tests for the triangulation harness: the disagreement engine over
//! synthetic claims, Tarjan acyclicity (incl. RED fixtures that plant a cycle),
//! the manifest path-dependency scanner, and the live-tree gate.
//!
//! justifies: INV-TEST-PANIC-AS-ASSERTION; these are integrity-tool unit tests where setup panics signal fixture breakage, see tools/integrity/src/triangulation.rs

use super::{
    check_direction_over, check_member_set_over, load_dependency_direction, scan_crate_imports,
    scan_path_dependencies, textual_member_set, Claim, CrateGraph, DependencyDirection,
    GitManifestView, TrackedManifest, TriangulationEngine,
};

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
    a.add_edge("netbat", "syncbat");
    let mut b = CrateGraph::default();
    b.add_edge("netbat", "syncbat");
    b.add_edge("netbat", "core");
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

/// The source-usage oracle's import scanner picks up `use`/`extern crate` heads
/// at a token boundary and ignores comments. Deliberately independent of syn.
#[test]
fn source_usage_scanner_collects_use_and_extern_crate_heads() {
    let src = r#"
// use commented_out::Thing;  -- a comment, must be ignored
use batpak::store::Store;
use batpak_macros::EventPayload;
extern crate syncbat;
use std::collections::BTreeMap;
"#;
    let imports = scan_crate_imports(src);
    assert!(imports.contains("batpak"), "got {imports:?}");
    assert!(imports.contains("batpak_macros"), "got {imports:?}");
    assert!(imports.contains("syncbat"), "got {imports:?}");
    assert!(imports.contains("std"), "got {imports:?}");
    assert!(
        !imports.contains("commented_out"),
        "commented import must be ignored; got {imports:?}"
    );
}

fn direction_model(yaml: &str) -> DependencyDirection {
    yaml_serde::from_str(yaml).expect("parse synthetic dependency_direction model")
}

const SYNTHETIC_MODEL: &str = r#"
layers:
  - tier: support
    crates: [support]
  - tier: core
    crates: [core]
  - tier: consumer
    crates: [consumer]
"#;

/// GREEN: a graph whose every edge goes strictly downward passes the direction
/// gate (consumer -> core -> support).
#[test]
fn dependency_direction_accepts_downward_edges() {
    let model = direction_model(SYNTHETIC_MODEL);
    let mut g = CrateGraph::default();
    g.add_edge("consumer", "core");
    g.add_edge("core", "support");
    check_direction_over(&g, &model).expect("strictly-downward graph must pass the direction gate");
}

/// RED FIXTURE: an UPWARD edge (support -> consumer) is a layering inversion the
/// plain acyclicity check would permit (the graph is still a DAG). The direction
/// gate must reject it, naming the offending edge.
#[test]
fn dependency_direction_rejects_upward_edge() {
    let model = direction_model(SYNTHETIC_MODEL);
    let mut g = CrateGraph::default();
    g.add_edge("support", "consumer"); // foundational crate reaching UP — illegal.
    let err = check_direction_over(&g, &model)
        .expect_err("upward edge must fail INV-DEPENDENCY-DIRECTION");
    let msg = format!("{err:#}");
    assert!(msg.contains("INV-DEPENDENCY-DIRECTION"), "msg: {msg}");
    assert!(
        msg.contains("support"),
        "msg must name the offending edge: {msg}"
    );
}

/// RED FIXTURE: a SAME-TIER edge is also forbidden (a layer must be internally
/// independent). `rank(from) <= rank(to)` catches the equal case.
#[test]
fn dependency_direction_rejects_same_tier_edge() {
    let model = direction_model(
        r#"
layers:
  - tier: peers
    crates: [a, b]
  - tier: lower
    crates: [c]
"#,
    );
    let mut g = CrateGraph::default();
    g.add_edge("a", "b"); // same tier — illegal.
    let err = check_direction_over(&g, &model).expect_err("same-tier edge must fail");
    assert!(format!("{err:#}").contains("INV-DEPENDENCY-DIRECTION"));
}

/// RED FIXTURE (lockstep): a workspace crate with no layer assignment fails — a
/// NEW crate cannot silently escape the direction rule.
#[test]
fn dependency_direction_rejects_unranked_crate() {
    let model = direction_model(SYNTHETIC_MODEL);
    let mut g = CrateGraph::default();
    g.add_edge("rogue", "core"); // `rogue` is in no layer.
    let err = check_direction_over(&g, &model).expect_err("unranked crate must fail the lockstep");
    let msg = format!("{err:#}");
    assert!(msg.contains("absent from"), "msg: {msg}");
    assert!(msg.contains("rogue"), "msg: {msg}");
}

/// RED FIXTURE: a crate listed in two layers fails `ranks()` (ambiguous rank).
#[test]
fn dependency_direction_rejects_double_listed_crate() {
    let model = direction_model(
        r#"
layers:
  - tier: lower
    crates: [dup]
  - tier: upper
    crates: [dup]
"#,
    );
    let g = CrateGraph::default();
    let err = check_direction_over(&g, &model).expect_err("a crate in two layers must fail");
    assert!(format!("{err:#}").contains("more than"), "msg: {err:#}");
}

/// The committed `dependency_direction.yaml` ranks every live workspace member
/// (lockstep) and the live crate graph respects the declared direction.
#[test]
fn live_dependency_direction_is_clean() {
    let repo_root = crate::repo_surface::repo_root().expect("repo root");
    let model = load_dependency_direction(&repo_root).expect("load dependency_direction.yaml");
    let graph = super::CargoMetadataGraphOracle::graph(&repo_root).expect("cargo-metadata graph");
    check_direction_over(&graph, &model)
        .expect("committed dependency_direction.yaml must pass on the live tree");
}

// --- FACT D7-C: workspace member-set (cargo-metadata vs git+manifest text). ---

fn manifest(dir: &str, name: &str) -> TrackedManifest {
    TrackedManifest {
        dir: dir.to_owned(),
        name: Some(name.to_owned()),
    }
}

/// The git+text oracle admits exactly the manifests whose directory the textual
/// `members` globs admit and `exclude` does not — exercising an explicit member,
/// a `crates/*` glob, and an excluded directory.
#[test]
fn textual_member_set_honors_members_globs_and_exclude() {
    let view = GitManifestView {
        manifests: vec![
            manifest("crates/core", "batpak"),
            manifest("crates/syncbat", "syncbat"),
            manifest("tools/integrity", "batpak-integrity"),
            // present + tracked, but NOT admitted by any members entry:
            manifest("fuzz", "batpak-fuzz"),
            // a deeper dir a single-component `crates/*` glob must NOT admit:
            manifest("crates/core/build_support", "should-not-admit"),
        ],
        members: vec!["crates/*".to_owned(), "tools/integrity".to_owned()],
        exclude: vec!["fuzz".to_owned()],
    };
    assert_eq!(
        textual_member_set(&view),
        "batpak,batpak-integrity,syncbat",
        "only members-admitted, non-excluded, single-component manifests count"
    );
}

/// GREEN: when cargo's member set and the git+text derivation agree, no
/// disagreement fires.
#[test]
fn member_set_oracles_agree_no_disagreement() {
    let view = GitManifestView {
        manifests: vec![
            manifest("crates/core", "batpak"),
            manifest("crates/syncbat", "syncbat"),
        ],
        members: vec!["crates/core".to_owned(), "crates/syncbat".to_owned()],
        exclude: Vec::new(),
    };
    // cargo resolved the same two members (sorted, comma-joined).
    let findings = check_member_set_over("batpak,syncbat", &view);
    assert!(
        findings.is_empty(),
        "agreeing member sets must not disagree; got {findings:?}"
    );
}

/// RED FIXTURE: a directory the `members` glob admits and that HAS a tracked
/// `Cargo.toml` (so the git+text oracle counts it) but which cargo's resolution
/// DROPPED from the workspace (e.g. it is gitignored from cargo's view, or cargo
/// silently excluded it). The two derivations disagree — a hard finding the engine
/// surfaces with BOTH oracle names + values, never picking a winner.
#[test]
fn member_set_oracles_disagree_when_git_sees_a_member_cargo_missed() {
    let view = GitManifestView {
        manifests: vec![
            manifest("crates/core", "batpak"),
            // tracked Cargo.toml under the `crates/*` glob, admitted textually:
            manifest("crates/ghost", "ghost"),
        ],
        members: vec!["crates/*".to_owned()],
        exclude: Vec::new(),
    };
    // cargo metadata only resolved `batpak` (it never saw `ghost`).
    let findings = check_member_set_over("batpak", &view);
    assert_eq!(findings.len(), 1, "exactly one disagreeing group");
    let d = &findings[0];
    assert_eq!(d.subject, "workspace");
    assert_eq!(d.predicate, "member-set");
    assert_eq!(
        d.votes,
        vec![
            ("cargo-member-set".to_owned(), "batpak".to_owned()),
            ("git-member-set".to_owned(), "batpak,ghost".to_owned()),
        ],
        "both derivations + values must be surfaced"
    );
}

/// The live workspace's two member-set oracles AGREE: cargo's resolution and the
/// git+text derivation yield the same crate-name set. A drift (a tracked member
/// manifest cargo dropped, or vice versa) would fail this.
#[test]
fn live_member_set_oracles_agree() {
    let repo_root = crate::repo_surface::repo_root().expect("repo root");
    let cargo = super::CargoMemberSetOracle::member_set(&repo_root).expect("cargo member set");
    let view = super::GitMemberSetOracle::view(&repo_root).expect("git manifest view");
    let findings = check_member_set_over(&cargo, &view);
    assert!(
        findings.is_empty(),
        "live cargo vs git+text member sets must agree; got {findings:?}"
    );
}
