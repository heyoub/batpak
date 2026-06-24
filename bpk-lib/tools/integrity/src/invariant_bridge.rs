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

use super::anchors::{extract_anchors, load_known_invariants, resolve_anchor, JustifiesAnchor};

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
    "crates/bvisor/tests/",
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
    let mut all_citations = BTreeSet::new();
    for entry in crate::harness_lints::load_ledger_citations(repo_root)? {
        let citations = entry.invariants.iter().cloned().collect::<BTreeSet<_>>();
        all_citations.extend(citations.iter().cloned());
        check_ledger_entry_citations(
            Some((entry.title, entry.line)),
            &citations,
            known_invariants,
            waivers,
        )?;
    }
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
    let waiver_name = format!("testing_ledger.yaml:{line}:{title}");
    if waivers.contains(&waiver_name)? {
        return Ok(());
    }
    ensure(
        !citations.is_empty(),
        format!("testing_ledger.yaml:{line}: `{title}` must cite at least one catalog INV-* id"),
    )?;
    for citation in citations {
        ensure(
            known_invariants.contains(citation),
            format!("testing_ledger.yaml:{line}: `{title}` cites non-catalog invariant {citation}"),
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
    let waiver_names = citation_waivers.names();
    let escalate = uncovered
        .iter()
        .filter(|invariant| !ledger_citations.contains(*invariant))
        .filter(|invariant| !waives_invariant(&waiver_names, invariant))
        .cloned()
        .collect::<Vec<_>>();
    ensure(
        escalate.is_empty(),
        format!(
            "invariant-bridge: catalog invariants without direct test artifact, ledger entry, or waiver: {}",
            escalate.join(", ")
        ),
    )?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    // justifies: INV-TEST-PANIC-AS-ASSERTION; setup panics signal fixture breakage, see tools/integrity/src/main.rs
    fn temp_repo(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "batpak-invariant-bridge-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp repo");
        path
    }

    fn cleanup(repo: &Path) {
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn invariant_bridge_rejects_uncited_invariant() {
        // A test artifact that DOES cite the invariant in its header passes;
        // the same artifact stripped of the citation must make the
        // header-citation rule bail.
        let repo = temp_repo("uncited");
        let rel = "crates/core/tests/synthetic_bridge.rs";
        fs::create_dir_all(repo.join("crates/core/tests")).expect("tests dir");

        fs::write(
            repo.join(rel),
            "//! PROVES INV-SYNTHETIC-BRIDGE holds under replay.\nfn t() {}\n",
        )
        .expect("write cited test");
        invariant_test_artifacts_cite_header(&repo, "INV-SYNTHETIC-BRIDGE", &[rel.to_owned()])
            .expect("cited header passes");

        fs::write(
            repo.join(rel),
            "//! This header mentions no invariant id at all.\nfn t() {}\n",
        )
        .expect("write uncited test");
        let err =
            invariant_test_artifacts_cite_header(&repo, "INV-SYNTHETIC-BRIDGE", &[rel.to_owned()])
                .expect_err("uncited header must fail");
        assert!(
            err.to_string().contains("INV-SYNTHETIC-BRIDGE")
                && err.to_string().contains("none cite it"),
            "unexpected error: {err:#}"
        );
        cleanup(&repo);
    }

    #[test]
    fn invariant_bridge_rejects_noncatalog_ledger_citation() {
        // A ledger entry citing a known catalog INV passes; citing an INV that
        // is not in the catalog must make the ledger-citation rule bail.
        let known: BTreeSet<String> = BTreeSet::from(["INV-REAL".to_owned()]);
        let waivers = CitationWaivers::default();

        let good = BTreeSet::from(["INV-REAL".to_owned()]);
        check_ledger_entry_citations(Some(("good entry".to_owned(), 10)), &good, &known, &waivers)
            .expect("catalog citation passes");

        let bad = BTreeSet::from(["INV-DOES-NOT-EXIST".to_owned()]);
        let err = check_ledger_entry_citations(
            Some(("bad entry".to_owned(), 11)),
            &bad,
            &known,
            &waivers,
        )
        .expect_err("non-catalog citation must fail");
        assert!(
            err.to_string()
                .contains("non-catalog invariant INV-DOES-NOT-EXIST"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn invariant_bridge_rejects_empty_ledger_citation() {
        // An entry that cites at least one INV passes; an entry citing nothing
        // must bail ("must cite at least one catalog INV-* id").
        let known: BTreeSet<String> = BTreeSet::from(["INV-REAL".to_owned()]);
        let waivers = CitationWaivers::default();
        let empty = BTreeSet::new();
        let err =
            check_ledger_entry_citations(Some(("empty".to_owned(), 7)), &empty, &known, &waivers)
                .expect_err("empty citation set must fail");
        assert!(
            err.to_string().contains("must cite at least one"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn invariant_bridge_rejects_unresolvable_waiver_anchor() {
        // A waiver whose `adr` resolves to a real ADR loads cleanly; a waiver
        // whose `adr` carries no resolvable anchor must make load_waivers bail.
        let repo = temp_repo("waiver-anchor");
        // Provide an ADR so the green waiver's anchor resolves. resolve_anchor
        // searches the PROJECT root (repo_root.parent()), so place it there.
        let project = repo.join("project");
        let repo_root = project.join("bpk-lib");
        fs::create_dir_all(&repo_root).expect("repo root");
        fs::write(project.join("ADR-0001-some-decision.md"), "# ADR-0001\n").expect("adr file");

        let good = repo_root.join("good_waivers.yaml");
        fs::write(
            &good,
            "- name: INV-X:crates/core/tests/x.rs\n  justification: proven via fuzz harness\n  adr: ADR-0001\n  witness: tests/x.rs\n",
        )
        .expect("write good waiver");
        load_waivers(&good, &repo_root).expect("resolvable anchor passes");

        let bad = repo_root.join("bad_waivers.yaml");
        fs::write(
            &bad,
            "- name: INV-X:crates/core/tests/x.rs\n  justification: proven via fuzz harness\n  adr: no anchor here at all\n  witness: tests/x.rs\n",
        )
        .expect("write bad waiver");
        // `CitationWaivers` deliberately carries no `Debug`, so match the
        // result rather than `expect_err` (which would require `Debug`).
        let result = load_waivers(&bad, &repo_root);
        assert!(result.is_err(), "unresolvable anchor must fail");
        let Err(err) = result else {
            unreachable!("asserted is_err directly above")
        };
        assert!(
            err.to_string().contains("does not resolve to a real ADR"),
            "unexpected error: {err:#}"
        );
        cleanup(&repo);
    }

    #[test]
    fn invariant_bridge_rejects_witnessless_waiver() {
        // A waiver carrying a witness is honored by `contains`; an otherwise
        // valid waiver WITHOUT a witness must bail when consulted ("must carry
        // a witness while the bridge is active").
        let with_witness = WaiverRecord {
            name: "INV-X:crates/core/tests/x.rs".to_owned(),
            justification: "proven via fuzz harness".to_owned(),
            adr: "ADR-0001".to_owned(),
            witness: Some("tests/x.rs".to_owned()),
        };
        let mut entries = BTreeMap::new();
        entries.insert(with_witness.name.clone(), with_witness);
        let waivers = CitationWaivers { entries };
        assert!(
            waivers
                .contains("INV-X:crates/core/tests/x.rs")
                .expect("witnessed waiver consulted"),
            "witnessed waiver must be honored"
        );

        let no_witness = WaiverRecord {
            name: "INV-Y:crates/core/tests/y.rs".to_owned(),
            justification: "proven via fuzz harness".to_owned(),
            adr: "ADR-0001".to_owned(),
            witness: None,
        };
        let mut entries = BTreeMap::new();
        entries.insert(no_witness.name.clone(), no_witness);
        let waivers = CitationWaivers { entries };
        let err = waivers
            .contains("INV-Y:crates/core/tests/y.rs")
            .expect_err("witness-less waiver must fail");
        assert!(
            err.to_string().contains("must carry a witness"),
            "unexpected error: {err:#}"
        );
    }
}
