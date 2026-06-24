//! Assurance Levels (AL-DEF) loader + anti-drift lockstep.
//!
//! Loads `traceability/assurance_levels.yaml`, resolves any production source
//! path to its declared assurance level (default `L1`), and enforces the
//! anti-laundering lockstep: the set of globs declared at `L3` or `L4` must
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

use crate::repo_surface::{core_src_root, load_yaml, production_rust_roots, relative, rust_files};
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

/// The default level for any production file that matches no glob.
pub(crate) const DEFAULT_LEVEL: AssuranceLevel = AssuranceLevel::L1;

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
        "crates/syncbat/src/register_store.rs",
    ),
    // netbat-boundary-protocol (NETBAT_BOUNDARY_MUTANT_FILES)
    ("netbat-boundary-protocol", "crates/netbat/src/lib.rs"),
    ("netbat-boundary-protocol", "crates/netbat/src/route.rs"),
    ("netbat-boundary-protocol", "crates/netbat/src/transport.rs"),
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
/// matching level wins (so an L4 glob beats an overlapping L2 one); unmatched
/// files default to [`DEFAULT_LEVEL`] (`L1`).
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

    let missing_from_registry: Vec<(&str, &str)> = mirror_ref.difference(&registry_ref).copied().collect();
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

    let extra_in_registry: Vec<(&str, &str)> = registry_ref.difference(&mirror_ref).copied().collect();
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

/// Advisory (non-blocking): list production `.rs` files matched by no glob in
/// the manifest (i.e. files resolving to the default `L1`). Wired to print, not
/// fail — earns blocking authority later.
pub(crate) fn unleveled_files(repo_root: &Path, entries: &[AssuranceEntry]) -> Vec<String> {
    let mut paths = rust_files(&core_src_root(repo_root));
    for root in production_rust_roots(repo_root) {
        paths.extend(rust_files(&root));
    }
    let mut unleveled: Vec<String> = paths
        .iter()
        .map(|p| relative(repo_root, p))
        .filter(|rel| !entry_matches_path(entries, rel))
        .collect();
    unleveled.sort();
    unleveled.dedup();
    unleveled
}

/// Loads the manifest, runs the lockstep gate, and prints an advisory line for
/// unleveled production files. Called from `structural::run`.
pub(crate) fn check(repo_root: &Path) -> Result<()> {
    let entries = load_manifest(repo_root)?;
    check_seam_lockstep(&entries)?;
    let seam_registry = load_seam_registry(repo_root)?;
    check_seam_registry_lockstep(&seam_registry)?;

    let unleveled = unleveled_files(repo_root, &entries);
    if unleveled.is_empty() {
        outln!("assurance-level-check: ok (every production file resolves to a declared AL)");
    } else {
        outln!(
            "assurance-level-check: ok ({} unleveled production file(s) default to {} — advisory):",
            unleveled.len(),
            DEFAULT_LEVEL.as_str()
        );
        for rel in &unleveled {
            outln!("  - {rel} [{}]", resolve_level(&entries, rel).as_str());
        }
    }
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
                    entry
                        .globs
                        .retain(|glob| !glob.contains("control"));
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
