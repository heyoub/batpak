//! Triangulation harness (`GAUNTLET-TRIANGULATION`) — N-version differential
//! testing over NON-TYPE repo facts. No oracle is privileged: each oracle emits
//! a normalized claim set and the engine flags any `(subject, predicate)` group
//! where two oracles DISAGREE on the value. A disagreement is a hard finding —
//! the engine never picks a winner; a human/agent must reconcile the code or the
//! oracles. rustc is the type source-of-truth and is deliberately NOT re-derived
//! here (see spec item 3.0); triangulation targets architecture/doctrine facts.
//!
//! The `Oracle` trait + disagreement engine cross-check workspace crate-graph
//! acyclicity via THREE independent oracles:
//!   1. `CargoMetadataGraphOracle` — edges from `cargo metadata` (`no_deps`,
//!      reading each member's declared path dependencies) + Tarjan SCC.
//!   2. `ManifestScanGraphOracle` — edges re-derived by directly scanning each
//!      member `Cargo.toml` for `path = "../<dir>"` entries + Tarjan SCC.
//!   3. `SourceUsageGraphOracle` — edges re-derived from ACTUAL SOURCE USAGE
//!      (`use <crate>::` / `extern crate`), a genuinely different evidence source
//!      than the manifests. It emits ONLY the `acyclic` predicate (not the full
//!      edge-signature): the usage edge set is a legal SUBSET of the manifest set,
//!      but acyclicity is monotone under edge removal, so a usage-graph cycle the
//!      manifests miss is a true finding. (See the oracle's doc for the proof.)
//!
//! If the oracles disagree on the acyclicity verdict (or oracles 1/2 on the edge
//! set), that is itself a finding; if they AGREE that a cycle exists, the gate
//! fails naming the cycle.
//!
//! Beyond acyclicity the gate enforces the STRONGER `INV-DEPENDENCY-DIRECTION`
//! allowed-edge rule: every NORMAL build-graph edge must go strictly DOWNWARD in
//! the layer order declared in `traceability/dependency_direction.yaml`. A legal
//! DAG can still carry an edge in the WRONG direction (a foundational crate
//! reaching UP into a consumer); the direction gate forbids it, and lockstep
//! requires every workspace member to be assigned a layer so a NEW crate cannot
//! escape the rule. Both clean on the live tree ⟹ green.
//!
//! The declarative rule surface — which oracles run and which directional
//! invariant is enforced — is mirrored in `traceability/fitness_functions.yaml`
//! (the dual-ergonomic registry), kept in lockstep by `fitness_functions.rs`.
//!
//! Anchors: INV-WORKSPACE-DAG-ACYCLIC, INV-DEPENDENCY-DIRECTION
//! (traceability/invariants.yaml), architecture_ir.rs (cargo-metadata projection
//! it complements).

use crate::repo_surface::{ensure, tracked_repo_files};
use anyhow::{anyhow, Context, Result};
use cargo_metadata::MetadataCommand;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[cfg(test)]
#[path = "triangulation_tests.rs"]
mod triangulation_tests;

/// A normalized fact emitted by an oracle: oracle `name` asserts that `subject`
/// has `predicate` equal to `value`. The engine groups by `(subject, predicate)`
/// and flags any group whose `value`s are not unanimous across oracles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Claim {
    pub(crate) subject: String,
    pub(crate) predicate: String,
    pub(crate) value: String,
    pub(crate) oracle: String,
}

/// The claims one oracle emits over the repo for a single run.
#[derive(Debug, Default, Clone)]
pub(crate) struct ClaimSet {
    claims: Vec<Claim>,
}

impl ClaimSet {
    pub(crate) fn new() -> Self {
        Self { claims: Vec::new() }
    }

    pub(crate) fn assert(
        &mut self,
        oracle: &str,
        subject: &str,
        predicate: &str,
        value: impl Into<String>,
    ) {
        self.claims.push(Claim {
            subject: subject.to_owned(),
            predicate: predicate.to_owned(),
            value: value.into(),
            oracle: oracle.to_owned(),
        });
    }

    pub(crate) fn claims(&self) -> &[Claim] {
        &self.claims
    }
}

/// An oracle is one independent way of deriving facts about the repo. No oracle
/// is the source of truth; the engine cross-checks them. `name` is quoted in
/// disagreement findings so the human knows WHICH two derivations diverged.
pub(crate) trait Oracle {
    fn name(&self) -> &str;
    fn claims(&self, repo_root: &Path) -> Result<ClaimSet>;
}

/// A `(subject, predicate)` group where oracles did not agree on the value.
/// Carrying both oracle names + their values is the whole point: the finding
/// forces reconciling the two derivations, not silently trusting one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Disagreement {
    pub(crate) subject: String,
    pub(crate) predicate: String,
    /// `(oracle_name, value)` pairs, sorted, that disagreed.
    pub(crate) votes: Vec<(String, String)>,
}

