//! GAUNTLET-REPO-IR (Phase 3, item 6) — the minimal queryable repo-IR backbone.
//!
//! ONE normalized, serializable structure that binds the gauntlet's otherwise
//! scattered fact families into a single column-store (data-oriented / ECS): each
//! fact kind is its own column keyed by a `RepoNodeId`, so a fitness function may
//! iterate one column without touching unrelated data, and a NEW fact kind is a
//! NEW column rather than churn in the existing checks. Today each gate re-walks
//! the repo independently (`architecture_lints`, `harness_lints`, `invariant_bridge`,
//! `traceability` each parse files via their own `SourceCache`); the repo-IR is
//! the shared substrate they fold over — banana-split-fused: ONE traversal, N
//! checks (the same fold-fusion law `project_fused2/3` tests for projections,
//! turned on the gauntlet's own facts — the metacircular tie).
//!
//! This is the Phase 3 SKELETON. It binds SIX fact families that already have
//! authoritative homes elsewhere in the integrity crate, so the IR is a pure
//! projection (no new source-of-truth):
//!   1. AL assignments        — `assurance.rs` manifest (`AssuranceEntry`).
//!   2. gate ownership        — `gate_registry::GATES` (slug, blocking, red fixture).
//!   3. waiver ownership      — `typed_waivers.yaml` (`Waiver`: id, owner, target).
//!   4. public-surface map    — store pub-fn coverage inventory.
//!   5. mutation-seam map     — `assurance::CRITICAL_SEAM_MUTANT_GLOBS` (slug→glob).
//!   6. docs traceability     — `invariants.yaml` catalog (id, witness_test).
//!
//! The fused fold-runner ([`run_fitness`]) applies every registered [`Fitness`]
//! in a SINGLE pass over the IR; [`run_fitness_separately`] runs each fitness in
//! its own pass. The metacircular law [`tests::run_fused_equals_run_separate`]
//! asserts the two produce identical findings (order-independent) for ≥2 facts —
//! the gauntlet's own fold fusion is itself tested, exactly as item 1a does for
//! projections.
//!
//! DEFERRED breadth (logged, not built here): syn-derived symbol/call/type
//! columns, crate-graph dep edges (lives in `triangulation.rs` today), on-disk
//! format-version column (item 4), the YAML-authored fitness registry (item 3.4),
//! and re-hosting the existing serial checks onto the IR (item 6.3). See
//! GAUNTLET_ISSUES.md.
//!
//! Anchors: INV-GAUNTLET-FOLD-FUSION (traceability/invariants.yaml),
//! architecture_ir.rs (the on-disk IR sibling this complements).

use crate::assurance::{self, AssuranceEntry};
use crate::docs_catalog::{self, CatalogInvariant};
use crate::gate_registry::{self, Gate};
use crate::source_cache::SourceCache;
use crate::store_pub_fn_coverage;
use crate::typed_waivers::{self, Waiver};
use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

const SCHEMA_VERSION: u32 = 1;

/// Stable per-node identity inside the IR: `(kind, ordinal)`. The ordinal is the
/// row index within that kind's column, so a node is addressable without a global
/// arena — enough for the minimal skeleton's column scans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) struct RepoNodeId {
    pub(crate) kind: NodeKind,
    pub(crate) ordinal: usize,
}

/// The fact-family columns. Each variant is one ECS component column; a fitness
/// function declares the single column it folds over via [`Fitness::over`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum NodeKind {
    AlAssignment,
    Gate,
    Waiver,
    PublicSurface,
    MutationSeam,
    DocInvariant,
}

/// AL (assurance-level) assignment: a manifest entry's level + the optional seam
/// it mirrors + its glob set. (ECS column 1.)
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AlAssignmentFact {
    pub(crate) level: String,
    pub(crate) seam: Option<String>,
    pub(crate) globs: Vec<String>,
}

/// Gate ownership: a registry gate's slug, blocking authority, and named red
/// fixture (the DO-178B qualification owner). (ECS column 2.)
#[derive(Debug, Clone, Serialize)]
pub(crate) struct GateFact {
    pub(crate) slug: String,
    pub(crate) has_blocking_authority: bool,
    pub(crate) red_fixture_test: Option<String>,
}

