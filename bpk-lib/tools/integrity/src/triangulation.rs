//! Triangulation harness (`GAUNTLET-TRIANGULATION`) — N-version differential
//! testing over NON-TYPE repo facts. No oracle is privileged: each oracle emits
//! a normalized claim set and the engine flags any `(subject, predicate)` group
//! where two oracles DISAGREE on the value. A disagreement is a hard finding —
//! the engine never picks a winner; a human/agent must reconcile the code or the
//! oracles. rustc is the type source-of-truth and is deliberately NOT re-derived
//! here (see spec item 3.0); triangulation targets architecture/doctrine facts.
//!
//! This is the Phase 3 SKELETON: the `Oracle` trait + disagreement engine + ONE
//! concrete triangulated fact wired blocking — workspace crate-graph acyclicity,
//! cross-checked by two independent oracles:
//!   1. `CargoMetadataGraphOracle` — edges from `cargo metadata` (`no_deps`,
//!      reading each member's declared path dependencies) + Tarjan SCC.
//!   2. `ManifestScanGraphOracle` — edges re-derived by directly scanning each
//!      member `Cargo.toml` for `path = "../<dir>"` entries + Tarjan SCC.
//! If the two graphs disagree on the acyclicity verdict (or on the edge set),
//! that is itself a finding; if they AGREE that a cycle exists, the gate fails
//! naming the cycle. Both clean + agreeing on the live tree ⟹ green.
//!
//! Anchors: INV-WORKSPACE-DAG-ACYCLIC (traceability/invariants.yaml),
//! architecture_ir.rs (cargo-metadata projection it complements).

use crate::repo_surface::ensure;
use anyhow::{Context, Result};
use cargo_metadata::MetadataCommand;
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
    fn new() -> Self {
        Self { claims: Vec::new() }
    }

    fn assert(&mut self, oracle: &str, subject: &str, predicate: &str, value: impl Into<String>) {
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
    fn render(&self) -> String {
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
    ]
}

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

    outln!(
        "triangulation: ok ({} oracles agree; workspace crate graph is a DAG)",
        engine.oracles.len()
    );
    Ok(())
}