impl Disagreement {
    pub(crate) fn render(&self) -> String {
        let votes = self
            .votes
            .iter()
            .map(|(oracle, value)| format!("{oracle}={value}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "DISAGREEMENT(subject={}, predicate={}): {}",
            self.subject, self.predicate, votes
        )
    }
}

/// The disagreement engine: run every oracle, group all claims by
/// `(subject, predicate)`, and report any group with more than one distinct
/// value. The engine does NOT pick a winner — see the module doc.
pub(crate) struct TriangulationEngine {
    oracles: Vec<Box<dyn Oracle>>,
}

impl TriangulationEngine {
    pub(crate) fn new(oracles: Vec<Box<dyn Oracle>>) -> Self {
        Self { oracles }
    }

    /// Collect every oracle's claims into one pool. Surfaces which oracles ran.
    pub(crate) fn collect(&self, repo_root: &Path) -> Result<Vec<Claim>> {
        let mut pool = Vec::new();
        for oracle in &self.oracles {
            let set = oracle
                .claims(repo_root)
                .with_context(|| format!("oracle `{}` claims", oracle.name()))?;
            pool.extend(set.claims().iter().cloned());
        }
        Ok(pool)
    }

    /// Pure disagreement detection over a claim pool. Split out so it can be
    /// unit-tested with synthetic claims (no live repo needed).
    pub(crate) fn disagreements(pool: &[Claim]) -> Vec<Disagreement> {
        // group: (subject, predicate) -> value -> set of oracle names
        let mut groups: BTreeMap<(String, String), BTreeMap<String, BTreeSet<String>>> =
            BTreeMap::new();
        for claim in pool {
            groups
                .entry((claim.subject.clone(), claim.predicate.clone()))
                .or_default()
                .entry(claim.value.clone())
                .or_default()
                .insert(claim.oracle.clone());
        }
        let mut out = Vec::new();
        for ((subject, predicate), by_value) in groups {
            if by_value.len() <= 1 {
                continue; // unanimous (or single oracle) — agreement.
            }
            let mut votes: Vec<(String, String)> = Vec::new();
            for (value, oracles) in &by_value {
                for oracle in oracles {
                    votes.push((oracle.clone(), value.clone()));
                }
            }
            votes.sort();
            out.push(Disagreement {
                subject,
                predicate,
                votes,
            });
        }
        out
    }
}

/// A directed graph over workspace crate names with a Tarjan SCC acyclicity
/// check. Shared by both crate-graph oracles so they differ ONLY in how the
/// edges are sourced, not in how acyclicity is decided.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct CrateGraph {
    /// node -> sorted set of intra-workspace dependency targets.
    edges: BTreeMap<String, BTreeSet<String>>,
}

impl CrateGraph {
    fn add_node(&mut self, name: &str) {
        self.edges.entry(name.to_owned()).or_default();
    }

    fn add_edge(&mut self, from: &str, to: &str) {
        self.add_node(from);
        self.add_node(to);
        self.edges
            .get_mut(from)
            .expect("node inserted above")
            .insert(to.to_owned());
    }

    /// Canonical, order-stable serialization of the edge set — used as a claim
    /// value so two oracles can be cross-checked on the WHOLE graph, not just
    /// the boolean verdict.
    fn edge_signature(&self) -> String {
        let mut lines = Vec::new();
        for (from, tos) in &self.edges {
            for to in tos {
                lines.push(format!("{from}->{to}"));
            }
        }
        lines.sort();
        lines.join(";")
    }

    /// Tarjan strongly-connected-components. Any component of size > 1 is a
    /// dependency cycle; a self-edge is also a (degenerate) cycle. Returns the
    /// sorted cyclic node sets (empty ⟹ acyclic).
    fn cycles(&self) -> Vec<Vec<String>> {
        let nodes: Vec<&String> = self.edges.keys().collect();
        let index_of: BTreeMap<&String, usize> =
            nodes.iter().enumerate().map(|(i, n)| (*n, i)).collect();
        let mut state = TarjanState::new(nodes.len());
        for &start in index_of.values() {
            if state.index[start].is_none() {
                self.strongconnect(start, &nodes, &index_of, &mut state);
            }
        }
        let mut cyclic: Vec<Vec<String>> = Vec::new();
        for comp in state.components {
            let is_cycle = comp.len() > 1 || {
                // size-1 component is cyclic only via a self-edge.
                let only = nodes[comp[0]];
                self.edges
                    .get(only)
                    .is_some_and(|tos| tos.contains(only.as_str()))
            };
            if is_cycle {
                let mut named: Vec<String> = comp.iter().map(|&i| nodes[i].clone()).collect();
                named.sort();
                cyclic.push(named);
            }
        }
        cyclic.sort();
        cyclic
    }

    fn strongconnect(
        &self,
        v: usize,
        nodes: &[&String],
        index_of: &BTreeMap<&String, usize>,
        state: &mut TarjanState,
    ) {
        state.index[v] = Some(state.counter);
        state.lowlink[v] = state.counter;
        state.counter += 1;
        state.stack.push(v);
        state.on_stack[v] = true;

        if let Some(tos) = self.edges.get(nodes[v]) {
            for to in tos {
                let Some(&w) = index_of.get(to) else {
                    continue; // edge to a non-workspace target; ignore.
                };
                if state.index[w].is_none() {
                    self.strongconnect(w, nodes, index_of, state);
                    state.lowlink[v] = state.lowlink[v].min(state.lowlink[w]);
                } else if state.on_stack[w] {
                    state.lowlink[v] = state.lowlink[v].min(state.index[w].unwrap_or(usize::MAX));
                }
            }
        }

        if state.index[v] == Some(state.lowlink[v]) {
            let mut component = Vec::new();
            while let Some(w) = state.stack.pop() {
                state.on_stack[w] = false;
                component.push(w);
                if w == v {
                    break;
                }
            }
            state.components.push(component);
        }
    }
}

