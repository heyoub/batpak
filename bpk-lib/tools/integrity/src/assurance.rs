//! Assurance Levels (AL-DEF) loader + anti-drift lockstep.
//!
//! Loads `traceability/assurance_levels.yaml`, resolves any production source
//! path to its declared assurance level (explicit manifest globs, else derived
//! `L1` from Cargo production roots), and enforces the anti-laundering lockstep: the set of globs declared at `L3` or `L4` must
//! equal — exactly — the set of globs covered by `critical_mutation_seams()`
//! in `bpk-lib/tools/xtask/src/commands/mutants/lanes.rs`, so a file's
//! assurance level and its mutation criticality cannot drift apart.
//!
//! `tools/integrity` cannot depend on `tools/xtask`, so the seam glob arrays
//! are mirrored here as [`CRITICAL_SEAM_MUTANT_GLOBS`]. The xtask side carries
//! the same lockstep discipline (`ci_mutation_seam_matrix_matches_registry`),
//! and this module's lockstep test fails the moment the manifest diverges from
//! the mirror — the mirror is the integrity-side single source for "which
//! globs are critical seams".

use crate::repo_surface::{load_yaml, production_rust_roots, relative, rust_files};
use anyhow::{bail, Result};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::Path;

/// The five assurance levels, ordered L0 (lowest) .. L4 (highest).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Deserialize)]
pub(crate) enum AssuranceLevel {
    L0,
    L1,
    L2,
    L3,
    L4,
}

impl AssuranceLevel {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            AssuranceLevel::L0 => "L0",
            AssuranceLevel::L1 => "L1",
            AssuranceLevel::L2 => "L2",
            AssuranceLevel::L3 => "L3",
            AssuranceLevel::L4 => "L4",
        }
    }
}

/// One manifest entry: a level applied to a set of path globs.
#[derive(Debug, Deserialize)]
pub(crate) struct AssuranceEntry {
    pub(crate) level: AssuranceLevel,
    /// Required on L3/L4 entries: the `critical_mutation_seams()` slug whose
    /// globs this entry mirrors.
    #[serde(default)]
    pub(crate) seam: Option<String>,
    #[serde(default)]
    pub(crate) globs: Vec<String>,
}

/// The default level for production files under a derived root with no manifest glob.
pub(crate) const DEFAULT_LEVEL: AssuranceLevel = AssuranceLevel::L1;

/// True when `rel` lies under any Cargo-derived production root.
pub(crate) fn matches_derived_production_root(root_rels: &[String], rel: &str) -> bool {
    root_rels
        .iter()
        .any(|root_rel| rel == root_rel || rel.starts_with(&format!("{root_rel}/")))
}

fn production_rel_paths(repo_root: &Path) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    for root in production_rust_roots(repo_root)? {
        paths.extend(rust_files(&root));
    }
    let mut rels: Vec<String> = paths.iter().map(|path| relative(repo_root, path)).collect();
    rels.sort();
    rels.dedup();
    Ok(rels)
}

fn production_root_rels(repo_root: &Path) -> Result<Vec<String>> {
    let mut root_rels: Vec<String> = production_rust_roots(repo_root)?
        .iter()
        .map(|root| relative(repo_root, root))
        .collect();
    root_rels.sort();
    root_rels.dedup();
    Ok(root_rels)
}

