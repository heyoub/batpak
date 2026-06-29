//! D10 / D11 dep-graph half — the AUTHORITATIVE no-async-runtime gate.
//!
//! The pure scanner + the cargo-metadata pipeline live in the SHARED source
//! `crates/core/build_support/no_runtime_scanner.rs`, `#[path]`-included below.
//! The IDENTICAL source is also compiled into `crates/core/build.rs` (the early
//! FAIL-CLOSED sentinel), so the build tripwire and this gate share ONE verdict
//! — there is no second grep to drift.
//!
//! The gate walks the RESOLVED Cargo dependency GRAPH (`cargo metadata`'s
//! `resolve` section), not a `Cargo.toml` string, so it catches a runtime that
//! is renamed, optional+feature-enabled, target-specific, workspace-inherited,
//! or pulled in TRANSITIVELY by another production dependency — every evasion the
//! old `[dependencies]`-string grep was blind to. `flume` is runtime-neutral
//! (caller owns the executor) and is never in the runtime set, so it is never
//! flagged.
//!
//! D10: scan the PRODUCTION graph of all runtime crates (`RUNTIME_CRATE_ROOTS`).
//! D11 dep-graph half: scan the store's production graph — the store is a module
//! of `batpak`, so its graph is `batpak`'s production graph (`STORE_ROOT`).

use anyhow::{bail, Result};
use std::path::Path;

#[path = "../../shared/no_runtime_scanner.rs"]
pub(crate) mod scanner;

/// The store's runtime-crate root for D11's dep-graph half. The store is a
/// module of `batpak` (not a separate crate), so its production dependency graph
/// IS `batpak`'s production graph.
const STORE_ROOT: &[&str] = &["batpak"];

/// D10 gate: no async runtime anywhere in the production graph of the runtime
/// crates. Runs `cargo metadata` at the workspace root and FAILS on any hit.
pub(crate) fn check(repo_root: &Path) -> Result<()> {
    check_roots(
        repo_root,
        scanner::RUNTIME_CRATE_ROOTS,
        "D10 (runtime crates)",
    )
}

/// D11 dep-graph half: no async EXECUTOR in the store's production graph.
pub(crate) fn check_store(repo_root: &Path) -> Result<()> {
    check_roots(repo_root, STORE_ROOT, "D11 (store dep-graph)")
}