struct TarjanState {
    index: Vec<Option<usize>>,
    lowlink: Vec<usize>,
    on_stack: Vec<bool>,
    stack: Vec<usize>,
    counter: usize,
    components: Vec<Vec<usize>>,
}

impl TarjanState {
    fn new(n: usize) -> Self {
        Self {
            index: vec![None; n],
            lowlink: vec![0; n],
            on_stack: vec![false; n],
            stack: Vec::new(),
            counter: 0,
            components: Vec::new(),
        }
    }
}

/// Oracle 1: workspace crate graph from `cargo metadata` (`no_deps`), reading
/// each member package's declared `dependencies` and keeping only those with a
/// `path` (intra-workspace path deps).
struct CargoMetadataGraphOracle;

impl CargoMetadataGraphOracle {
    fn graph(repo_root: &Path) -> Result<CrateGraph> {
        let mut cmd = MetadataCommand::new();
        cmd.current_dir(repo_root);
        cmd.no_deps();
        let metadata = cmd.exec().context("cargo metadata")?;
        let members: BTreeSet<_> = metadata.workspace_members.iter().collect();
        let member_names: BTreeSet<String> = metadata
            .packages
            .iter()
            .filter(|p| members.contains(&p.id))
            .map(|p| p.name.to_string())
            .collect();
        let mut graph = CrateGraph::default();
        for package in metadata.packages.iter().filter(|p| members.contains(&p.id)) {
            graph.add_node(&package.name);
            for dep in &package.dependencies {
                if dep.path.is_none() {
                    continue; // external (registry) dep — not a workspace edge.
                }
                // Only normal (build-graph) edges constrain the workspace DAG.
                // `dev-`/`build-dependencies` do not participate in the library
                // build graph, and Cargo intentionally permits cycles through
                // them (e.g. a `*-testkit` crate that path-depends on the crate
                // it supports, which in turn dev-depends on the testkit). Such a
                // cycle cannot break layering or incremental library builds, so
                // it must not trip INV-WORKSPACE-DAG-ACYCLIC.
                if dep.kind != cargo_metadata::DependencyKind::Normal {
                    continue;
                }
                if member_names.contains(dep.name.as_str()) {
                    graph.add_edge(&package.name, &dep.name);
                }
            }
        }
        Ok(graph)
    }
}

impl Oracle for CargoMetadataGraphOracle {
    fn name(&self) -> &str {
        "cargo-metadata"
    }

    fn claims(&self, repo_root: &Path) -> Result<ClaimSet> {
        let graph = Self::graph(repo_root)?;
        Ok(graph_claims(self.name(), &graph))
    }
}

/// Oracle 2: the SAME workspace crate graph re-derived independently by scanning
/// each member's `Cargo.toml` `[dependencies]` block for `path = "..."` entries
/// and mapping the directory back to its package name via cargo metadata's
/// member list. Independent of oracle 1's dependency-resolution path.
struct ManifestScanGraphOracle;

impl ManifestScanGraphOracle {
    fn graph(repo_root: &Path) -> Result<CrateGraph> {
        // Map directory (relative to workspace root) -> package name, from the
        // member manifests. cargo metadata gives us the member manifest paths;
        // we re-read the [package] name + [dependencies] paths ourselves.
        let mut cmd = MetadataCommand::new();
        cmd.current_dir(repo_root);
        cmd.no_deps();
        let metadata = cmd.exec().context("cargo metadata")?;
        let members: BTreeSet<_> = metadata.workspace_members.iter().collect();

        let mut dir_to_name: BTreeMap<String, String> = BTreeMap::new();
        let mut member_manifests: Vec<(String, std::path::PathBuf)> = Vec::new();
        for package in metadata.packages.iter().filter(|p| members.contains(&p.id)) {
            let manifest = package.manifest_path.as_std_path().to_path_buf();
            let dir = manifest
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| manifest.clone());
            let canon = std::fs::canonicalize(&dir).unwrap_or(dir);
            dir_to_name.insert(
                canon.to_string_lossy().to_string(),
                package.name.to_string(),
            );
            member_manifests.push((package.name.to_string(), manifest));
        }

        let mut graph = CrateGraph::default();
        for (name, manifest) in &member_manifests {
            graph.add_node(name);
            let manifest_dir = manifest.parent().unwrap_or(manifest);
            let text = std::fs::read_to_string(manifest)
                .with_context(|| format!("read {}", manifest.display()))?;
            for dep_rel in scan_path_dependencies(&text) {
                let target_dir = manifest_dir.join(&dep_rel);
                let canon = std::fs::canonicalize(&target_dir).unwrap_or(target_dir);
                if let Some(target_name) = dir_to_name.get(&canon.to_string_lossy().to_string()) {
                    graph.add_edge(name, target_name);
                }
            }
        }
        Ok(graph)
    }
}