/// Waiver ownership: a typed waiver's id, the human owner, the gate family it
/// waives, and its target. (ECS column 3.)
#[derive(Debug, Clone, Serialize)]
pub(crate) struct WaiverFact {
    pub(crate) id: String,
    pub(crate) owner: String,
    pub(crate) kind: String,
    pub(crate) target: String,
}

/// Public-surface map: a `Store` public fn and whether it is test-covered /
/// allowlisted. (ECS column 4.)
#[derive(Debug, Clone, Serialize)]
pub(crate) struct PublicSurfaceFact {
    pub(crate) name: String,
    pub(crate) covered: bool,
    pub(crate) allowlisted: bool,
}

/// Mutation-seam map: a critical seam slug and one of its mutant-file globs.
/// (ECS column 5.)
#[derive(Debug, Clone, Serialize)]
pub(crate) struct MutationSeamFact {
    pub(crate) slug: String,
    pub(crate) glob: String,
}

/// Docs traceability: a catalog invariant id, its statement, and the strong-tier
/// `witness_test` citation (if any). (ECS column 6.)
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DocInvariantFact {
    pub(crate) id: String,
    pub(crate) statement: String,
    pub(crate) witness_test: Option<String>,
}

/// The normalized repo-IR: parallel component columns keyed by row ordinal.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RepoIr {
    pub(crate) schema_version: u32,
    pub(crate) generated_by: &'static str,
    pub(crate) al_assignments: Vec<AlAssignmentFact>,
    pub(crate) gates: Vec<GateFact>,
    pub(crate) waivers: Vec<WaiverFact>,
    pub(crate) public_surface: Vec<PublicSurfaceFact>,
    pub(crate) mutation_seams: Vec<MutationSeamFact>,
    pub(crate) doc_invariants: Vec<DocInvariantFact>,
}

/// One finding emitted by a fitness function: the node it concerns and a message.
/// `Ord` makes the order-independence assertion in the metacircular law a simple
/// sorted-set comparison.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Finding {
    pub(crate) node: RepoNodeId,
    pub(crate) fitness: &'static str,
    pub(crate) message: String,
}

/// A fitness function folds over exactly ONE node-kind column. The fused runner
/// groups fitnesses by `over()` and applies all of a column's fitnesses in a
/// single pass over that column (the banana-split fusion).
pub(crate) trait Fitness {
    fn name(&self) -> &'static str;
    fn over(&self) -> NodeKind;
    fn check(&self, id: RepoNodeId, ir: &RepoIr, sink: &mut Vec<Finding>);
}

impl RepoIr {
    /// Borrow a column's length for the fused runner's index walk.
    fn column_len(&self, kind: NodeKind) -> usize {
        match kind {
            NodeKind::AlAssignment => self.al_assignments.len(),
            NodeKind::Gate => self.gates.len(),
            NodeKind::Waiver => self.waivers.len(),
            NodeKind::PublicSurface => self.public_surface.len(),
            NodeKind::MutationSeam => self.mutation_seams.len(),
            NodeKind::DocInvariant => self.doc_invariants.len(),
        }
    }
}

/// FUSED fold: one pass per node-kind column, applying ALL fitnesses registered
/// for that kind. This is `project_fusedN`'s shape (one traversal, N folds) turned
/// on repo facts instead of events.
pub(crate) fn run_fitness(ir: &RepoIr, fitnesses: &[&dyn Fitness]) -> Vec<Finding> {
    let mut findings = Vec::new();
    for kind in ALL_KINDS {
        let column: Vec<&&dyn Fitness> = fitnesses.iter().filter(|f| f.over() == kind).collect();
        if column.is_empty() {
            continue;
        }
        let len = ir.column_len(kind);
        for ordinal in 0..len {
            let id = RepoNodeId { kind, ordinal };
            for fitness in &column {
                fitness.check(id, ir, &mut findings);
            }
        }
    }
    findings.sort();
    findings
}

