// Shared NO-ASYNC-RUNTIME dep-graph scanner (D10 / D11 dep-graph half).
//
// AUTHORITATIVE SHARED SOURCE. This file is compiled into BOTH:
//   * `crates/core/build.rs` (via its `shared_checks` module `include!`) — the
//     EARLY FAIL-CLOSED sentinel; its `Err` stops the build.
//   * the `batpak-integrity` binary (via `tools/shared/no_runtime_scanner.rs`,
//     `#[path]`-included by `no_runtime_gate.rs`) — the authoritative gate run
//     by `structural-check` + the registered red fixtures.
// There is therefore ONE implementation of the verdict, not two divergent greps.
//
// It inspects the RESOLVED Cargo dependency GRAPH (`cargo metadata`'s `resolve`
// section — exactly what `architecture_ir.rs` reads), NOT a `Cargo.toml` string.
// That makes it catch what the old grep could not: a runtime that is RENAMED in
// `[dependencies]`, pulled OPTIONAL+feature-enabled, declared TARGET-SPECIFIC
// (`[target.'cfg(..)'.dependencies]`), WORKSPACE-INHERITED, or pulled in
// TRANSITIVELY by another production dependency. Scope is the PRODUCTION
// subgraph of the runtime crate(s): only `kind == null` (normal) dependency
// edges are walked, so `[dev-dependencies]` and `[build-dependencies]` (and the
// test/gauntlet tooling reachable only through them) are excluded.
//
// flume is NOT a runtime: it is a runtime-neutral synchronous channel library
// (the caller owns any executor), the project's sanctioned no-async escape
// hatch. It is never in `ASYNC_RUNTIME_PACKAGES`, so it is never flagged.

/// The async-EXECUTOR / async-RUNTIME package names that are forbidden anywhere
/// in a runtime crate's production dependency graph. Matched against the REAL
/// resolved package name (`packages[].name`), so a `Cargo.toml` rename cannot
/// hide them. `flume` is deliberately absent (runtime-neutral channels).
///
/// This is the set of COMMON Rust async runtimes — EXTEND ON SIGHTING. It includes
/// the standalone io_uring executors (`monoio`/`glommio`) which do NOT sit on
/// `tokio`, so a transitive-via-tokio walk alone would miss them. `actix-rt` is
/// caught transitively (it rides `tokio`). A runtime not listed here that pulls in
/// none of these would evade — add it the moment it is sighted in any graph.
pub(crate) const ASYNC_RUNTIME_PACKAGES: &[&str] = &[
    "tokio",
    "async-std",
    "smol",
    "async-executor",
    "monoio",
    "glommio",
];

/// One offending runtime package and the production-edge path that pulls it in,
/// from a workspace root down to the runtime (e.g.
/// `["batpak", "some-prod-dep", "tokio"]`). The path is the WHY for the error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeHit {
    /// The real resolved package name of the async runtime that was reached.
    pub package: String,
    /// Root → … → runtime, by package name (the production-edge path).
    pub pull_path: Vec<String>,
}

/// A resolved-graph node reduced to what the scanner needs: the package's real
/// name and the real names of its PRODUCTION (normal-edge) dependencies. Built
/// from `cargo metadata` (real graph) or hand-constructed (red fixtures).
#[derive(Debug, Clone)]
pub(crate) struct ResolvedNode {
    pub name: String,
    /// Names of production (normal `kind == null`) dependency packages.
    pub prod_deps: Vec<String>,
}

/// THE PURE VERDICT. Walk the production subgraph from `roots` (workspace runtime
/// crate names) over normal edges only and return every reachable async-runtime
/// package with the path that pulls it. Empty result == clean graph.
///
/// Deterministic: nodes are addressed by name, the runtime set is fixed, and the
/// returned hits are sorted by `(package, pull_path)`. A cycle cannot loop the
/// walk forever (visited set). This is the single shared decision both the
/// build.rs sentinel and the integrity gate call.
pub(crate) fn scan_resolved_graph(nodes: &[ResolvedNode], roots: &[&str]) -> Vec<RuntimeHit> {
    let mut by_name: std::collections::BTreeMap<&str, &ResolvedNode> = std::collections::BTreeMap::new();
    for node in nodes {
        by_name.insert(node.name.as_str(), node);
    }
    let runtimes: std::collections::BTreeSet<&str> =
        ASYNC_RUNTIME_PACKAGES.iter().copied().collect();

    let mut hits: Vec<RuntimeHit> = Vec::new();
    let mut visited: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for root in roots {
        let mut path: Vec<String> = Vec::new();
        walk(root, &by_name, &runtimes, &mut visited, &mut path, &mut hits);
    }
    hits.sort_by(|a, b| {
        a.package
            .cmp(&b.package)
            .then_with(|| a.pull_path.cmp(&b.pull_path))
    });
    hits.dedup();
    hits
}