impl Oracle for ManifestScanGraphOracle {
    fn name(&self) -> &str {
        "manifest-scan"
    }

    fn claims(&self, repo_root: &Path) -> Result<ClaimSet> {
        let graph = Self::graph(repo_root)?;
        Ok(graph_claims(self.name(), &graph))
    }
}

/// Oracle 3: the crate graph re-derived from ACTUAL SOURCE USAGE, not manifests.
/// For each workspace member it scans every `.rs` file under the crate's `src/`
/// for references to a SIBLING workspace crate's import root (`use <crate>::`,
/// `extern crate <crate>`, or a `<crate>::` path), mapping the Rust import name
/// (hyphens → underscores) back to the member package name. This is a genuinely
/// INDEPENDENT derivation: it reads the code's real dependency on a crate's API,
/// not the manifest's declaration.
///
/// CRITICAL honesty note — why this oracle emits ONLY the `acyclic` predicate and
/// NOT `edge-signature`: a manifest may legitimately declare a dependency the
/// source never `use`s (re-exports, macro-only deps, feature-gated paths), so the
/// source-usage edge set is a SUBSET of the manifest edge set, not equal. Asserting
/// edge-signature would therefore false-fire on a legal subset. But acyclicity is
/// monotone under edge removal: a subgraph of a DAG is a DAG, so if the manifest
/// graph is acyclic the usage graph MUST be too. A disagreement on `acyclic`
/// (manifest=acyclic, usage=cyclic) is then a genuine finding — a real dependency
/// cycle expressed in code that the manifest's path-dep accounting missed. That is
/// the only cross-checkable claim, and it is non-tautological.
struct SourceUsageGraphOracle;

impl SourceUsageGraphOracle {
    fn graph(repo_root: &Path) -> Result<CrateGraph> {
        let mut cmd = MetadataCommand::new();
        cmd.current_dir(repo_root);
        cmd.no_deps();
        let metadata = cmd.exec().context("cargo metadata")?;
        let members: BTreeSet<_> = metadata.workspace_members.iter().collect();

        // Map each member's Rust import name (package name with '-' → '_') to its
        // package name, and record each member's src/ directory.
        let mut import_to_name: BTreeMap<String, String> = BTreeMap::new();
        let mut member_src_dirs: Vec<(String, std::path::PathBuf)> = Vec::new();
        for package in metadata.packages.iter().filter(|p| members.contains(&p.id)) {
            let import = package.name.replace('-', "_");
            import_to_name.insert(import, package.name.to_string());
            let manifest = package.manifest_path.as_std_path().to_path_buf();
            let src = manifest
                .parent()
                .map(|d| d.join("src"))
                .unwrap_or_else(|| manifest.clone());
            member_src_dirs.push((package.name.to_string(), src));
        }

        let mut graph = CrateGraph::default();
        for (name, src_dir) in &member_src_dirs {
            graph.add_node(name);
            let self_import = name.replace('-', "_");
            for file in crate::repo_surface::rust_files(src_dir) {
                let text = std::fs::read_to_string(&file)
                    .with_context(|| format!("read {}", file.display()))?;
                for import in scan_crate_imports(&text) {
                    if import == self_import {
                        continue; // self-reference, not a dependency edge.
                    }
                    if let Some(target) = import_to_name.get(&import) {
                        graph.add_edge(name, target);
                    }
                }
            }
        }
        Ok(graph)
    }
}

impl Oracle for SourceUsageGraphOracle {
    fn name(&self) -> &str {
        "source-usage"
    }

    fn claims(&self, repo_root: &Path) -> Result<ClaimSet> {
        let graph = Self::graph(repo_root)?;
        // ONLY the acyclic predicate is cross-checkable (see the oracle doc);
        // emitting edge-signature would false-fire on a legal manifest superset.
        let mut set = ClaimSet::new();
        set.assert(
            self.name(),
            "workspace-crate-graph",
            "acyclic",
            graph.cycles().is_empty().to_string(),
        );
        Ok(set)
    }
}

// --- FACT D7-C: workspace MEMBER SET (cargo-metadata vs git+manifest text). ---
//
// subject="workspace", predicate="member-set", value = sorted comma-joined crate
// names. Two GENUINELY INDEPENDENT derivations:
//
//   Oracle 1 (`cargo-member-set`): cargo's own workspace resolution
//     (`metadata.workspace_members` → package names). This reuses the SAME
//     `cargo metadata` evidence the crate-graph oracles use.
//
//   Oracle 2 (`git-member-set`): NO cargo resolution at all. It enumerates
//     tracked `Cargo.toml` files via `git ls-files`, reads the ROOT manifest's
//     `members`/`exclude` arrays TEXTUALLY, admits only manifests whose directory
//     the textual `members` globs admit (and `exclude` does not), and line-scans
//     each admitted manifest's `[package] name = "..."`. Its evidence artifacts
//     (git's tracked-file set + raw manifest text + textual glob parsing) share no
//     code path with cargo's resolver, so a disagreement is a real finding — e.g.
//     a directory the `members` glob admits that has a `Cargo.toml` but that cargo
//     dropped from the workspace (or vice versa).