fn check_roots(repo_root: &Path, roots: &[&str], label: &str) -> Result<()> {
    let hits = scanner::scan_workspace_for_runtimes(repo_root, roots)
        .map_err(|err| anyhow::anyhow!("{label}: resolved-dep-graph scan failed: {err}"))?;
    if let Some(hit) = hits.first() {
        bail!(
            "{label}: async runtime `{}` is reachable in the PRODUCTION dependency graph.\n\
             Pulled via: {}.\n\
             tokio/async-std/smol/async-executor are forbidden (including renamed/optional/\n\
             target-specific/transitive forms). flume is runtime-neutral and allowed.\n\
             See ADR-0001 / INV-STORE-SYNC-ONLY.",
            hit.package,
            hit.pull_path.join(" -> "),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::scanner::{
        model_from_metadata_json, scan_resolved_graph, ResolvedNode, ASYNC_RUNTIME_PACKAGES,
    };
    use super::*;
    use crate::repo_surface::repo_root;

    fn node(name: &str, deps: &[&str]) -> ResolvedNode {
        ResolvedNode {
            name: name.to_string(),
            prod_deps: deps.iter().map(|d| (*d).to_string()).collect(),
        }
    }

    /// flume must NEVER be in the runtime set, and a graph whose only channel
    /// dep is flume must scan clean — the sanctioned no-async escape hatch.
    #[test]
    fn flume_is_never_a_runtime_and_is_not_flagged() {
        let mut failures: Vec<String> = Vec::new();
        if ASYNC_RUNTIME_PACKAGES.contains(&"flume") {
            failures.push("flume must not be in ASYNC_RUNTIME_PACKAGES".to_string());
        }
        let nodes = vec![
            node("batpak", &["flume", "serde"]),
            node("flume", &[]),
            node("serde", &[]),
        ];
        let hits = scan_resolved_graph(&nodes, &["batpak"]);
        if !hits.is_empty() {
            failures.push(format!("flume-only graph was flagged: {hits:?}"));
        }
        assert!(failures.is_empty(), "{failures:?}");
    }

    /// THE D10 RED FIXTURE (GateNegativePath). A synthetic resolved graph with a
    /// planted RENAMED tokio, a TRANSITIVE tokio, and a TARGET-SPECIFIC tokio —
    /// each an evasion the old `[dependencies]` grep could not see — must make
    /// the verdict return a hit (i.e. the gate Errs). Collect-and-assert: no
    /// `panic!` even in tests.
    #[test]
    fn planted_runtime_dep_is_rejected() {
        let mut failures: Vec<String> = Vec::new();

        // (a) RENAMED tokio: declared as `my_rt` in [dependencies], resolves to
        // the real package `tokio`. The grep on the manifest string `my_rt`
        // would miss it; the graph sees the real name.
        let renamed = r#"{
          "packages": [
            {"id":"ws+batpak@0.1.0","name":"batpak"},
            {"id":"registry+x#tokio@1.0.0","name":"tokio"}
          ],
          "workspace_members": ["ws+batpak@0.1.0"],
          "resolve": {"root": null, "nodes": [
            {"id":"ws+batpak@0.1.0","deps":[
              {"name":"my_rt","pkg":"registry+x#tokio@1.0.0","dep_kinds":[{"kind":null,"target":null}]}
            ]},
            {"id":"registry+x#tokio@1.0.0","deps":[]}
          ]}
        }"#;
        assert_planted_rejected(renamed, "renamed", &mut failures);

        // (b) TRANSITIVE tokio: batpak -> some-prod-dep -> tokio. Not in batpak's
        // own [dependencies] at all.
        let transitive = r#"{
          "packages": [
            {"id":"ws+batpak@0.1.0","name":"batpak"},
            {"id":"registry+x#dep@1.0.0","name":"some-prod-dep"},
            {"id":"registry+x#tokio@1.0.0","name":"tokio"}
          ],
          "workspace_members": ["ws+batpak@0.1.0"],
          "resolve": {"root": null, "nodes": [
            {"id":"ws+batpak@0.1.0","deps":[
              {"name":"some-prod-dep","pkg":"registry+x#dep@1.0.0","dep_kinds":[{"kind":null,"target":null}]}
            ]},
            {"id":"registry+x#dep@1.0.0","deps":[
              {"name":"tokio","pkg":"registry+x#tokio@1.0.0","dep_kinds":[{"kind":null,"target":null}]}
            ]},
            {"id":"registry+x#tokio@1.0.0","deps":[]}
          ]}
        }"#;
        assert_planted_rejected(transitive, "transitive", &mut failures);

        // (c) TARGET-SPECIFIC async runtime: kind==null, target set.
        let target_specific = r#"{
          "packages": [
            {"id":"ws+batpak@0.1.0","name":"batpak"},
            {"id":"registry+x#smol@2.0.0","name":"smol"}
          ],
          "workspace_members": ["ws+batpak@0.1.0"],
          "resolve": {"root": null, "nodes": [
            {"id":"ws+batpak@0.1.0","deps":[
              {"name":"smol","pkg":"registry+x#smol@2.0.0","dep_kinds":[{"kind":null,"target":"cfg(windows)"}]}
            ]},
            {"id":"registry+x#smol@2.0.0","deps":[]}
          ]}
        }"#;
        assert_planted_rejected(target_specific, "target-specific", &mut failures);

        assert!(failures.is_empty(), "{failures:?}");
    }

    fn assert_planted_rejected(json: &str, label: &str, failures: &mut Vec<String>) {
        match model_from_metadata_json(json, &["batpak"]) {
            Ok((nodes, _roots)) => {
                let hits = scan_resolved_graph(&nodes, &["batpak"]);
                if hits.is_empty() {
                    failures.push(format!("planted {label} runtime was NOT flagged"));
                }
            }
            Err(err) => failures.push(format!("planted {label} fixture failed to model: {err}")),
        }
    }

    /// REGRESSION GUARD: the new scanner still catches what the OLD grep caught —
    /// a literal `tokio` in `[dependencies]` (modeled as a direct normal edge).
    #[test]
    fn literal_tokio_in_dependencies_is_still_caught() {
        let nodes = vec![node("batpak", &["tokio"]), node("tokio", &[])];
        let hits = scan_resolved_graph(&nodes, &["batpak"]);
        assert_eq!(
            hits.len(),
            1,
            "literal tokio must still be caught: {hits:?}"
        );
        assert_eq!(hits[0].package, "tokio");
    }

    /// BUILD-FAIL-FAST proof (structural): build.rs's sentinel must propagate the
    /// scanner verdict as an `Err` that TERMINATES the build, and must NOT
    /// downgrade a hit to a `cargo:warning`. `fail(..)` emits a warning as a side
    /// effect but RETURNS the error string; the sentinel must `return Err(fail(..))`
    /// (the Err is what stops cargo), and any skipped/failed-metadata path must
    /// also fail closed (map_err -> `?`), never pass silently.
    #[test]
    fn build_rs_sentinel_returns_err_not_warning() {
        let repo = repo();
        let build_rs =
            std::fs::read_to_string(repo.join("crates/core/build.rs")).expect("read build.rs");
        let mut failures: Vec<String> = Vec::new();

        // The sentinel calls the SHARED scanner (not a private grep).
        if !build_rs.contains("scan_workspace_for_runtimes") {
            failures
                .push("build.rs sentinel must call the shared scan_workspace_for_runtimes".into());
        }
        // A hit returns Err (build-terminating), not a bare warning.
        if !build_rs.contains("return Err(fail(&format!(") {
            failures.push("build.rs must `return Err(fail(..))` on a runtime hit".into());
        }
        // The fn that holds the sentinel is `?`-propagated from main (fail-fast).
        if !build_rs.contains("check_no_tokio_in_deps()?;") {
            failures.push("build.rs main must `?`-propagate check_no_tokio_in_deps".into());
        }
        // A failed metadata scan fails CLOSED (map_err on the scan Result), not a
        // silent pass.
        if !build_rs.contains("could not be proven") {
            failures.push("build.rs must fail closed when the scan itself fails".into());
        }
        assert!(failures.is_empty(), "{failures:?}");
    }

    /// The REAL graph is clean today (no runtime) and the gate is GREEN. This is
    /// the live-tree green-path proof the scan does not false-positive on flume.
    #[test]
    fn real_graph_is_clean_today() {
        check(&repo()).expect("D10: the real production graph must be runtime-free");
        check_store(&repo()).expect("D11 dep-graph: the store graph must be runtime-free");
    }

    fn repo() -> std::path::PathBuf {
        repo_root().expect("repo root resolves from tools/integrity")
    }
}