/// Mirror of the `*_MUTANT_FILES` glob arrays behind `critical_mutation_seams()`
/// (`bpk-lib/tools/xtask/src/commands/mutants/lanes.rs`). Each tuple is
/// `(seam_slug, glob)`. The lockstep test asserts the L3 ∪ L4 glob set in the
/// manifest equals the glob set here, and that every manifest L3/L4 `seam:`
/// names a slug present here.
pub(crate) const CRITICAL_SEAM_MUTANT_GLOBS: &[(&str, &str)] = &[
    // writer-commit (WRITER_COMMIT_MUTANT_FILES)
    ("writer-commit", "crates/core/src/store/write/**/*.rs"),
    (
        "writer-commit",
        "crates/core/src/store/write/control/**/*.rs",
    ),
    // cursor-delivery (CURSOR_MUTANT_FILES)
    (
        "cursor-delivery",
        "crates/core/src/store/delivery/cursor.rs",
    ),
    (
        "cursor-delivery",
        "crates/core/src/store/delivery/observation.rs",
    ),
    ("cursor-delivery", "crates/core/src/store/reactor_typed.rs"),
    // projection-flow (PROJECTION_MUTANT_FILES)
    (
        "projection-flow",
        "crates/core/src/store/projection/flow/**/*.rs",
    ),
    (
        "projection-flow",
        "crates/core/src/store/projection/registry.rs",
    ),
    ("projection-flow", "crates/core/src/store/projection/mod.rs"),
    (
        "projection-flow",
        "crates/core/src/store/projection/watch.rs",
    ),
    // segment-scan (SEGMENT_SCAN_MUTANT_FILES)
    ("segment-scan", "crates/core/src/store/segment/scan/**/*.rs"),
    // hash-chain-replay (HASH_CHAIN_REPLAY_MUTANT_FILES)
    (
        "hash-chain-replay",
        "crates/core/src/store/ancestry/by_hash.rs",
    ),
    (
        "hash-chain-replay",
        "crates/core/src/store/cold_start/rebuild.rs",
    ),
    ("hash-chain-replay", "crates/core/src/store/chain_walk.rs"),
    ("hash-chain-replay", "crates/core/src/store/read_walk.rs"),
    // frontier-wait-durable (FRONTIER_WAIT_MUTANT_FILES)
    (
        "frontier-wait-durable",
        "crates/core/src/store/write/writer.rs",
    ),
    // frontier-append-gate (FRONTIER_APPEND_GATE_MUTANT_FILES)
    ("frontier-append-gate", "crates/core/src/store/gate.rs"),
    // event-payload-registry-validator (EVENT_PAYLOAD_REGISTRY_MUTANT_FILES)
    (
        "event-payload-registry-validator",
        "crates/core/src/event/payload.rs",
    ),
    (
        "event-payload-registry-validator",
        "crates/core/src/store/config.rs",
    ),
    (
        "event-payload-registry-validator",
        "crates/core/src/store/mod.rs",
    ),
    // platform-backend (PLATFORM_BACKEND_MUTANT_FILES)
    ("platform-backend", "crates/core/src/store/platform/**/*.rs"),
    ("platform-backend", "crates/core/src/store/config.rs"),
    ("platform-backend", "crates/core/src/store/mod.rs"),
    // testing-ledger-structural-lint (TESTING_LEDGER_LINT_MUTANT_FILES)
    (
        "testing-ledger-structural-lint",
        "tools/integrity/src/harness_lints.rs",
    ),
    // syncbat-runtime-dispatch (SYNCBAT_RUNTIME_MUTANT_FILES)
    ("syncbat-runtime-dispatch", "crates/syncbat/src/builder.rs"),
    ("syncbat-runtime-dispatch", "crates/syncbat/src/core.rs"),
    ("syncbat-runtime-dispatch", "crates/syncbat/src/error.rs"),
    ("syncbat-runtime-dispatch", "crates/syncbat/src/handler.rs"),
    (
        "syncbat-runtime-dispatch",
        "crates/syncbat/src/operation.rs",
    ),
    ("syncbat-runtime-dispatch", "crates/syncbat/src/receipt.rs"),
    (
        "syncbat-runtime-dispatch",
        "crates/syncbat/src/store_sink.rs",
    ),
    // syncbat-register-catalog (SYNCBAT_CATALOG_MUTANT_FILES)
    ("syncbat-register-catalog", "crates/syncbat/src/register.rs"),
    (
        "syncbat-register-catalog",
        "crates/syncbat/src/register_store/**/*.rs",
    ),
    (
        "syncbat-subscription-runtime",
        "crates/syncbat/src/subscription_runtime/**/*.rs",
    ),
    (
        "syncbat-subscription-runtime",
        "crates/syncbat/src/operation_status.rs",
    ),
    (
        "syncbat-subscription-runtime",
        "crates/syncbat/src/operation_status_sink.rs",
    ),
    // netbat-boundary-protocol (NETBAT_BOUNDARY_MUTANT_FILES)
    ("netbat-boundary-protocol", "crates/netbat/src/lib.rs"),
    ("netbat-boundary-protocol", "crates/netbat/src/route.rs"),
    (
        "netbat-boundary-protocol",
        "crates/netbat/src/transport/**/*.rs",
    ),
    // fork-isolation (FORK_MUTANT_FILES)
    (
        "fork-isolation",
        "crates/core/src/store/file_classification.rs",
    ),
    ("fork-isolation", "crates/core/src/store/fork_report.rs"),
    // import-reapply (IMPORT_MUTANT_FILES)
    ("import-reapply", "crates/core/src/store/import.rs"),
];