/// SEPARATE fold: each fitness gets its own full pass over its column. The
/// metacircular law asserts this is equivalent (as a set) to [`run_fitness`].
/// Test-only: it exists purely to be the other side of the fold-fusion equality.
#[cfg(test)]
pub(crate) fn run_fitness_separately(ir: &RepoIr, fitnesses: &[&dyn Fitness]) -> Vec<Finding> {
    let mut findings = Vec::new();
    for fitness in fitnesses {
        let kind = fitness.over();
        let len = ir.column_len(kind);
        for ordinal in 0..len {
            let id = RepoNodeId { kind, ordinal };
            fitness.check(id, ir, &mut findings);
        }
    }
    findings.sort();
    findings
}

const ALL_KINDS: [NodeKind; 6] = [
    NodeKind::AlAssignment,
    NodeKind::Gate,
    NodeKind::Waiver,
    NodeKind::PublicSurface,
    NodeKind::MutationSeam,
    NodeKind::DocInvariant,
];

/// Build the IR by projecting the six fact families from their authoritative
/// homes. Pure projection — no fact is born here.
pub(crate) fn build(repo_root: &Path) -> Result<RepoIr> {
    let al_assignments = build_al_assignments(repo_root)?;
    let gates = build_gates();
    let waivers = build_waivers(repo_root)?;
    let public_surface = build_public_surface(repo_root)?;
    let mutation_seams = build_mutation_seams();
    let doc_invariants = build_doc_invariants(repo_root)?;

    Ok(RepoIr {
        schema_version: SCHEMA_VERSION,
        generated_by: "batpak-integrity repo-ir",
        al_assignments,
        gates,
        waivers,
        public_surface,
        mutation_seams,
        doc_invariants,
    })
}

fn build_al_assignments(repo_root: &Path) -> Result<Vec<AlAssignmentFact>> {
    let entries: Vec<AssuranceEntry> = assurance::load_manifest(repo_root)?;
    Ok(entries
        .into_iter()
        .map(|entry| AlAssignmentFact {
            level: entry.level.as_str().to_owned(),
            seam: entry.seam,
            globs: entry.globs,
        })
        .collect())
}

fn build_gates() -> Vec<GateFact> {
    gate_registry::GATES
        .iter()
        .map(|gate: &Gate| GateFact {
            slug: gate.slug.to_owned(),
            has_blocking_authority: gate.has_blocking_authority,
            red_fixture_test: gate.red_fixture_test.map(str::to_owned),
        })
        .collect()
}

fn build_waivers(repo_root: &Path) -> Result<Vec<WaiverFact>> {
    let waivers: Vec<Waiver> = typed_waivers::load_waivers(repo_root)?;
    Ok(waivers
        .into_iter()
        .map(|waiver| WaiverFact {
            id: waiver.id,
            owner: waiver.owner,
            kind: format!("{:?}", waiver.kind),
            target: waiver.target,
        })
        .collect())
}

fn build_public_surface(repo_root: &Path) -> Result<Vec<PublicSurfaceFact>> {
    let mut cache = SourceCache::new(repo_root);
    Ok(store_pub_fn_coverage::inventory(repo_root, &mut cache)?
        .into_iter()
        .map(|entry| PublicSurfaceFact {
            name: entry.name,
            covered: entry.covered,
            allowlisted: entry.allowlisted,
        })
        .collect())
}

fn build_mutation_seams() -> Vec<MutationSeamFact> {
    assurance::CRITICAL_SEAM_MUTANT_GLOBS
        .iter()
        .map(|(slug, glob)| MutationSeamFact {
            slug: (*slug).to_owned(),
            glob: (*glob).to_owned(),
        })
        .collect()
}

