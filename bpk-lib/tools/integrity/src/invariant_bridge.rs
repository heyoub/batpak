//! Anchor extraction policy: any `INV-*` token inside a `### Invariant:`
//! block — including contrast mentions like "INV-FOO contrast with INV-BAR"
//! — counts as a citation. Intentionally permissive: prose alone can fulfill
//! citation without a `Catalog invariants:` bullet. Tightening requires a
//! deliberate doctrine change plus backfill.

use crate::repo_surface::{ensure, load_yaml, relative, resolve_repo_or_core_path};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::shared_checks::{
    extract_anchors, load_known_invariants, resolve_anchor, JustifiesAnchor,
};

#[derive(Debug, Deserialize)]
struct InvariantRecord {
    id: String,
    statement: String,
    artifacts: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ArtifactRecord {
    id: String,
    paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct WaiverRecord {
    name: String,
    justification: String,
    adr: String,
    #[serde(default)]
    witness: Option<String>,
}

#[derive(Default)]
struct CitationWaivers {
    entries: BTreeMap<String, WaiverRecord>,
}

pub(crate) const TESTED_CRATES: &[&str] = &[
    "crates/core/tests/",
    "crates/syncbat/tests/",
    "crates/netbat/tests/",
];

pub(crate) fn check(repo_root: &Path, tracked_files: &[PathBuf]) -> Result<()> {
    let trace_dir = repo_root.join("traceability");
    let invariants: Vec<InvariantRecord> =
        load_yaml(&trace_dir.join("invariants.yaml")).context("invariants")?;
    let artifacts: Vec<ArtifactRecord> =
        load_yaml(&trace_dir.join("artifacts.yaml")).context("artifacts")?;
    let known = load_known_invariants(repo_root).map_err(anyhow::Error::msg)?;
    check_catalog_statements(&invariants)?;
    let artifact_map = artifacts
        .iter()
        .map(|artifact| (artifact.id.as_str(), artifact))
        .collect::<BTreeMap<_, _>>();
    let citation_waivers = load_waivers(
        &trace_dir.join("invariant_citation_waivers.yaml"),
        repo_root,
    )?;
    let ledger_waivers = load_waivers(&trace_dir.join("ledger_prose_waivers.yaml"), repo_root)?;

    check_catalog_artifact_header_citations(
        repo_root,
        &invariants,
        &artifact_map,
        &citation_waivers,
    )?;
    check_doctrine_header_anchors(repo_root, tracked_files, &known)?;
    check_ledger_prose_citations(repo_root, &known, &ledger_waivers)?;
    check_catalog_test_coverage(
        repo_root,
        &invariants,
        &artifact_map,
        &known,
        &ledger_waivers,
        &citation_waivers,
    )?;
    Ok(())
}

pub(crate) fn invariant_test_artifacts_cite_header(
    repo_root: &Path,
    invariant_id: &str,
    artifact_paths: &[String],
) -> Result<()> {
    let mut saw_test_artifact = false;
    for artifact_path in artifact_paths {
        if !is_test_module_path(artifact_path) {
            continue;
        }
        let full = resolve_repo_or_core_path(repo_root, artifact_path);
        if !full.is_file() {
            continue;
        }
        saw_test_artifact = true;
        if test_header_cites(&full, invariant_id)? {
            return Ok(());
        }
    }
    ensure(
        !saw_test_artifact,
        format!(
            "invariant {invariant_id} has test artifacts but none cite it in the first 40 lines"
        ),
    )?;
    Ok(())
}

pub(crate) fn test_header_cites(path: &Path, invariant_id: &str) -> Result<bool> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(content
        .lines()
        .take(40)
        .any(|line| line.contains(invariant_id)))
}

pub(crate) fn validate_header_anchors(
    repo_root: &Path,
    path: &str,
    content: &str,
    known_invariants: &BTreeSet<String>,
) -> Result<()> {
    for (index, line) in content.lines().take(40).enumerate() {
        let trimmed = line.trim_start();
        if !(trimmed.starts_with("//!")
            && (trimmed.contains("PROVES:")
                || trimmed.contains("DEFENDS:")
                || trimmed.contains("INVARIANTS:")))
        {
            continue;
        }
        for anchor in extract_anchors(trimmed) {
            ensure(
                resolve_anchor(&anchor, repo_root, known_invariants),
                format!(
                    "{path}:{}: doctrine header anchor `{}` does not resolve to traceability catalog or ADR",
                    index + 1,
                    render_anchor(&anchor),
                ),
            )?;
        }
    }
    Ok(())
}

fn check_catalog_artifact_header_citations(
    repo_root: &Path,
    invariants: &[InvariantRecord],
    artifact_map: &BTreeMap<&str, &ArtifactRecord>,
    waivers: &CitationWaivers,
) -> Result<()> {
    for invariant in invariants {
        let mut saw_test_artifact = false;
        let mut cited = false;
        for artifact_id in &invariant.artifacts {
            let Some(artifact) = artifact_map.get(artifact_id.as_str()) else {
                continue;
            };
            for artifact_path in &artifact.paths {
                if !is_test_module_path(artifact_path) {
                    continue;
                }
                let full = resolve_repo_or_core_path(repo_root, artifact_path);
                if !full.is_file() {
                    continue;
                }
                saw_test_artifact = true;
                let waiver_name = format!("{}:{artifact_path}", invariant.id);
                if waivers.contains(&waiver_name)? {
                    cited = true;
                    continue;
                }
                cited |= test_header_cites(&full, &invariant.id)?;
            }
        }
        ensure(
            !saw_test_artifact || cited,
            format!(
                "invariant {} has test artifacts but none cite it in the first 40 lines",
                invariant.id
            ),
        )?;
    }
    Ok(())
}

fn check_catalog_statements(invariants: &[InvariantRecord]) -> Result<()> {
    for invariant in invariants {
        let statement = invariant.statement.trim();
        ensure(
            !statement.is_empty(),
            format!("invariant {} has empty statement", invariant.id),
        )?;
        let words = statement.split_whitespace().count();
        ensure(
            words >= 6,
            format!(
                "invariant {} statement is too short: {words} words, expected at least 6",
                invariant.id
            ),
        )?;
    }
    Ok(())
}

fn check_doctrine_header_anchors(
    repo_root: &Path,
    tracked_files: &[PathBuf],
    known_invariants: &BTreeSet<String>,
) -> Result<()> {
    for path in tracked_files {
        let rel = relative(repo_root, path);
        if !is_in_tested_crate(&rel) || !rel.ends_with(".rs") {
            continue;
        }
        let content = fs::read_to_string(path).with_context(|| format!("read {rel}"))?;
        validate_header_anchors(repo_root, &rel, &content, known_invariants)?;
    }
    Ok(())
}

fn check_ledger_prose_citations(
    repo_root: &Path,
    known_invariants: &BTreeSet<String>,
    waivers: &CitationWaivers,
) -> Result<()> {
    collect_ledger_citations(repo_root, known_invariants, waivers).map(|_| ())
}

fn collect_ledger_citations(
    repo_root: &Path,
    known_invariants: &BTreeSet<String>,
    waivers: &CitationWaivers,
) -> Result<BTreeSet<String>> {
    let path = repo_root
        .parent()
        .unwrap_or(repo_root)
        .join("041_TESTING_LEDGER.md");
    let content = fs::read_to_string(&path).context("read 041_TESTING_LEDGER.md")?;
    let mut current_title: Option<(String, usize)> = None;
    let mut current_citations = BTreeSet::new();
    let mut all_citations = BTreeSet::new();

    for (index, line) in content.lines().enumerate() {
        if let Some(title) = line.strip_prefix("### Invariant: ") {
            check_ledger_entry_citations(
                current_title.take(),
                &current_citations,
                known_invariants,
                waivers,
            )?;
            current_title = Some((title.trim().to_owned(), index + 1));
            current_citations.clear();
            continue;
        }
        if current_title.is_some() {
            for anchor in extract_anchors(line) {
                if let JustifiesAnchor::Invariant(id) = anchor {
                    all_citations.insert(id.clone());
                    current_citations.insert(id);
                }
            }
        }
    }
    check_ledger_entry_citations(current_title, &current_citations, known_invariants, waivers)?;
    Ok(all_citations)
}

fn check_ledger_entry_citations(
    current_title: Option<(String, usize)>,
    citations: &BTreeSet<String>,
    known_invariants: &BTreeSet<String>,
    waivers: &CitationWaivers,
) -> Result<()> {
    let Some((title, line)) = current_title else {
        return Ok(());
    };
    let waiver_name = format!("041_TESTING_LEDGER.md:{line}:{title}");
    if waivers.contains(&waiver_name)? {
        return Ok(());
    }
    ensure(
        !citations.is_empty(),
        format!("041_TESTING_LEDGER.md:{line}: `{title}` must cite at least one catalog INV-* id"),
    )?;
    for citation in citations {
        ensure(
            known_invariants.contains(citation),
            format!(
                "041_TESTING_LEDGER.md:{line}: `{title}` cites non-catalog invariant {citation}"
            ),
        )?;
    }
    Ok(())
}

fn check_catalog_test_coverage(
    repo_root: &Path,
    invariants: &[InvariantRecord],
    artifact_map: &BTreeMap<&str, &ArtifactRecord>,
    known_invariants: &BTreeSet<String>,
    ledger_waivers: &CitationWaivers,
    citation_waivers: &CitationWaivers,
) -> Result<()> {
    let ledger_citations = collect_ledger_citations(repo_root, known_invariants, ledger_waivers)?;
    let uncovered = invariants
        .iter()
        .filter(|invariant| !invariant_has_test_artifact(repo_root, invariant, artifact_map))
        .map(|invariant| invariant.id.clone())
        .collect::<Vec<_>>();
    if !uncovered.is_empty() {
        println!(
            "invariant-bridge: catalog invariants without direct test artifact: {}",
            uncovered.join(", ")
        );
    }
    let waiver_names = citation_waivers.names();
    let escalate = uncovered
        .iter()
        .filter(|invariant| !ledger_citations.contains(*invariant))
        .filter(|invariant| !waives_invariant(&waiver_names, invariant))
        .cloned()
        .collect::<Vec<_>>();
    if !escalate.is_empty() {
        println!(
            "invariant-bridge: ESCALATE — uncovered AND no ledger entry AND no waiver: {}",
            escalate.join(", ")
        );
    }
    Ok(())
}

fn invariant_has_test_artifact(
    repo_root: &Path,
    invariant: &InvariantRecord,
    artifact_map: &BTreeMap<&str, &ArtifactRecord>,
) -> bool {
    invariant.artifacts.iter().any(|artifact_id| {
        artifact_map
            .get(artifact_id.as_str())
            .is_some_and(|artifact| {
                artifact
                    .paths
                    .iter()
                    .any(|path| is_test_artifact(repo_root, path))
            })
    })
}

fn is_test_artifact(repo_root: &Path, path: &str) -> bool {
    if path.starts_with("crates/core/tests") {
        return true;
    }
    resolve_repo_or_core_path(repo_root, path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext == "rs")
        && path.contains("/tests/")
}

fn is_test_module_path(path: &str) -> bool {
    is_in_tested_crate(path)
        && path.ends_with(".rs")
        && !path.contains("/fixtures/")
        && !path.contains("/support/")
        && !path.contains("/ui/")
        && !path.contains("/golden/")
}

fn is_in_tested_crate(rel: &str) -> bool {
    TESTED_CRATES.iter().any(|prefix| rel.starts_with(prefix))
}

fn load_waivers(path: &Path, repo_root: &Path) -> Result<CitationWaivers> {
    if !path.exists() {
        return Ok(CitationWaivers::default());
    }
    let records: Vec<WaiverRecord> =
        load_yaml(path).with_context(|| format!("parse {}", path.display()))?;
    let mut entries = BTreeMap::new();
    for record in records {
        ensure(
            !record.name.trim().is_empty(),
            format!("{} contains waiver with blank name", path.display()),
        )?;
        ensure(
            !record.justification.trim().is_empty(),
            format!(
                "{} waiver {} missing justification",
                path.display(),
                record.name
            ),
        )?;
        ensure(
            !record.adr.trim().is_empty(),
            format!("{} waiver {} missing adr", path.display(), record.name),
        )?;
        let anchors = extract_anchors(&record.adr);
        ensure(
            !anchors.is_empty(),
            format!(
                "{} waiver {} adr `{}` does not resolve to a real ADR",
                path.display(),
                record.name,
                record.adr
            ),
        )?;
        for anchor in anchors {
            ensure(
                resolve_anchor(&anchor, repo_root, &BTreeSet::new()),
                format!(
                    "{} waiver {} adr `{}` does not resolve to a real ADR",
                    path.display(),
                    record.name,
                    record.adr
                ),
            )?;
        }
        ensure(
            entries.insert(record.name.clone(), record).is_none(),
            format!("{} contains duplicate waiver name", path.display()),
        )?;
    }
    Ok(CitationWaivers { entries })
}

impl CitationWaivers {
    fn contains(&self, name: &str) -> Result<bool> {
        let Some(record) = self.entries.get(name) else {
            return Ok(false);
        };
        ensure(
            record
                .witness
                .as_deref()
                .is_some_and(|witness| !witness.trim().is_empty()),
            format!("waiver {name} must carry a witness while the bridge is active"),
        )?;
        Ok(true)
    }

    fn names(&self) -> BTreeSet<String> {
        self.entries.keys().cloned().collect()
    }
}

fn waives_invariant(waiver_names: &BTreeSet<String>, invariant: &str) -> bool {
    let prefix = format!("{invariant}:");
    waiver_names
        .iter()
        .any(|name| name == invariant || name.starts_with(&prefix))
}

fn render_anchor(anchor: &JustifiesAnchor) -> String {
    match anchor {
        JustifiesAnchor::Invariant(id) => id.clone(),
        JustifiesAnchor::Adr(n) => format!("ADR-{n:04}"),
        JustifiesAnchor::Path(path) => path.to_string_lossy().into_owned(),
    }
}