pub(crate) fn manifest_path(repo_root: &Path) -> std::path::PathBuf {
    repo_root.join("traceability/assurance_levels.yaml")
}

pub(crate) fn seam_registry_path(repo_root: &Path) -> std::path::PathBuf {
    repo_root.join("traceability/seam_registry.yaml")
}

/// One row of `traceability/seam_registry.yaml` — authoritative seam slug → globs
/// map consumed by lockstep against [`CRITICAL_SEAM_MUTANT_GLOBS`].
#[derive(Debug, Deserialize)]
pub(crate) struct SeamRegistryEntry {
    pub(crate) slug: String,
    pub(crate) assurance_level: String,
    #[serde(default)]
    pub(crate) dst_coverage: bool,
    pub(crate) globs: Vec<String>,
}

/// Load the assurance manifest entries from `traceability/assurance_levels.yaml`.
pub(crate) fn load_manifest(repo_root: &Path) -> Result<Vec<AssuranceEntry>> {
    load_yaml(&manifest_path(repo_root))
}

/// Load the seam registry from `traceability/seam_registry.yaml`.
pub(crate) fn load_seam_registry(repo_root: &Path) -> Result<Vec<SeamRegistryEntry>> {
    load_yaml(&seam_registry_path(repo_root))
}

/// True when `rel` (a repo-root-relative, forward-slash path) is matched by the
/// `**`/`*` glob `pattern`. Supports the `crates/.../**/*.rs` and trailing `*`
/// forms used by the seam globs; the `**` segment matches any number of path
/// components (including zero).
pub(crate) fn glob_matches(pattern: &str, rel: &str) -> bool {
    glob_match_parts(
        &pattern.split('/').collect::<Vec<_>>(),
        &rel.split('/').collect::<Vec<_>>(),
    )
}

fn glob_match_parts(pat: &[&str], path: &[&str]) -> bool {
    match pat.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => {
            // `**` matches zero or more path components.
            (0..=path.len()).any(|skip| glob_match_parts(rest, &path[skip..]))
        }
        Some((head, rest)) => match path.split_first() {
            Some((seg, path_rest)) if segment_matches(head, seg) => {
                glob_match_parts(rest, path_rest)
            }
            _ => false,
        },
    }
}

/// Single-component match supporting a leading/trailing `*` wildcard (e.g.
/// `*.rs`). A bare `*` matches any single component.
fn segment_matches(pattern: &str, segment: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    match pattern.split_once('*') {
        None => pattern == segment,
        Some((prefix, suffix)) => {
            segment.len() >= prefix.len() + suffix.len()
                && segment.starts_with(prefix)
                && segment.ends_with(suffix)
        }
    }
}

/// True when any manifest entry glob matches `rel`.
pub(crate) fn entry_matches_path(entries: &[AssuranceEntry], rel: &str) -> bool {
    entries
        .iter()
        .any(|e| e.globs.iter().any(|g| glob_matches(g, rel)))
}