/// Depth-first walk over production edges. Records a [`RuntimeHit`] the first
/// time a runtime package is reached (the path captured is the first discovered
/// pull path), then keeps walking the rest of the graph.
fn walk(
    name: &str,
    by_name: &std::collections::BTreeMap<&str, &ResolvedNode>,
    runtimes: &std::collections::BTreeSet<&str>,
    visited: &mut std::collections::BTreeSet<String>,
    path: &mut Vec<String>,
    hits: &mut Vec<RuntimeHit>,
) {
    path.push(name.to_string());
    if runtimes.contains(name) {
        hits.push(RuntimeHit {
            package: name.to_string(),
            pull_path: path.clone(),
        });
        // A runtime package's own sub-deps are irrelevant; do not descend.
        path.pop();
        return;
    }
    if visited.insert(name.to_string()) {
        if let Some(node) = by_name.get(name) {
            for dep in &node.prod_deps {
                walk(dep, by_name, runtimes, visited, path, hits);
            }
        }
    }
    path.pop();
}

/// Parse `cargo metadata --format-version 1` JSON into the production-edge
/// resolved-node model + the workspace runtime root names. Uses only
/// `serde_json::Value` so the SAME parser compiles in both build.rs (serde_json
/// build-dep) and the integrity binary (serde_json dep), with no `cargo_metadata`
/// typed dependency required.
///
/// `runtime_root_names` selects which workspace members are the runtime crates to
/// root the walk at (D10: all runtime crates; D11: just the store-owning crate).
/// Only members whose name is in `runtime_root_names` become roots.
pub(crate) fn model_from_metadata_json(
    json: &str,
    runtime_root_names: &[&str],
) -> Result<(Vec<ResolvedNode>, Vec<String>), String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|err| format!("parse cargo metadata JSON: {err}"))?;

    // id -> real package name (`packages[].name`). The resolve graph keys on
    // package ids; a renamed dep's id still resolves to its real crate name.
    let packages = value
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .ok_or("cargo metadata JSON has no `packages` array")?;
    let mut id_to_name: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for package in packages {
        let id = json_str(package, "id")?;
        let name = json_str(package, "name")?;
        id_to_name.insert(id, name);
    }

    let resolve = value
        .get("resolve")
        .ok_or("cargo metadata JSON has no `resolve` section (run without --no-deps)")?;
    let resolve_nodes = resolve
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .ok_or("cargo metadata `resolve.nodes` is not an array")?;

    let mut nodes: Vec<ResolvedNode> = Vec::with_capacity(resolve_nodes.len());
    for node in resolve_nodes {
        let id = json_str(node, "id")?;
        let name = id_to_name
            .get(&id)
            .cloned()
            .ok_or_else(|| format!("resolve node id `{id}` not found in packages"))?;
        nodes.push(ResolvedNode {
            name,
            prod_deps: production_dep_names(node, &id_to_name)?,
        });
    }

    let runtime_set: std::collections::BTreeSet<&str> =
        runtime_root_names.iter().copied().collect();
    let mut roots: Vec<String> = value
        .get("workspace_members")
        .and_then(serde_json::Value::as_array)
        .ok_or("cargo metadata JSON has no `workspace_members`")?
        .iter()
        .filter_map(serde_json::Value::as_str)
        .filter_map(|id| id_to_name.get(id).cloned())
        .filter(|name| runtime_set.contains(name.as_str()))
        .collect();
    roots.sort();
    roots.dedup();
    Ok((nodes, roots))
}