/// One tracked-manifest view row for the git+text oracle: a workspace-root-relative
/// manifest DIRECTORY (forward-slashed, e.g. `crates/core`) and the `[package]
/// name` line-scanned out of that `Cargo.toml`. `name` is `None` for a virtual or
/// nameless manifest (it contributes no member name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrackedManifest {
    pub(crate) dir: String,
    pub(crate) name: Option<String>,
}

/// The injected inputs the git+text member-set oracle derives from — kept as a
/// pure value so a RED fixture can drive [`textual_member_set`] without a live
/// tree or any `git`/`cargo` invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GitManifestView {
    /// Every tracked `Cargo.toml` (except the root) as a `(dir, name)` row.
    pub(crate) manifests: Vec<TrackedManifest>,
    /// The root manifest's `members` array, parsed TEXTUALLY (each entry a path
    /// or a trailing-`*` glob, forward-slashed).
    pub(crate) members: Vec<String>,
    /// The root manifest's `exclude` array, parsed TEXTUALLY.
    pub(crate) exclude: Vec<String>,
}

/// Match a cargo workspace member/exclude glob `pattern` against a manifest
/// `dir`, both forward-slashed and root-relative. Cargo member globs support a
/// trailing `*` matching exactly one path component (`crates/*` admits
/// `crates/core` but not `crates/a/b`); an exact entry matches that one directory.
/// Deliberately a tiny dedicated matcher (not cargo's resolver) so this oracle's
/// admission decision is textual and independent.
fn member_glob_matches(pattern: &str, dir: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix("/*") {
        // `crates/*` admits a single component directly under `crates/`.
        match dir.strip_prefix(prefix).and_then(|r| r.strip_prefix('/')) {
            Some(rest) => !rest.is_empty() && !rest.contains('/'),
            None => false,
        }
    } else {
        pattern == dir
    }
}

/// Derive the workspace member-set value PURELY from the injected git+text view:
/// keep every tracked manifest whose directory the textual `members` globs admit
/// and the `exclude` globs do not, take its `[package] name`, sort, and
/// comma-join. This is the testable core of the `git-member-set` oracle.
pub(crate) fn textual_member_set(view: &GitManifestView) -> String {
    let mut names: BTreeSet<String> = BTreeSet::new();
    for manifest in &view.manifests {
        let admitted = view
            .members
            .iter()
            .any(|pattern| member_glob_matches(pattern, &manifest.dir));
        if !admitted {
            continue;
        }
        let excluded = view
            .exclude
            .iter()
            .any(|pattern| member_glob_matches(pattern, &manifest.dir));
        if excluded {
            continue;
        }
        if let Some(name) = &manifest.name {
            names.insert(name.clone());
        }
    }
    names.into_iter().collect::<Vec<_>>().join(",")
}

/// Cross-check the two member-set derivations over INJECTED values (no live tree).
/// Builds the `workspace`/`member-set` claim pool and returns the disagreement
/// engine's verdict. Split out so a RED fixture can force a disagreement. The live
/// gate cross-checks these two oracles through the engine roster directly (both
/// are registered in `default_oracles`), so this helper is the test-only driver.
#[cfg(test)]
pub(crate) fn check_member_set_over(
    cargo_member_set: &str,
    view: &GitManifestView,
) -> Vec<Disagreement> {
    let textual = textual_member_set(view);
    let pool = vec![
        Claim {
            subject: "workspace".to_owned(),
            predicate: "member-set".to_owned(),
            value: cargo_member_set.to_owned(),
            oracle: "cargo-member-set".to_owned(),
        },
        Claim {
            subject: "workspace".to_owned(),
            predicate: "member-set".to_owned(),
            value: textual,
            oracle: "git-member-set".to_owned(),
        },
    ];
    TriangulationEngine::disagreements(&pool)
}

/// Oracle 1 for D7-C: workspace member set from cargo's own resolution.
struct CargoMemberSetOracle;

impl CargoMemberSetOracle {
    /// Sorted comma-joined member crate names from `metadata.workspace_members`.
    fn member_set(repo_root: &Path) -> Result<String> {
        let mut cmd = MetadataCommand::new();
        cmd.current_dir(repo_root);
        cmd.no_deps();
        let metadata = cmd.exec().context("cargo metadata")?;
        let members: BTreeSet<_> = metadata.workspace_members.iter().collect();
        let names: BTreeSet<String> = metadata
            .packages
            .iter()
            .filter(|p| members.contains(&p.id))
            .map(|p| p.name.to_string())
            .collect();
        Ok(names.into_iter().collect::<Vec<_>>().join(","))
    }
}

impl Oracle for CargoMemberSetOracle {
    fn name(&self) -> &str {
        "cargo-member-set"
    }

    fn claims(&self, repo_root: &Path) -> Result<ClaimSet> {
        let mut set = ClaimSet::new();
        set.assert(
            self.name(),
            "workspace",
            "member-set",
            Self::member_set(repo_root)?,
        );
        Ok(set)
    }
}