/// Resolve a repo-root-relative source path to its assurance level. The highest
/// matching manifest glob wins (so an L4 glob beats an overlapping L2 one).
/// Files under a Cargo production root with no manifest match resolve to
/// [`DEFAULT_LEVEL`] (`L1`) via derivation, not an invisible default.
pub(crate) fn resolve_level(entries: &[AssuranceEntry], rel: &str) -> AssuranceLevel {
    let mut best: Option<AssuranceLevel> = None;
    for entry in entries {
        if entry.globs.iter().any(|g| glob_matches(g, rel)) {
            best = Some(best.map_or(entry.level, |b| b.max(entry.level)));
        }
    }
    best.unwrap_or(DEFAULT_LEVEL)
}

/// Anti-drift lockstep: the L3 ∪ L4 glob set in the manifest must equal the
/// critical-seam glob set ([`CRITICAL_SEAM_MUTANT_GLOBS`]) exactly, and every
/// L3/L4 entry's `seam:` must name a real seam slug. Returns `Err` on any
/// drift so assurance level and mutation criticality cannot diverge.
pub(crate) fn check_seam_lockstep(entries: &[AssuranceEntry]) -> Result<()> {
    let seam_slugs: BTreeSet<&str> = CRITICAL_SEAM_MUTANT_GLOBS
        .iter()
        .map(|(slug, _)| *slug)
        .collect();
    let seam_globs: BTreeSet<&str> = CRITICAL_SEAM_MUTANT_GLOBS
        .iter()
        .map(|(_, glob)| *glob)
        .collect();

    let mut manifest_globs: BTreeSet<String> = BTreeSet::new();
    for entry in entries {
        if !matches!(entry.level, AssuranceLevel::L3 | AssuranceLevel::L4) {
            continue;
        }
        match &entry.seam {
            None => bail!(
                "assurance-level-check: {} entry has no `seam:` — every L3/L4 entry must name a \
                 critical_mutation_seams() slug so AL and mutation criticality stay in lockstep.",
                entry.level.as_str()
            ),
            Some(seam) if !seam_slugs.contains(seam.as_str()) => bail!(
                "assurance-level-check: {} entry names seam `{seam}`, which is not a \
                 critical_mutation_seams() slug. Known seams: {}.",
                entry.level.as_str(),
                seam_slugs.iter().copied().collect::<Vec<_>>().join(", ")
            ),
            Some(_) => {}
        }
        for glob in &entry.globs {
            manifest_globs.insert(glob.clone());
        }
    }

    let manifest_ref: BTreeSet<&str> = manifest_globs.iter().map(String::as_str).collect();

    let missing_from_manifest: Vec<&str> = seam_globs.difference(&manifest_ref).copied().collect();
    if !missing_from_manifest.is_empty() {
        bail!(
            "assurance-level-check: critical-seam glob(s) absent from any L3/L4 manifest entry: {}.\n\
             Every critical_mutation_seams() glob must be declared L3 or L4 in \
             traceability/assurance_levels.yaml.",
            missing_from_manifest.join(", ")
        );
    }

    let extra_in_manifest: Vec<&str> = manifest_ref.difference(&seam_globs).copied().collect();
    if !extra_in_manifest.is_empty() {
        bail!(
            "assurance-level-check: L3/L4 manifest glob(s) not backed by any critical seam: {}.\n\
             An L3/L4 glob must be mirrored from a `*_MUTANT_FILES` array \
             (bpk-lib/tools/xtask/src/commands/mutants/lanes.rs).",
            extra_in_manifest.join(", ")
        );
    }

    Ok(())
}