/// The real package names of a resolve node's PRODUCTION dependencies: each
/// `deps[]` entry whose `dep_kinds[]` contains a `kind == null` (normal) edge.
/// `dev`/`build` edges are skipped. A target-specific normal dep still has
/// `kind == null` (only its `target` differs), so it IS included — that is how a
/// `[target.'cfg(..)'.dependencies] tokio` is caught.
fn production_dep_names(
    node: &serde_json::Value,
    id_to_name: &std::collections::BTreeMap<String, String>,
) -> Result<Vec<String>, String> {
    let deps = node
        .get("deps")
        .and_then(serde_json::Value::as_array)
        .ok_or("resolve node has no `deps` array")?;
    let mut names: Vec<String> = Vec::new();
    for dep in deps {
        if !dep_has_normal_edge(dep) {
            continue;
        }
        let pkg = json_str(dep, "pkg")?;
        if let Some(name) = id_to_name.get(&pkg) {
            names.push(name.clone());
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

/// True when a resolve `deps[]` entry has at least one NORMAL (`kind == null`)
/// edge — i.e. it is a production dependency on at least one target. Older cargo
/// without `dep_kinds` is treated as a normal edge (conservative: never hides a
/// runtime from the gate).
fn dep_has_normal_edge(dep: &serde_json::Value) -> bool {
    match dep.get("dep_kinds").and_then(serde_json::Value::as_array) {
        Some(kinds) => kinds.iter().any(|kind| {
            // `kind` absent or null == normal; "dev"/"build" are excluded.
            matches!(
                kind.get("kind"),
                None | Some(serde_json::Value::Null)
            )
        }),
        None => true,
    }
}

fn json_str(value: &serde_json::Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("cargo metadata JSON entry missing string field `{key}`"))
}

/// Run `cargo metadata --format-version 1` at `manifest_dir` and return its
/// stdout JSON. Locates cargo via the `CARGO` env var (set in build scripts and
/// by `cargo run`/`cargo test`), falling back to `"cargo"` on PATH. `cargo
/// metadata` does NOT take the build lock, so calling it from build.rs is safe.
pub(crate) fn run_cargo_metadata(manifest_dir: &std::path::Path) -> Result<String, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| std::ffi::OsString::from("cargo"));
    let output = std::process::Command::new(&cargo)
        .current_dir(manifest_dir)
        .args(["metadata", "--format-version", "1"])
        .output()
        .map_err(|err| format!("spawn `cargo metadata`: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "`cargo metadata` failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|err| format!("`cargo metadata` stdout was not UTF-8: {err}"))
}

/// END-TO-END: run `cargo metadata` at `manifest_dir`, build the production
/// model rooted at `runtime_root_names`, and return the runtime hits. This is
/// the single impure entry both the build.rs sentinel and the integrity gate
/// call so they share the WHOLE pipeline, not just the verdict.
pub(crate) fn scan_workspace_for_runtimes(
    manifest_dir: &std::path::Path,
    runtime_root_names: &[&str],
) -> Result<Vec<RuntimeHit>, String> {
    let json = run_cargo_metadata(manifest_dir)?;
    let (nodes, roots) = model_from_metadata_json(&json, runtime_root_names)?;
    if roots.is_empty() {
        return Err(format!(
            "no-runtime scanner found NONE of the runtime root crate(s) {runtime_root_names:?} \
             in the workspace — refusing to pass vacuously (a typo'd root would skip the scan)"
        ));
    }
    let root_refs: Vec<&str> = roots.iter().map(String::as_str).collect();
    Ok(scan_resolved_graph(&nodes, &root_refs))
}

/// The runtime crate roots whose PRODUCTION graph D10 forbids an async runtime
/// in. `batpak` is core; the family crates depend on it and must also stay
/// runtime-free. (`batpak-integrity`/`xtask` are TOOLING, excluded.)
pub(crate) const RUNTIME_CRATE_ROOTS: &[&str] =
    &["batpak", "syncbat", "netbat", "bvisor", "hostbat"];

#[cfg(test)]
mod scanner_tests {
    use super::*;

    fn node(name: &str, deps: &[&str]) -> ResolvedNode {
        ResolvedNode {
            name: name.to_string(),
            prod_deps: deps.iter().map(|d| d.to_string()).collect(),
        }
    }

    #[test]
    fn clean_graph_with_flume_is_not_flagged() {
        // flume present as a production dep; it is runtime-neutral and must NOT
        // be flagged. This is the real-shape baseline.
        let nodes = vec![
            node("batpak", &["flume", "serde"]),
            node("flume", &["spin"]),
            node("serde", &[]),
            node("spin", &[]),
        ];
        let hits = scan_resolved_graph(&nodes, &["batpak"]);
        assert!(hits.is_empty(), "flume must never be flagged: {hits:?}");
    }

    #[test]
    fn direct_runtime_is_flagged() {
        let nodes = vec![node("batpak", &["tokio"]), node("tokio", &[])];
        let hits = scan_resolved_graph(&nodes, &["batpak"]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].package, "tokio");
        assert_eq!(hits[0].pull_path, vec!["batpak", "tokio"]);
    }

    #[test]
    fn transitive_runtime_behind_a_prod_dep_is_flagged() {
        // The OLD grep CANNOT see this: tokio is not in batpak's [dependencies].
        let nodes = vec![
            node("batpak", &["some-prod-dep"]),
            node("some-prod-dep", &["tokio"]),
            node("tokio", &[]),
        ];
        let hits = scan_resolved_graph(&nodes, &["batpak"]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].package, "tokio");
        assert_eq!(
            hits[0].pull_path,
            vec!["batpak", "some-prod-dep", "tokio"]
        );
    }

    #[test]
    fn runtime_reachable_only_through_dev_edge_is_not_flagged() {
        // The model only carries PRODUCTION edges, so a dev-only tokio never
        // appears as a prod_dep and is correctly invisible to the verdict.
        let nodes = vec![
            node("batpak", &["serde"]), // tokio NOT a prod dep
            node("serde", &[]),
            node("tokio", &[]), // present in graph (dev), but unreachable via prod
        ];
        let hits = scan_resolved_graph(&nodes, &["batpak"]);
        assert!(hits.is_empty(), "dev-only runtime must not be flagged: {hits:?}");
    }

    #[test]
    fn cycle_in_graph_terminates() {
        let nodes = vec![
            node("batpak", &["a"]),
            node("a", &["b"]),
            node("b", &["a", "tokio"]),
            node("tokio", &[]),
        ];
        let hits = scan_resolved_graph(&nodes, &["batpak"]);
        assert_eq!(hits.len(), 1, "cycle must not loop forever; {hits:?}");
        assert_eq!(hits[0].package, "tokio");
    }

    #[test]
    fn model_from_metadata_extracts_prod_edges_and_skips_dev_build() {
        // Minimal metadata JSON: batpak -> tokio is a normal edge, batpak ->
        // proptest is a dev edge (must be skipped). A rename is modeled by the
        // dep `name` differing from the real package `name` while `pkg` resolves
        // to the real package.
        let json = r#"{
          "packages": [
            {"id":"ws+batpak@0.1.0","name":"batpak"},
            {"id":"registry+x#tokio@1.0.0","name":"tokio"},
            {"id":"registry+x#proptest@1.0.0","name":"proptest"}
          ],
          "workspace_members": ["ws+batpak@0.1.0"],
          "resolve": {
            "root": null,
            "nodes": [
              {"id":"ws+batpak@0.1.0","deps":[
                {"name":"my_rt","pkg":"registry+x#tokio@1.0.0","dep_kinds":[{"kind":null,"target":null}]},
                {"name":"proptest","pkg":"registry+x#proptest@1.0.0","dep_kinds":[{"kind":"dev","target":null}]}
              ]},
              {"id":"registry+x#tokio@1.0.0","deps":[]},
              {"id":"registry+x#proptest@1.0.0","deps":[]}
            ]
          }
        }"#;
        let (nodes, roots) =
            model_from_metadata_json(json, &["batpak"]).expect("model builds");
        assert_eq!(roots, vec!["batpak".to_string()]);
        let hits = scan_resolved_graph(&nodes, &["batpak"]);
        // The RENAMED tokio (declared `my_rt`) IS flagged by its real name.
        assert_eq!(hits.len(), 1, "renamed tokio must be flagged: {hits:?}");
        assert_eq!(hits[0].package, "tokio");
        // proptest came in via a DEV edge and must be absent from prod_deps.
        let batpak = nodes.iter().find(|n| n.name == "batpak").expect("batpak node");
        assert!(
            !batpak.prod_deps.contains(&"proptest".to_string()),
            "dev edge must be excluded from the production model"
        );
    }

    #[test]
    fn target_specific_normal_edge_is_a_production_dep() {
        // A `[target.'cfg(unix)'.dependencies] tokio` has kind==null, target set.
        let json = r#"{
          "packages": [
            {"id":"ws+batpak@0.1.0","name":"batpak"},
            {"id":"registry+x#tokio@1.0.0","name":"tokio"}
          ],
          "workspace_members": ["ws+batpak@0.1.0"],
          "resolve": {"root": null, "nodes": [
            {"id":"ws+batpak@0.1.0","deps":[
              {"name":"tokio","pkg":"registry+x#tokio@1.0.0","dep_kinds":[{"kind":null,"target":"cfg(unix)"}]}
            ]},
            {"id":"registry+x#tokio@1.0.0","deps":[]}
          ]}
        }"#;
        let (nodes, _roots) =
            model_from_metadata_json(json, &["batpak"]).expect("model builds");
        let hits = scan_resolved_graph(&nodes, &["batpak"]);
        assert_eq!(hits.len(), 1, "target-specific runtime must be flagged: {hits:?}");
        assert_eq!(hits[0].package, "tokio");
    }

    #[test]
    fn unknown_runtime_root_yields_no_roots() {
        let json = r#"{
          "packages": [{"id":"ws+batpak@0.1.0","name":"batpak"}],
          "workspace_members": ["ws+batpak@0.1.0"],
          "resolve": {"root": null, "nodes": [
            {"id":"ws+batpak@0.1.0","deps":[]}
          ]}
        }"#;
        let (_nodes, roots) =
            model_from_metadata_json(json, &["does-not-exist"]).expect("model builds");
        assert!(roots.is_empty(), "a typo'd root must resolve to NO roots");
    }
}
