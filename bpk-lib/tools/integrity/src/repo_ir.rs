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
//! It binds SIX fact families that already have authoritative homes elsewhere in
//! the integrity crate, so the IR is a pure projection (no new source-of-truth):
//!   1. AL assignments        — `assurance.rs` manifest (`AssuranceEntry`).
//!   2. gate ownership        — `gate_registry::GATES` (slug, blocking, red fixture).
//!   3. waiver ownership      — `typed_waivers.yaml` (`Waiver`: id, owner, target).
//!   4. public-surface map    — store pub-fn coverage inventory.
//!   5. mutation-seam map     — PARSED from `traceability/seam_registry.yaml`
//!      (slug → glob → assurance level). This column is no longer mirrored from
//!      the in-code `assurance::CRITICAL_SEAM_MUTANT_GLOBS` array (the exact
//!      mirror-drift surface D9 was meant to retire); it reads the YAML source
//!      directly, so a seam edited only in the registry is reflected here.
//!   6. docs traceability     — `invariants.yaml` catalog (id, witness_test).
//!
//! The fused fold-runner ([`run_fitness`]) applies every registered [`Fitness`]
//! in a SINGLE pass over the IR; `run_fitness_separately` runs each fitness in
//! its own pass. The metacircular law `tests::run_fused_equals_run_separate`
//! asserts the two produce identical findings (order-independent) for ≥2 facts —
//! the gauntlet's own fold fusion is itself tested, exactly as item 1a does for
//! projections.
//!
//! BLOCKING (D9): [`check`] folds the BLOCKING fitnesses over the live IR and
//! `bail!`s on any finding, so the fitness runner is wired into the serial
//! `structural-check` run path (`structural.rs`) as a real gate (`repo-ir-fitness`)
//! with a qualified anti-vacuous RED fixture — not an advisory skeleton. The
//! blocking invariant it enforces is parse-derived and non-duplicate: every seam
//! glob PARSED from `seam_registry.yaml` must match ≥1 tracked file (a dead glob
//! in the registry = 0 mutants = vacuous PASS; the assurance lockstep checks only
//! YAML↔mirror string-equality, and `glob_coverage` scans only `lanes.rs`, so the
//! registry's own globs are otherwise unchecked against the filesystem).
//!
//! DEFERRED breadth (logged, not built here): syn-derived symbol/call/type
//! columns, crate-graph dep edges (lives in `triangulation.rs` today), on-disk
//! format-version column (item 4), the YAML-authored fitness registry (item 3.4).
//! Deferred breadth is tracked on the release board (`traceability/releases/0.9.0.yaml`)
//! and live seam truth (`traceability/seam_registry.yaml`).
//!
//! Anchors: INV-GAUNTLET-FOLD-FUSION (traceability/invariants.yaml),
//! architecture_ir.rs (the on-disk IR sibling this complements).

use crate::assurance::{self, AssuranceEntry, SeamRegistryEntry};
use crate::docs_catalog::{self, CatalogInvariant};
use crate::gate_registry::{self, Gate};
use crate::repo_surface::{relative, tracked_repo_files};
use crate::source_cache::SourceCache;
use crate::store_pub_fn_coverage;
use crate::typed_waivers::{self, Waiver};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::BTreeSet;
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