/// Oracle 2 for D7-C: workspace member set from git-tracked manifests + textual
/// root-manifest glob parsing. Uses NO `cargo metadata` / `MetadataCommand` — its
/// whole evidence base is `git ls-files` + raw `Cargo.toml` text.
struct GitMemberSetOracle;

impl GitMemberSetOracle {
    /// Build the [`GitManifestView`] from the live tree: read the root manifest's
    /// `members`/`exclude` arrays textually, then enumerate every tracked
    /// `Cargo.toml` (except the root) and line-scan its `[package] name`.
    fn view(repo_root: &Path) -> Result<GitManifestView> {
        let root_manifest = repo_root.join("Cargo.toml");
        let root_text = std::fs::read_to_string(&root_manifest)
            .with_context(|| format!("read {}", root_manifest.display()))?;
        let members = scan_workspace_array(&root_text, "members");
        let exclude = scan_workspace_array(&root_text, "exclude");

        let mut manifests = Vec::new();
        for path in tracked_repo_files(repo_root)? {
            if path.file_name().and_then(|n| n.to_str()) != Some("Cargo.toml") {
                continue;
            }
            if path == root_manifest {
                continue; // the root virtual manifest is not a member.
            }
            let Some(dir) = path.parent() else {
                continue;
            };
            let Ok(rel) = dir.strip_prefix(repo_root) else {
                continue; // outside the workspace root (e.g. project-root sibling).
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            manifests.push(TrackedManifest {
                dir: rel,
                name: scan_package_name(&text),
            });
        }
        manifests.sort_by(|a, b| a.dir.cmp(&b.dir));
        Ok(GitManifestView {
            manifests,
            members,
            exclude,
        })
    }
}

impl Oracle for GitMemberSetOracle {
    fn name(&self) -> &str {
        "git-member-set"
    }

    fn claims(&self, repo_root: &Path) -> Result<ClaimSet> {
        let view = Self::view(repo_root)?;
        let mut set = ClaimSet::new();
        set.assert(
            self.name(),
            "workspace",
            "member-set",
            textual_member_set(&view),
        );
        Ok(set)
    }
}

/// Scan the root `Cargo.toml` text for a `[workspace]`-table array (`members` or
/// `exclude`) and return its string entries, forward-slashed. Deliberately a line
/// scanner (NOT a TOML/cargo parser) so the git-member-set oracle derives the
/// admission globs by genuinely different means than cargo's resolver. Handles the
/// common multi-line array form (one entry per line, trailing comma) and skips
/// comment lines.
fn scan_workspace_array(manifest: &str, key: &str) -> Vec<String> {
    let mut in_workspace = false;
    let mut in_array = false;
    let mut out = Vec::new();
    for raw in manifest.lines() {
        let line = raw.trim();
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            // Any new table header ends the [workspace] table / the array.
            in_workspace = line == "[workspace]";
            in_array = false;
            continue;
        }
        if !in_workspace {
            continue;
        }
        let scan = if in_array {
            line
        } else if let Some(rest) = strip_array_key(line, key) {
            in_array = true;
            rest
        } else {
            continue;
        };
        for entry in scan.split(',') {
            if let Some(value) = quoted_value(entry) {
                out.push(value.replace('\\', "/"));
            }
        }
        if scan.contains(']') {
            in_array = false;
        }
    }
    out
}

/// If `line` opens the array `key = [` (single- or multi-line), return the text
/// AFTER the opening `[` so any same-line entries are scanned too.
fn strip_array_key<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(key)?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    rest.strip_prefix('[')
}

/// The first double-quoted value in `s`, or `None` if there is no complete pair.
fn quoted_value(s: &str) -> Option<String> {
    let open = s.find('"')?;
    let after = &s[open + 1..];
    let close = after.find('"')?;
    Some(after[..close].to_owned())
}

/// Line-scan a member `Cargo.toml` for its `[package] name = "..."`. Returns the
/// FIRST `name` under the `[package]` table. Deliberately textual (not a TOML
/// parser) so the git-member-set oracle stays independent of cargo's manifest
/// parsing.
fn scan_package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for raw in manifest.lines() {
        let line = raw.trim();
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(rest) = line.strip_prefix("name") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                if let Some(value) = quoted_value(rest) {
                    return Some(value);
                }
            }
        }
    }
    None
}

/// Extract the set of crate-import identifiers a Rust source file references via
/// `use <ident>::`, `use <ident>;`, `extern crate <ident>`, or a leading
/// `<ident>::` path at a token boundary. Deliberately a hand token scanner (not
/// syn path-resolution) so this oracle derives edges by genuinely different means
/// than the manifest oracles. Comment lines are skipped. Only the FIRST path
/// segment is collected; the caller filters to workspace import names.
fn scan_crate_imports(source: &str) -> BTreeSet<String> {
    let mut imports = BTreeSet::new();
    for raw in source.lines() {
        let line = raw.trim_start();
        if line.starts_with("//") || line.starts_with("/*") || line.starts_with('*') {
            continue;
        }
        for token in ["use ", "extern crate "] {
            if let Some(rest) = line.strip_prefix(token) {
                let rest = rest.trim_start();
                let head = leading_ident(rest);
                if !head.is_empty() {
                    imports.insert(head);
                }
            }
        }
    }
    imports
}