/// Lockstep: every `(slug, glob)` in `traceability/seam_registry.yaml` must equal
/// the [`CRITICAL_SEAM_MUTANT_GLOBS`] mirror exactly — no drift between the
/// authoritative YAML and the integrity-side seam map.
pub(crate) fn check_seam_registry_lockstep(entries: &[SeamRegistryEntry]) -> Result<()> {
    let mut registry_pairs: BTreeSet<(String, String)> = BTreeSet::new();
    for entry in entries {
        if entry.slug.is_empty() {
            bail!("seam-registry-check: entry has empty slug");
        }
        if entry.assurance_level.is_empty() {
            bail!(
                "seam-registry-check: seam `{}` has empty assurance_level",
                entry.slug
            );
        }
        if entry.globs.is_empty() {
            bail!(
                "seam-registry-check: seam `{}` must declare at least one glob",
                entry.slug
            );
        }
        // A seam that claims DST corpus coverage must carry a real assurance
        // level (never the bottom `L0`): the flag asserts a graduated-seed proof
        // exists, so it cannot ride on an unassured seam. Reading the flag here
        // keeps the schema field load-bearing.
        if entry.dst_coverage && entry.assurance_level == "L0" {
            bail!(
                "seam-registry-check: seam `{}` declares dst_coverage but is assurance_level L0; \
                 DST corpus coverage requires a real (non-L0) assurance level",
                entry.slug
            );
        }
        for glob in &entry.globs {
            registry_pairs.insert((entry.slug.clone(), glob.clone()));
        }
    }

    let mirror_pairs: BTreeSet<(String, String)> = CRITICAL_SEAM_MUTANT_GLOBS
        .iter()
        .map(|(slug, glob)| ((*slug).to_owned(), (*glob).to_owned()))
        .collect();

    let registry_ref: BTreeSet<(&str, &str)> = registry_pairs
        .iter()
        .map(|(s, g)| (s.as_str(), g.as_str()))
        .collect();
    let mirror_ref: BTreeSet<(&str, &str)> = mirror_pairs
        .iter()
        .map(|(s, g)| (s.as_str(), g.as_str()))
        .collect();

    let missing_from_registry: Vec<(&str, &str)> =
        mirror_ref.difference(&registry_ref).copied().collect();
    if !missing_from_registry.is_empty() {
        bail!(
            "seam-registry-check: critical-seam pair(s) absent from seam_registry.yaml: {}.",
            missing_from_registry
                .iter()
                .map(|(s, g)| format!("{s} -> {g}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let extra_in_registry: Vec<(&str, &str)> =
        registry_ref.difference(&mirror_ref).copied().collect();
    if !extra_in_registry.is_empty() {
        bail!(
            "seam-registry-check: seam_registry.yaml pair(s) not backed by CRITICAL_SEAM_MUTANT_GLOBS: {}.",
            extra_in_registry
                .iter()
                .map(|(s, g)| format!("{s} -> {g}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    Ok(())
}

/// List production `.rs` files matched by no manifest glob and not under any
/// derived production root. Any non-empty result is a hard failure: production
/// code must be explicitly classified (manifest glob or derived L1 root).
///
/// MUTATION MAP for the two `!` terms in the filter below:
/// * `delete !` on `!matches_derived_production_root` (trailing term) IS
///   observable: every `rel` from `production_rel_paths` lies under a production
///   root, so `matches_derived` is always true; dropping its `!` turns the
///   conjunction into `!entry_matches_path && true` (the derived-L1 set), which is
///   non-empty on the committed tree, so `check()` fails closed. Killed by
///   `committed_manifest_full_check_is_green` and the direct
///   `unleveled_files_is_empty_over_committed_manifest`.
/// * `delete !` on `!entry_matches_path` (leading term) is EQUIVALENT: because
///   `matches_derived` is always true, `!matches_derived` is unconditionally
///   false, so `(..) && false` is empty no matter what the first term yields.
///   `production_rel_paths` and `production_root_rels` both enumerate the same
///   `production_rust_roots`, and every walked file is structurally under one of
///   those roots for ANY `repo_root` (not just the committed tree) — so no input
///   can make `matches_derived` false. The `-> Ok(vec![])` body mutant IS killed
///   (it strips the `?` error path that `root_resolution_failure_propagates` asserts).
pub(crate) fn unleveled_files(repo_root: &Path, entries: &[AssuranceEntry]) -> Result<Vec<String>> {
    let root_rels = production_root_rels(repo_root)?;
    let mut unleveled: Vec<String> = production_rel_paths(repo_root)?
        .into_iter()
        .filter(|rel| {
            !entry_matches_path(entries, rel) && !matches_derived_production_root(&root_rels, rel)
        })
        .collect();
    unleveled.sort();
    unleveled.dedup();
    Ok(unleveled)
}

/// Production files with no manifest glob that resolve via derived L1 roots.
pub(crate) fn derived_l1_files(
    repo_root: &Path,
    entries: &[AssuranceEntry],
) -> Result<Vec<String>> {
    let root_rels = production_root_rels(repo_root)?;
    let mut derived: Vec<String> = production_rel_paths(repo_root)?
        .into_iter()
        .filter(|rel| {
            !entry_matches_path(entries, rel) && matches_derived_production_root(&root_rels, rel)
        })
        .collect();
    derived.sort();
    derived.dedup();
    Ok(derived)
}

/// Count production files matched by at least one manifest glob.
pub(crate) fn declared_files(repo_root: &Path, entries: &[AssuranceEntry]) -> Result<usize> {
    Ok(production_rel_paths(repo_root)?
        .into_iter()
        .filter(|rel| entry_matches_path(entries, rel))
        .count())
}

/// Loads the manifest, runs the lockstep gate, and fails closed on unleveled
/// production files. Called from `structural::run`.
pub(crate) fn check(repo_root: &Path) -> Result<()> {
    let entries = load_manifest(repo_root)?;
    check_seam_lockstep(&entries)?;
    let seam_registry = load_seam_registry(repo_root)?;
    check_seam_registry_lockstep(&seam_registry)?;

    let unleveled = unleveled_files(repo_root, &entries)?;
    if !unleveled.is_empty() {
        bail!(
            "assurance-level-check: {} production file(s) match no manifest glob and no derived \
             production root — add an explicit glob or fix the production-root derivation:\n{}",
            unleveled.len(),
            unleveled
                .iter()
                .map(|rel| format!("  - {rel}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    let declared = declared_files(repo_root, &entries)?;
    let derived = derived_l1_files(repo_root, &entries)?;
    outln!(
        "assurance-level-check: ok ({} declared, {} derived {}, 0 unleveled)",
        declared,
        derived.len(),
        DEFAULT_LEVEL.as_str()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo_surface::repo_root;

    fn repo() -> std::path::PathBuf {
        repo_root().expect("repo root resolves from tools/integrity")
    }

    #[test]
    fn glob_matches_double_star_and_suffix() {
        assert!(glob_matches(
            "crates/core/src/store/write/**/*.rs",
            "crates/core/src/store/write/control/submission.rs"
        ));
        assert!(glob_matches(
            "crates/core/src/store/write/**/*.rs",
            "crates/core/src/store/write/writer.rs"
        ));
        assert!(glob_matches(
            "crates/core/src/store/gate.rs",
            "crates/core/src/store/gate.rs"
        ));
        assert!(!glob_matches(
            "crates/core/src/store/write/**/*.rs",
            "crates/core/src/store/read_walk.rs"
        ));
    }

    #[test]
    fn resolve_level_picks_highest_match_and_defaults_l1() {
        let entries = vec![
            AssuranceEntry {
                level: AssuranceLevel::L2,
                seam: None,
                globs: vec!["crates/core/src/store/config.rs".to_owned()],
            },
            AssuranceEntry {
                level: AssuranceLevel::L4,
                seam: Some("platform-backend".to_owned()),
                globs: vec!["crates/core/src/store/config.rs".to_owned()],
            },
        ];
        // Overlapping globs: the higher level wins.
        assert_eq!(
            resolve_level(&entries, "crates/core/src/store/config.rs"),
            AssuranceLevel::L4
        );
        // Unmatched file defaults to L1.
        assert_eq!(
            resolve_level(&entries, "crates/core/src/lib.rs"),
            AssuranceLevel::L1
        );
    }

    // GREEN: the committed manifest passes the lockstep against the live mirror.
    #[test]
    fn derived_production_root_matches_src_files() {
        let root_rels = vec!["crates/syncbat/src".to_owned()];
        assert!(matches_derived_production_root(
            &root_rels,
            "crates/syncbat/src/subscription_runtime/event_stream.rs"
        ));
        assert!(!matches_derived_production_root(
            &root_rels,
            "crates/syncbat/tests/runtime.rs"
        ));
    }

    #[test]
    fn committed_manifest_passes_seam_lockstep() {
        let entries = load_manifest(&repo()).expect("load assurance manifest");
        check_seam_lockstep(&entries).expect("committed manifest must pass the seam lockstep");
    }

    #[test]
    fn committed_seam_registry_passes_lockstep() {
        let registry = load_seam_registry(&repo()).expect("load seam registry");
        check_seam_registry_lockstep(&registry)
            .expect("committed seam_registry.yaml must pass lockstep");
    }

    #[test]
    fn missing_seam_registry_glob_fails_lockstep() {
        let registry = load_seam_registry(&repo()).expect("load seam registry");
        let trimmed: Vec<SeamRegistryEntry> = registry
            .into_iter()
            .map(|mut entry| {
                if entry.slug == "writer-commit" {
                    entry.globs.retain(|glob| !glob.contains("control"));
                }
                entry
            })
            .collect();
        let err = check_seam_registry_lockstep(&trimmed)
            .expect_err("dropping a seam glob must fail seam registry lockstep");
        assert!(
            err.to_string().contains("writer-commit"),
            "error must name the dropped seam, got: {err}"
        );
    }

    #[test]
    fn committed_manifest_full_check_is_green() {
        check(&repo()).expect("assurance::check must be green on the clean tree");
    }

    // The committed production surface is fully classified: every file is either
    // manifest-matched or under a derived production root, so `unleveled_files`
    // is empty — while `derived_l1_files` is NON-empty. Dropping the `!` on the
    // `matches_derived_production_root` term turns `unleveled` INTO the derived
    // set, so this directly kills that mutant (and the emptiness is non-vacuous
    // because the derived set is non-empty).
    #[test]
    fn unleveled_files_is_empty_over_committed_manifest() {
        let repo = repo();
        let entries = load_manifest(&repo).expect("load manifest");
        let unleveled = unleveled_files(&repo, &entries).expect("compute unleveled files");
        assert!(
            unleveled.is_empty(),
            "every production file must be classified (manifest glob or derived L1): {unleveled:?}"
        );
        let derived = derived_l1_files(&repo, &entries).expect("compute derived files");
        assert!(
            !derived.is_empty(),
            "the derived-L1 set must be non-empty so the unleveled emptiness is non-vacuous"
        );
    }

    #[test]
    fn production_rel_paths_lists_real_sources() {
        let paths = production_rel_paths(&repo()).expect("enumerate production rel paths");
        assert!(!paths.is_empty(), "production surface must be non-empty");
        assert!(
            paths.iter().any(|p| p == "crates/core/src/lib.rs"),
            "must include the core lib root: {paths:?}"
        );
    }

    #[test]
    fn derived_l1_files_are_unmatched_production_files() {
        let repo = repo();
        // With NO manifest entries, every production file is derived-L1.
        let all_derived = derived_l1_files(&repo, &[]).expect("derive with empty manifest");
        assert!(all_derived.len() > 1, "empty manifest derives many files");
        assert!(
            all_derived.iter().any(|p| p == "crates/core/src/lib.rs"),
            "an unmatched production file must be derived: {all_derived:?}"
        );
        assert!(
            !all_derived.iter().any(|p| p == "xyzzy" || p.is_empty()),
            "the derived list must be real paths, not placeholders: {all_derived:?}"
        );

        // A manifest-declared L4 seam file is NOT derived (it matched a glob, so
        // `!entry_matches_path && matches_derived` excludes it; the `||` mutant
        // would re-include it).
        let entries = load_manifest(&repo).expect("load manifest");
        let derived = derived_l1_files(&repo, &entries).expect("derive with real manifest");
        assert!(
            !derived.iter().any(|p| p == "crates/core/src/store/gate.rs"),
            "a manifest-declared seam file must be excluded from derived-L1: {derived:?}"
        );
    }

    #[test]
    fn declared_files_counts_manifest_matches() {
        let repo = repo();
        let entries = load_manifest(&repo).expect("load manifest");
        let declared = declared_files(&repo, &entries).expect("count declared files");
        assert!(
            declared > 1,
            "the committed manifest declares many files, got {declared}"
        );
    }

    #[test]
    fn root_resolution_failure_propagates() {
        // A root with no Cargo workspace / manifest makes the production-root
        // derivation and manifest load error; both entry points must surface it.
        let bogus = std::env::temp_dir().join(format!(
            "batpak-assurance-bogus-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        assert!(
            unleveled_files(&bogus, &[]).is_err(),
            "unleveled_files must surface a bad production root"
        );
        assert!(
            check(&bogus).is_err(),
            "check must surface a missing manifest"
        );
    }

    // RED fixture (a): a seam file dropped from every L3/L4 glob → lockstep Err.
    // Model: the manifest omits the `frontier-append-gate` glob entirely, so a
    // critical seam file is no longer covered by any L3/L4 entry.
    #[test]
    fn missing_seam_glob_fails_lockstep() {
        let entries = vec![full_l4_entry_except(&["crates/core/src/store/gate.rs"])];
        let err = check_seam_lockstep(&entries)
            .expect_err("a seam glob missing from L3/L4 must fail the lockstep");
        assert!(
            err.to_string().contains("crates/core/src/store/gate.rs"),
            "error must name the dropped seam glob, got: {err}"
        );
    }

    // RED fixture (b): an L3/L4 glob not backed by any critical seam → Err.
    #[test]
    fn extra_unbacked_glob_fails_lockstep() {
        let mut globs: Vec<String> = CRITICAL_SEAM_MUTANT_GLOBS
            .iter()
            .map(|(_, g)| (*g).to_owned())
            .collect();
        globs.push("crates/core/src/store/NOT_A_SEAM.rs".to_owned());
        let entries = vec![AssuranceEntry {
            level: AssuranceLevel::L4,
            seam: Some("hash-chain-replay".to_owned()),
            globs,
        }];
        let err = check_seam_lockstep(&entries)
            .expect_err("an L3/L4 glob with no backing seam must fail the lockstep");
        assert!(
            err.to_string().contains("NOT_A_SEAM"),
            "error must name the unbacked glob, got: {err}"
        );
    }

    // RED fixture (c): an L3/L4 entry naming a non-existent seam slug → Err.
    #[test]
    fn unknown_seam_slug_fails_lockstep() {
        let entries = vec![AssuranceEntry {
            level: AssuranceLevel::L4,
            seam: Some("seam-that-does-not-exist".to_owned()),
            globs: vec!["crates/core/src/store/gate.rs".to_owned()],
        }];
        let err =
            check_seam_lockstep(&entries).expect_err("an unknown seam slug must fail the lockstep");
        assert!(
            err.to_string().contains("seam-that-does-not-exist"),
            "error must name the bad slug, got: {err}"
        );
    }

    // Build one L4 entry carrying every seam glob EXCEPT those in `drop`, so a
    // single seam file can be surgically removed from coverage.
    fn full_l4_entry_except(drop: &[&str]) -> AssuranceEntry {
        let globs = CRITICAL_SEAM_MUTANT_GLOBS
            .iter()
            .map(|(_, g)| *g)
            .filter(|g| !drop.contains(g))
            .map(str::to_owned)
            .collect();
        AssuranceEntry {
            level: AssuranceLevel::L4,
            seam: Some("hash-chain-replay".to_owned()),
            globs,
        }
    }
}