/// Mutation-seam map: a critical seam slug, one of its mutant-file globs, and the
/// seam's declared assurance level — all PARSED from `seam_registry.yaml` (not
/// mirrored from the in-code array). The level fills the previously-thin column so
/// a fitness can fold over seam criticality. (ECS column 5.)
#[derive(Debug, Clone, Serialize)]
pub(crate) struct MutationSeamFact {
    pub(crate) slug: String,
    pub(crate) glob: String,
    pub(crate) assurance_level: String,
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
    let mutation_seams = build_mutation_seams(repo_root)?;
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

/// PARSE the seam column from `traceability/seam_registry.yaml` — one row per
/// `(slug, glob)` pair, carrying the seam's declared assurance level. This is the
/// D9 parse-not-mirror change: the column now reflects the YAML source directly
/// rather than the in-code `CRITICAL_SEAM_MUTANT_GLOBS` mirror, so a seam edited
/// only in the registry (the exact mirror-drift D9 retires) is visible here.
fn build_mutation_seams(repo_root: &Path) -> Result<Vec<MutationSeamFact>> {
    let registry: Vec<SeamRegistryEntry> = assurance::load_seam_registry(repo_root)?;
    let mut facts = Vec::new();
    for entry in registry {
        for glob in &entry.globs {
            facts.push(MutationSeamFact {
                slug: entry.slug.clone(),
                glob: glob.clone(),
                assurance_level: entry.assurance_level.clone(),
            });
        }
    }
    Ok(facts)
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

/// FACT 3 (D9): every mutation seam PARSED from `seam_registry.yaml` must declare
/// a recognized assurance level (`L0`..`L4`). The registry's `assurance_level` is
/// a free `String` field (NOT deserialized into the `AssuranceLevel` enum), so a
/// typo'd or invalid tier is otherwise unchecked — the lockstep compares only
/// `(slug, glob)` pairs, never the level. This fitness folds over the seam column
/// and is a BLOCKING fitness (`BLOCKING_FITNESSES`). Distinct family from the
/// Gate/DocInvariant fitnesses above.
pub(crate) struct SeamLevelIsRecognized;

const RECOGNIZED_LEVELS: [&str; 5] = ["L0", "L1", "L2", "L3", "L4"];

impl Fitness for SeamLevelIsRecognized {
    fn name(&self) -> &'static str {
        "seam-level-is-recognized"
    }
    fn over(&self) -> NodeKind {
        NodeKind::MutationSeam
    }
    fn check(&self, id: RepoNodeId, ir: &RepoIr, sink: &mut Vec<Finding>) {
        let seam = &ir.mutation_seams[id.ordinal];
        if !RECOGNIZED_LEVELS.contains(&seam.assurance_level.as_str()) {
            sink.push(Finding {
                node: id,
                fitness: self.name(),
                message: format!(
                    "seam `{}` glob `{}` declares unrecognized assurance level `{}` (want one of {:?})",
                    seam.slug, seam.glob, seam.assurance_level, RECOGNIZED_LEVELS
                ),
            });
        }
    }
}

/// The registered fitness set. Adding a fitness here is the "novel predicate KIND
/// needs Rust" escape hatch (item 3.4); the YAML-authored common case is DEFERRED.
pub(crate) fn registered_fitnesses() -> Vec<Box<dyn Fitness>> {
    vec![
        Box::new(BlockingGateNamesRedFixture),
        Box::new(WitnessTestIsPathFnShaped),
        Box::new(SeamLevelIsRecognized),
    ]
}

/// The fitnesses whose findings BLOCK (fail `structural-check`) when [`check`]
/// folds them over the live IR. The full `registered_fitnesses()` set is run
/// advisory by the `repo-ir` subcommand; this subset is the gate.
fn blocking_fitnesses() -> Vec<Box<dyn Fitness>> {
    vec![
        Box::new(BlockingGateNamesRedFixture),
        Box::new(WitnessTestIsPathFnShaped),
        Box::new(SeamLevelIsRecognized),
    ]
}

/// BLOCKING gate entry (`repo-ir-fitness`, D9). Build the live IR, fold the
/// [`blocking_fitnesses`] over it, and `bail!` on ANY finding — so the fitness
/// runner is a real serial gate, not an advisory skeleton. Additionally checks a
/// parse-derived, FS-coupled invariant the fold cannot express: every seam glob
/// PARSED from `seam_registry.yaml` must match ≥1 tracked file (a dead registry
/// glob = 0 mutants = vacuous PASS; `glob_coverage` scans only `lanes.rs`, and the
/// assurance lockstep compares only YAML↔mirror string pairs, so the registry's
/// own globs are otherwise never checked against the filesystem). Returns real
/// `files_examined`/`assertions_run` counts for the receipt.
pub(crate) fn check(repo_root: &Path) -> Result<crate::receipts::GateWork> {
    let ir = build(repo_root)?;

    let owned = blocking_fitnesses();
    let fitnesses: Vec<&dyn Fitness> = owned.iter().map(AsRef::as_ref).collect();
    let findings = run_fitness(&ir, &fitnesses);
    if let Some(first) = findings.first() {
        bail!(
            "repo-ir-fitness: {} blocking finding(s) over the live repo-IR. First: [{}] {}",
            findings.len(),
            first.fitness,
            first.message
        );
    }

    // FS-coupled seam-glob existence check (cannot be a pure-IR fitness: it needs
    // the tracked-file set). Every parsed registry glob must match a real file.
    check_seam_globs_resolve(repo_root, &ir.mutation_seams)?;

    // files_examined: the IR's input families that were folded; assertions_run:
    // one per fitness-over-row plus one per seam-glob existence assertion.
    let seam_glob_assertions = ir.mutation_seams.len();
    let fold_assertions = column_total_for(&ir, &fitnesses);
    let assertions = fold_assertions.saturating_add(seam_glob_assertions).max(1);
    let mut inputs: BTreeSet<PathBuf> = BTreeSet::new();
    inputs.insert(assurance::seam_registry_path(repo_root));
    inputs.insert(assurance::manifest_path(repo_root));
    inputs.insert(repo_root.join("traceability").join("invariants.yaml"));
    let files = inputs.len().max(1);
    Ok(crate::receipts::GateWork::new(files, assertions, inputs))
}

/// Sum, across the fold, of (rows in each fitness's column) — the real number of
/// fitness assertions executed by [`run_fitness`].
fn column_total_for(ir: &RepoIr, fitnesses: &[&dyn Fitness]) -> usize {
    fitnesses
        .iter()
        .map(|f| ir.column_len(f.over()))
        .fold(0usize, usize::saturating_add)
}

/// Assert every seam glob parsed from `seam_registry.yaml` matches ≥1 tracked
/// file. A dead registry glob produces zero mutants and would pass vacuously.
fn check_seam_globs_resolve(repo_root: &Path, seams: &[MutationSeamFact]) -> Result<()> {
    let tracked = tracked_repo_files(repo_root)?;
    let rels: BTreeSet<String> = tracked
        .iter()
        .map(|p| relative(repo_root, p).replace('\\', "/"))
        .collect();
    for seam in seams {
        let matched = rels
            .iter()
            .any(|rel| assurance::glob_matches(&seam.glob, rel));
        if !matched {
            bail!(
                "repo-ir-fitness: seam `{}` glob `{}` (from seam_registry.yaml) matches NO tracked \
                 file. A dead seam glob produces zero mutants and would pass mutation smoke \
                 vacuously. Fix the registry glob or retire the seam.",
                seam.slug,
                seam.glob
            );
        }
    }
    Ok(())
}

/// Subcommand entry (`repo-ir`): build the IR, fold the registered fitnesses over
/// it via the FUSED runner (one traversal, N checks), report any findings, and
/// emit the IR as JSON (stdout or `--out`). This subcommand is the ADVISORY
/// human-facing view (it prints findings and the JSON but does not fail); the
/// BLOCKING gate is [`check`], wired into the serial `structural-check` run path
/// (`structural.rs`) as `repo-ir-fitness`.
pub(crate) fn run(repo_root: &Path, out: Option<PathBuf>) -> Result<()> {
    let ir = build(repo_root)?;

    let owned = registered_fitnesses();
    let fitnesses: Vec<&dyn Fitness> = owned.iter().map(AsRef::as_ref).collect();
    let findings = run_fitness(&ir, &fitnesses);
    if findings.is_empty() {
        errln!(
            "repo-ir: {} fitness function(s) clean over {} fact families",
            fitnesses.len(),
            ALL_KINDS.len()
        );
    } else {
        for finding in &findings {
            errln!("repo-ir finding [{}]: {}", finding.fitness, finding.message);
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
            outln!("repo-ir: wrote {}", out.display());
        }
        None => out!("{rendered}"),
    }
    Ok(())
}

#[cfg(test)]
#[path = "repo_ir_tests.rs"]
mod repo_ir_tests;