/// The leading Rust identifier of `s` (ASCII alphanumeric + `_`), stopping at the
/// first non-identifier byte (e.g. `::`, `;`, whitespace, `{`).
fn leading_ident(s: &str) -> String {
    s.chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect()
}

// --- INV-DEPENDENCY-DIRECTION: the allowed-edge (layer-direction) gate. ---

/// One layer of the dependency-direction model. Crates in the same tier must not
/// depend on each other; a crate may depend only on crates in a strictly lower
/// tier (lower rank = more foundational).
#[derive(Debug, Deserialize)]
struct DirectionLayer {
    tier: String,
    crates: Vec<String>,
}

/// The parsed `dependency_direction.yaml` model.
#[derive(Debug, Deserialize)]
pub(crate) struct DependencyDirection {
    layers: Vec<DirectionLayer>,
}

impl DependencyDirection {
    /// Map each crate name to its layer `(rank, tier)` (rank 0 = most foundational).
    fn ranks(&self) -> Result<BTreeMap<String, (usize, String)>> {
        let mut ranks: BTreeMap<String, (usize, String)> = BTreeMap::new();
        for (rank, layer) in self.layers.iter().enumerate() {
            for crate_name in &layer.crates {
                if let Some((_, prior_tier)) =
                    ranks.insert(crate_name.clone(), (rank, layer.tier.clone()))
                {
                    return Err(anyhow!(
                        "dependency_direction.yaml: crate `{crate_name}` is listed in more than \
                         one layer (`{prior_tier}` and `{}`) — each crate must appear in exactly \
                         one tier.",
                        layer.tier
                    ));
                }
            }
        }
        Ok(ranks)
    }
}

pub(crate) fn dependency_direction_path(repo_root: &Path) -> std::path::PathBuf {
    repo_root.join("traceability/dependency_direction.yaml")
}

fn load_dependency_direction(repo_root: &Path) -> Result<DependencyDirection> {
    crate::repo_surface::load_yaml(&dependency_direction_path(repo_root))
}

/// Enforce INV-DEPENDENCY-DIRECTION over a crate graph: every workspace member
/// must be assigned a layer (lockstep — a NEW crate cannot escape the rule), and
/// every NORMAL build-graph edge `from -> to` must go strictly DOWNWARD
/// (`rank(from) > rank(to)`). A same-tier or upward edge is a layering inversion
/// that a plain acyclicity check would miss. Split from `check` so a RED fixture
/// can drive it over a synthetic graph + model without the live tree.
pub(crate) fn check_direction_over(graph: &CrateGraph, model: &DependencyDirection) -> Result<()> {
    let ranks = model.ranks()?;

    // Lockstep: every node in the graph must be ranked.
    let unranked: Vec<&String> = graph
        .edges
        .keys()
        .filter(|node| !ranks.contains_key(*node))
        .collect();
    ensure(
        unranked.is_empty(),
        format!(
            "INV-DEPENDENCY-DIRECTION: workspace crate(s) absent from \
             dependency_direction.yaml: {}. Every member must be assigned a layer so a NEW crate \
             cannot silently escape the direction rule.",
            unranked
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )?;

    let mut violations: Vec<String> = Vec::new();
    for (from, tos) in &graph.edges {
        let Some((from_rank, from_tier)) = ranks.get(from) else {
            continue; // handled by the lockstep above.
        };
        for to in tos {
            let Some((to_rank, to_tier)) = ranks.get(to) else {
                continue;
            };
            if from_rank <= to_rank {
                violations.push(format!(
                    "{from} (tier `{from_tier}`, rank {from_rank}) -> {to} (tier `{to_tier}`, rank {to_rank})"
                ));
            }
        }
    }
    violations.sort();
    ensure(
        violations.is_empty(),
        format!(
            "INV-DEPENDENCY-DIRECTION: dependency edge(s) violate the declared layer order \
             (a crate may depend only on a STRICTLY LOWER layer; same-layer and upward edges are \
             forbidden):\n  {}\n\
             Fix the edge (depend downward) or correct dependency_direction.yaml if the layering \
             genuinely changed. See INV-DEPENDENCY-DIRECTION.",
            violations.join("\n  ")
        ),
    )?;
    Ok(())
}

/// Scan a `Cargo.toml` text body for `path = "..."` values that appear inside a
/// normal `[dependencies]` (or `[target.'...'.dependencies]`) section.
/// `[dev-dependencies]` and `[build-dependencies]` are deliberately excluded:
/// they are not part of the library build graph, and Cargo permits cycles
/// through them (e.g. a `*-testkit` crate that path-depends on the crate it
/// supports, which dev-depends on the testkit back). Counting them would trip
/// INV-WORKSPACE-DAG-ACYCLIC on a legal, non-build-graph cycle. Deliberately a
/// line scanner independent of cargo's own parser so the two oracles derive
/// edges by genuinely different means.
fn scan_path_dependencies(manifest: &str) -> Vec<String> {
    let mut in_deps = false;
    let mut paths = Vec::new();
    for raw in manifest.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            // Match only normal dependency tables: `[dependencies]` and
            // target-scoped `[target.'cfg(...)'.dependencies]`. Exclude
            // `[dev-dependencies]` / `[build-dependencies]` and their
            // target-scoped variants.
            let header = line.trim_start_matches('[').trim_end_matches(']');
            in_deps = header == "dependencies" || header.ends_with(".dependencies");
            continue;
        }
        if !in_deps || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.split("path").nth(1) {
            // expect: = "..."
            if let Some(open) = rest.find('"') {
                if let Some(close) = rest[open + 1..].find('"') {
                    paths.push(rest[open + 1..open + 1 + close].to_owned());
                }
            }
        }
    }
    paths
}