fn build_doc_invariants(repo_root: &Path) -> Result<Vec<DocInvariantFact>> {
    let catalog: Vec<CatalogInvariant> = docs_catalog::load_catalog(repo_root)?;
    Ok(catalog
        .into_iter()
        .map(|invariant| DocInvariantFact {
            id: invariant.id,
            statement: invariant.statement,
            witness_test: invariant.witness_test,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Skeleton fitness functions (≥2 facts) — these are the metacircular witnesses,
// NOT yet the blocking gates (those are re-hosted incrementally; see DEFERRED).
// ---------------------------------------------------------------------------

/// FACT 1: every gate that claims blocking authority must name a red fixture.
/// (Mirrors `gate_registry::check`'s law as a fold over the Gate column.)
pub(crate) struct BlockingGateNamesRedFixture;

impl Fitness for BlockingGateNamesRedFixture {
    fn name(&self) -> &'static str {
        "blocking-gate-names-red-fixture"
    }
    fn over(&self) -> NodeKind {
        NodeKind::Gate
    }
    fn check(&self, id: RepoNodeId, ir: &RepoIr, sink: &mut Vec<Finding>) {
        let gate = &ir.gates[id.ordinal];
        if gate.has_blocking_authority && gate.red_fixture_test.is_none() {
            sink.push(Finding {
                node: id,
                fitness: self.name(),
                message: format!(
                    "gate `{}` claims blocking authority but names no red fixture",
                    gate.slug
                ),
            });
        }
    }
}

/// FACT 2: every catalog invariant carrying a `witness_test` must spell it as a
/// `path::fn` reference (the strong-tier shape `docs_catalog` resolves). This is a
/// shape check over the DocInvariant column — distinct family from FACT 1.
pub(crate) struct WitnessTestIsPathFnShaped;

impl Fitness for WitnessTestIsPathFnShaped {
    fn name(&self) -> &'static str {
        "witness-test-is-path-fn-shaped"
    }
    fn over(&self) -> NodeKind {
        NodeKind::DocInvariant
    }
    fn check(&self, id: RepoNodeId, ir: &RepoIr, sink: &mut Vec<Finding>) {
        let inv = &ir.doc_invariants[id.ordinal];
        if let Some(witness) = &inv.witness_test {
            if !witness.contains("::") {
                sink.push(Finding {
                    node: id,
                    fitness: self.name(),
                    message: format!(
                        "invariant `{}` witness_test `{witness}` is not `path::fn`-shaped",
                        inv.id
                    ),
                });
            }
        }
    }
}

/// The registered fitness set for the skeleton. Adding a fitness here is the
/// "novel predicate KIND needs Rust" escape hatch (item 3.4); the YAML-authored
/// common case is DEFERRED.
pub(crate) fn registered_fitnesses() -> Vec<Box<dyn Fitness>> {
    vec![
        Box::new(BlockingGateNamesRedFixture),
        Box::new(WitnessTestIsPathFnShaped),
    ]
}

/// Subcommand entry: build the IR, fold the registered fitnesses over it via the
/// FUSED runner (one traversal, N checks), report any findings, and emit the IR
/// as JSON (stdout or `--out`). The fitness pass is advisory in this skeleton —
/// the blocking re-host of existing checks onto the IR is DEFERRED (item 6.3).
pub(crate) fn run(repo_root: &Path, out: Option<PathBuf>) -> Result<()> {
    let ir = build(repo_root)?;

    let owned = registered_fitnesses();
    let fitnesses: Vec<&dyn Fitness> = owned.iter().map(AsRef::as_ref).collect();
    let findings = run_fitness(&ir, &fitnesses);
    if findings.is_empty() {
        eprintln!(
            "repo-ir: {} fitness function(s) clean over {} fact families",
            fitnesses.len(),
            ALL_KINDS.len()
        );
    } else {
        for finding in &findings {
            eprintln!("repo-ir finding [{}]: {}", finding.fitness, finding.message);
        }
    }

    let rendered = format!("{}\n", serde_json::to_string_pretty(&ir)?);
    match out {
        Some(out) => {
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
            std::fs::write(&out, rendered).with_context(|| format!("write {}", out.display()))?;
            println!("repo-ir: wrote {}", out.display());
        }
        None => print!("{rendered}"),
    }
    Ok(())
}

#[cfg(test)]
#[path = "repo_ir_tests.rs"]
mod repo_ir_tests;