/// Project a crate graph into the two claims every graph oracle emits over the
/// shared subject `workspace-crate-graph`: the acyclicity verdict and the full
/// edge signature. Two oracles agreeing on BOTH is the green case.
fn graph_claims(oracle: &str, graph: &CrateGraph) -> ClaimSet {
    let mut set = ClaimSet::new();
    let cycles = graph.cycles();
    set.assert(
        oracle,
        "workspace-crate-graph",
        "acyclic",
        (cycles.is_empty()).to_string(),
    );
    set.assert(
        oracle,
        "workspace-crate-graph",
        "edge-signature",
        graph.edge_signature(),
    );
    set
}

/// The default oracle roster for the live gate. Exposed so a test can swap in a
/// synthetic-disagreement oracle without touching `check`.
fn default_oracles() -> Vec<Box<dyn Oracle>> {
    vec![
        Box::new(CargoMetadataGraphOracle),
        Box::new(ManifestScanGraphOracle),
        Box::new(SourceUsageGraphOracle),
        Box::new(CargoMemberSetOracle),
        Box::new(GitMemberSetOracle),
    ]
}

/// The oracle names the live roster emits — the in-code source of truth the
/// `fitness_functions.yaml` lockstep cross-checks. Derived from `default_oracles`
/// so it can never drift from the roster the gate actually runs.
pub(crate) fn default_oracle_names() -> Vec<String> {
    default_oracles()
        .iter()
        .map(|o| o.name().to_owned())
        .collect()
}

/// The catalog invariant ids the triangulation gate enforces — the in-code source
/// of truth the `fitness_functions.yaml` lockstep cross-checks.
pub(crate) const ENFORCED_INVARIANTS: &[&str] =
    &["INV-WORKSPACE-DAG-ACYCLIC", "INV-DEPENDENCY-DIRECTION"];

/// Blocking gate (`GAUNTLET-TRIANGULATION`, fact: `INV-WORKSPACE-DAG-ACYCLIC`).
/// Fails if (a) the two crate-graph oracles DISAGREE on any shared predicate, or
/// (b) they AGREE that the workspace crate graph contains a dependency cycle.
/// The clean live tree is a DAG with both oracles agreeing ⟹ green.
pub(crate) fn check(repo_root: &Path) -> Result<()> {
    let engine = TriangulationEngine::new(default_oracles());
    let pool = engine.collect(repo_root)?;

    let disagreements = TriangulationEngine::disagreements(&pool);
    ensure(
        disagreements.is_empty(),
        format!(
            "triangulation: oracles disagree (each is one non-privileged derivation; reconcile the \
             code or the oracles, the engine never picks a winner):\n  {}\n\
             See INV-WORKSPACE-DAG-ACYCLIC and GAUNTLET-TRIANGULATION.",
            disagreements
                .iter()
                .map(Disagreement::render)
                .collect::<Vec<_>>()
                .join("\n  ")
        ),
    )?;

    // Oracles agree; now enforce the agreed fact. Re-derive once for the cycle
    // report (both oracles agreed on the verdict, so either graph is canonical).
    let graph = CargoMetadataGraphOracle::graph(repo_root)?;
    let cycles = graph.cycles();
    ensure(
        cycles.is_empty(),
        format!(
            "triangulation: workspace crate graph is NOT acyclic — dependency cycle(s) found: {}.\n\
             A cargo workspace dependency cycle breaks layering and incremental builds; break the \
             cycle by extracting the shared surface into a lower crate.\n\
             See INV-WORKSPACE-DAG-ACYCLIC.",
            cycles
                .iter()
                .map(|c| format!("[{}]", c.join(" <-> ")))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )?;

    // Oracles agree the graph is acyclic; now enforce the STRONGER directional
    // invariant (INV-DEPENDENCY-DIRECTION): every edge must go strictly downward
    // in the declared layer order. A legal DAG can still carry an upward edge that
    // acyclicity alone permits; the direction model forbids it.
    let model = load_dependency_direction(repo_root)?;
    check_direction_over(&graph, &model)?;

    outln!(
        "triangulation: ok ({} oracles agree; workspace crate graph is a DAG respecting \
         dependency_direction.yaml layer order)",
        engine.oracles.len()
    );
    Ok(())
}
