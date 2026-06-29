use crate::invariant_bridge;
use crate::repo_surface::{
    core_examples_root, ensure, ensure_unique_ids, load_yaml, relative, repo_root,
    resolve_repo_or_core_path,
};
use crate::source_cache::SourceCache;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::Path;
use syn::visit::Visit;

#[derive(Debug, Deserialize)]
struct RequirementRecord {
    id: String,
    summary: String,
    artifacts: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct InvariantRecord {
    id: String,
    statement: String,
    artifacts: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct FlowRecord {
    id: String,
    summary: String,
    artifacts: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ObservationRecord {
    id: String,
    status: String,
    statement: String,
    evidence: Vec<String>,
    #[serde(default)]
    artifacts: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ArtifactRecord {
    id: String,
    kind: String,
    paths: Vec<String>,
}

pub(crate) fn run() -> Result<()> {
    let repo_root = repo_root()?;
    let trace_dir = repo_root.join("traceability");
    let requirements: Vec<RequirementRecord> =
        load_yaml(&trace_dir.join("requirements.yaml")).context("requirements")?;
    let invariants: Vec<InvariantRecord> =
        load_yaml(&trace_dir.join("invariants.yaml")).context("invariants")?;
    let flows: Vec<FlowRecord> = load_yaml(&trace_dir.join("flows.yaml")).context("flows")?;
    let observations: Vec<ObservationRecord> =
        load_yaml(&trace_dir.join("observations.yaml")).context("observations")?;
    let artifacts: Vec<ArtifactRecord> =
        load_yaml(&trace_dir.join("artifacts.yaml")).context("artifacts")?;
    let mut source_cache = SourceCache::new(&repo_root);

    ensure_unique_ids(
        requirements.iter().map(|r| r.id.as_str()),
        "duplicate requirement id",
    )?;
    ensure_unique_ids(
        invariants.iter().map(|i| i.id.as_str()),
        "duplicate invariant id",
    )?;
    ensure_unique_ids(flows.iter().map(|f| f.id.as_str()), "duplicate flow id")?;
    ensure_unique_ids(
        observations.iter().map(|o| o.id.as_str()),
        "duplicate observation id",
    )?;
    ensure_unique_ids(
        artifacts.iter().map(|a| a.id.as_str()),
        "duplicate artifact id",
    )?;

    let artifact_map: HashMap<&str, &ArtifactRecord> = artifacts
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect();
    let mut referenced_artifacts = BTreeSet::new();
    for requirement in &requirements {
        ensure(
            !requirement.summary.trim().is_empty(),
            format!("requirement {} missing summary", requirement.id),
        )?;
        ensure(
            !requirement.artifacts.is_empty(),
            format!("requirement {} must reference artifacts", requirement.id),
        )?;
        for artifact_id in &requirement.artifacts {
            ensure(
                artifact_map.contains_key(artifact_id.as_str()),
                format!(
                    "requirement {} references missing artifact {}",
                    requirement.id, artifact_id
                ),
            )?;
            referenced_artifacts.insert(artifact_id.as_str());
        }
    }

    for invariant in &invariants {
        ensure(
            !invariant.statement.trim().is_empty(),
            format!("invariant {} missing statement", invariant.id),
        )?;
        ensure(
            !invariant.artifacts.is_empty(),
            format!("invariant {} must reference artifacts", invariant.id),
        )?;
        for artifact_id in &invariant.artifacts {
            ensure(
                artifact_map.contains_key(artifact_id.as_str()),
                format!(
                    "invariant {} references missing artifact {}",
                    invariant.id, artifact_id
                ),
            )?;
            referenced_artifacts.insert(artifact_id.as_str());
        }
        let artifact_paths = invariant
            .artifacts
            .iter()
            .filter_map(|artifact_id| artifact_map.get(artifact_id.as_str()))
            .flat_map(|artifact| artifact.paths.iter().cloned())
            .collect::<Vec<_>>();
        invariant_bridge::invariant_test_artifacts_cite_header(
            &repo_root,
            &invariant.id,
            &artifact_paths,
        )?;
    }

    for flow in &flows {
        ensure(
            !flow.summary.trim().is_empty(),
            format!("flow {} missing summary", flow.id),
        )?;
        ensure(
            !flow.artifacts.is_empty(),
            format!("flow {} must reference artifacts", flow.id),
        )?;
        for artifact_id in &flow.artifacts {
            ensure(
                artifact_map.contains_key(artifact_id.as_str()),
                format!(
                    "flow {} references missing artifact {}",
                    flow.id, artifact_id
                ),
            )?;
            referenced_artifacts.insert(artifact_id.as_str());
        }
    }

    for observation in &observations {
        ensure(
            !observation.status.trim().is_empty(),
            format!("observation {} missing status", observation.id),
        )?;
        ensure(
            !observation.statement.trim().is_empty(),
            format!("observation {} missing statement", observation.id),
        )?;
        ensure(
            !observation.evidence.is_empty(),
            format!("observation {} must reference evidence", observation.id),
        )?;
        for evidence in &observation.evidence {
            ensure(
                !evidence.trim().is_empty(),
                format!("observation {} has blank evidence", observation.id),
            )?;
            validate_observation_evidence_with_cache(
                &repo_root,
                &observation.id,
                evidence,
                &mut source_cache,
            )?;
        }
        for artifact_id in &observation.artifacts {
            ensure(
                artifact_map.contains_key(artifact_id.as_str()),
                format!(
                    "observation {} references missing artifact {}",
                    observation.id, artifact_id
                ),
            )?;
            referenced_artifacts.insert(artifact_id.as_str());
        }
    }

    for artifact in &artifacts {
        ensure(
            !artifact.kind.trim().is_empty(),
            format!("artifact {} missing kind", artifact.id),
        )?;
        ensure(
            !artifact.paths.is_empty(),
            format!("artifact {} must declare paths", artifact.id),
        )?;
        for path in &artifact.paths {
            let full = resolve_repo_or_core_path(&repo_root, path);
            ensure(
                full.exists(),
                format!("artifact path missing: {}", full.display()),
            )?;
        }
        ensure(
            referenced_artifacts.contains(artifact.id.as_str()),
            format!(
                "artifact {} is orphaned from requirements/invariants/flows",
                artifact.id
            ),
        )?;
    }
    check_examples_artifact_complete(&repo_root, &artifacts)?;
    check_concept_canonical(&repo_root, &artifacts)?;
    crate::model_bindings::check(&repo_root)?;

    outln!("traceability-check: ok");
    Ok(())
}

#[derive(Debug, Deserialize)]
struct ConceptRow {
    concept_id: String,
    canonical_example: String,
    summary: String,
    #[serde(default)]
    example_family: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConceptCatalog {
    concepts: Vec<ConceptRow>,
}

/// `concept-canonical` gate (#68): the concept catalog maps each concept to ONE
/// canonical runnable example. Enforces (1) every `canonical_example` is a real
/// file listed in ART-EXAMPLES, (2) no duplicate `concept_id`, (3) no two rows
/// share a `canonical_example`, (4) every runnable example in ART-EXAMPLES is the
/// canonical example of exactly one concept (completeness — a NEW example cannot
/// escape the catalog, and a retired one cannot dangle). This makes example
/// duplication mechanically visible and BLOCKS a second example for an existing
/// concept. Split out so a RED fixture can drive the pure check.
fn check_concept_canonical(repo_root: &Path, artifacts: &[ArtifactRecord]) -> Result<()> {
    let catalog: ConceptCatalog = load_yaml(&repo_root.join("traceability/concept_catalog.yaml"))
        .context("concept catalog")?;
    let examples_artifact = artifacts
        .iter()
        .find(|artifact| artifact.id == "ART-EXAMPLES")
        .context("ART-EXAMPLES artifact must exist")?;
    let registered: BTreeSet<&str> = examples_artifact
        .paths
        .iter()
        .filter(|path| path.starts_with("crates/batpak-examples/src/bin/") && path.ends_with(".rs"))
        .map(String::as_str)
        .collect();
    check_concept_canonical_over(repo_root, &catalog, &registered)
}

fn check_concept_canonical_over(
    repo_root: &Path,
    catalog: &ConceptCatalog,
    registered: &BTreeSet<&str>,
) -> Result<()> {
    ensure(
        !catalog.concepts.is_empty(),
        "concept-canonical: concept_catalog.yaml lists no concepts (vacuous catalog)".to_owned(),
    )?;

    let mut seen_concepts: BTreeSet<&str> = BTreeSet::new();
    let mut seen_examples: BTreeSet<&str> = BTreeSet::new();
    for row in &catalog.concepts {
        ensure(
            !row.summary.trim().is_empty(),
            format!(
                "concept-canonical: concept `{}` has a blank summary",
                row.concept_id
            ),
        )?;
        ensure(
            seen_concepts.insert(row.concept_id.as_str()),
            format!(
                "concept-canonical: duplicate concept_id `{}` — one canonical per concept",
                row.concept_id
            ),
        )?;
        ensure(
            seen_examples.insert(row.canonical_example.as_str()),
            format!(
                "concept-canonical: example `{}` is canonical for more than one concept",
                row.canonical_example
            ),
        )?;
        // The canonical example must be a real file.
        ensure(
            resolve_repo_or_core_path(repo_root, &row.canonical_example).is_file(),
            format!(
                "concept-canonical: concept `{}` canonical_example `{}` does not exist",
                row.concept_id, row.canonical_example
            ),
        )?;
        // ...and must be registered in ART-EXAMPLES (so it is lock-gated + compiled).
        ensure(
            registered.contains(row.canonical_example.as_str()),
            format!(
                "concept-canonical: concept `{}` canonical_example `{}` is not in ART-EXAMPLES",
                row.concept_id, row.canonical_example
            ),
        )?;
    }

    // Completeness: every registered example is some concept's canonical example.
    let uncovered: Vec<&str> = registered
        .iter()
        .filter(|path| !seen_examples.contains(*path))
        .copied()
        .collect();
    ensure(
        uncovered.is_empty(),
        format!(
            "concept-canonical: runnable example(s) with no concept_catalog row: {}. \
             Every example must be the canonical example of exactly one concept (add a concept \
             row, or fold the example into an existing canonical one).",
            uncovered.join(", ")
        ),
    )?;
    check_durability_example_family_cap(catalog)?;
    Ok(())
}

const DURABILITY_EXAMPLE_FAMILY: &str = "durability";
const DURABILITY_CANONICAL_CAP: usize = 2;

/// #68 durability dedup: at most two distinct runnable examples may carry
/// `example_family: durability`.
fn check_durability_example_family_cap(catalog: &ConceptCatalog) -> Result<()> {
    let canonical: BTreeSet<&str> = catalog
        .concepts
        .iter()
        .filter(|row| row.example_family.as_deref() == Some(DURABILITY_EXAMPLE_FAMILY))
        .map(|row| row.canonical_example.as_str())
        .collect();
    ensure(
        canonical.len() <= DURABILITY_CANONICAL_CAP,
        format!(
            "concept-canonical: `{DURABILITY_EXAMPLE_FAMILY}` example_family allows at most \
             {DURABILITY_CANONICAL_CAP} distinct canonical examples; got {} ({})",
            canonical.len(),
            canonical.into_iter().collect::<Vec<_>>().join(", ")
        ),
    )?;
    Ok(())
}

fn check_examples_artifact_complete(repo_root: &Path, artifacts: &[ArtifactRecord]) -> Result<()> {
    let examples_artifact = artifacts
        .iter()
        .find(|artifact| artifact.id == "ART-EXAMPLES")
        .context("ART-EXAMPLES artifact must exist")?;
    let declared = examples_artifact
        .paths
        .iter()
        .filter(|path| path.starts_with("crates/batpak-examples/src/bin/") && path.ends_with(".rs"))
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut actual = BTreeSet::new();
    for entry in
        fs::read_dir(core_examples_root(repo_root)).context("read core examples directory")?
    {
        let entry = entry.context("read examples directory entry")?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            actual.insert(relative(repo_root, &path));
        }
    }
    ensure(
        declared == actual,
        format!(
            "ART-EXAMPLES must list every runnable crates/batpak-examples/src/bin/*.rs file exactly once; declared={declared:?}, actual={actual:?}"
        ),
    )
}

#[cfg(test)]
pub(crate) fn validate_observation_evidence(
    repo_root: &Path,
    observation_id: &str,
    evidence: &str,
) -> Result<()> {
    let mut source_cache = SourceCache::new(repo_root);
    validate_observation_evidence_with_cache(repo_root, observation_id, evidence, &mut source_cache)
}

fn validate_observation_evidence_with_cache(
    repo_root: &Path,
    observation_id: &str,
    evidence: &str,
    source_cache: &mut SourceCache,
) -> Result<()> {
    let trimmed = evidence.trim();
    let (path_part, symbol_part) = trimmed
        .split_once("::")
        .map(|(path, symbol)| (path.trim(), Some(symbol.trim())))
        .unwrap_or((trimmed, None));
    ensure(
        !path_part.is_empty(),
        format!("observation {observation_id} evidence `{evidence}` has blank path"),
    )?;
    let full = resolve_repo_or_core_path(repo_root, path_part);
    ensure(
        full.exists(),
        format!(
            "observation {observation_id} evidence `{evidence}` points at missing path `{path_part}`"
        ),
    )?;
    if full.extension().and_then(|ext| ext.to_str()) != Some("rs") {
        return Ok(());
    }
    let Some(symbol) = symbol_part else {
        bail!(
            "observation {observation_id} evidence `{evidence}` points at Rust source but does not name a test/function with `path :: function_name`"
        );
    };
    ensure(
        !symbol.is_empty(),
        format!("observation {observation_id} evidence `{evidence}` has blank function name"),
    )?;
    let file = source_cache
        .parse_rust(&full)
        .with_context(|| format!("parse observation evidence {}", relative(repo_root, &full)))?;
    ensure(
        rust_file_declares_fn(&file, symbol),
        format!(
            "observation {observation_id} evidence `{evidence}` names `{symbol}`, but no Rust function with that name exists in `{path_part}`"
        ),
    )
}

fn rust_file_declares_fn(file: &syn::File, name: &str) -> bool {
    struct FnFinder<'a> {
        name: &'a str,
        found: bool,
    }
    impl<'a, 'ast> syn::visit::Visit<'ast> for FnFinder<'a> {
        fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
            if node.sig.ident == self.name {
                self.found = true;
                return;
            }
            syn::visit::visit_item_fn(self, node);
        }
    }

    let mut finder = FnFinder { name, found: false };
    finder.visit_file(file);
    finder.found
}

#[cfg(test)]
mod concept_canonical_tests {
    use super::{
        check_concept_canonical, check_concept_canonical_over, ArtifactRecord, ConceptCatalog,
    };
    use crate::repo_surface::{load_yaml, repo_root};
    use std::collections::BTreeSet;

    fn catalog(yaml: &str) -> ConceptCatalog {
        yaml_serde::from_str(yaml).expect("parse synthetic concept catalog")
    }

    /// GREEN: the committed concept_catalog.yaml passes on the live tree — every
    /// canonical example exists, is in ART-EXAMPLES, no concept/example duplicated,
    /// and every registered example is covered.
    #[test]
    fn live_concept_catalog_is_clean() {
        let repo = repo_root().expect("repo root");
        let artifacts: Vec<ArtifactRecord> =
            load_yaml(&repo.join("traceability/artifacts.yaml")).expect("load artifacts");
        check_concept_canonical(&repo, &artifacts).expect("committed concept catalog must pass");
    }

    /// RED: two rows claim the same concept_id.
    #[test]
    fn rejects_duplicate_concept_id() {
        let repo = repo_root().expect("repo root");
        let c = catalog(
            r#"
concepts:
  - concept_id: dup
    canonical_example: crates/batpak-examples/src/bin/quickstart.rs
    summary: a
  - concept_id: dup
    canonical_example: crates/batpak-examples/src/bin/eight_jobs.rs
    summary: b
"#,
        );
        let registered: BTreeSet<&str> = [
            "crates/batpak-examples/src/bin/quickstart.rs",
            "crates/batpak-examples/src/bin/eight_jobs.rs",
        ]
        .into_iter()
        .collect();
        let err = check_concept_canonical_over(&repo, &c, &registered)
            .expect_err("duplicate concept_id must fail");
        assert!(format!("{err:#}").contains("duplicate concept_id"));
    }

    /// RED: two concepts name the same canonical example.
    #[test]
    fn rejects_example_canonical_for_two_concepts() {
        let repo = repo_root().expect("repo root");
        let c = catalog(
            r#"
concepts:
  - concept_id: a
    canonical_example: crates/batpak-examples/src/bin/quickstart.rs
    summary: a
  - concept_id: b
    canonical_example: crates/batpak-examples/src/bin/quickstart.rs
    summary: b
"#,
        );
        let registered: BTreeSet<&str> = ["crates/batpak-examples/src/bin/quickstart.rs"]
            .into_iter()
            .collect();
        let err = check_concept_canonical_over(&repo, &c, &registered)
            .expect_err("example canonical for two concepts must fail");
        assert!(format!("{err:#}").contains("more than one concept"));
    }

    /// RED: a canonical example missing from ART-EXAMPLES.
    #[test]
    fn rejects_unregistered_canonical_example() {
        let repo = repo_root().expect("repo root");
        let c = catalog(
            r#"
concepts:
  - concept_id: a
    canonical_example: crates/batpak-examples/src/bin/quickstart.rs
    summary: a
"#,
        );
        // quickstart.rs exists but is NOT in the (empty) registered set.
        let registered: BTreeSet<&str> = BTreeSet::new();
        let err = check_concept_canonical_over(&repo, &c, &registered)
            .expect_err("unregistered canonical example must fail");
        assert!(format!("{err:#}").contains("not in ART-EXAMPLES"));
    }

    /// RED: a registered example with no concept row (completeness).
    #[test]
    fn rejects_uncovered_registered_example() {
        let repo = repo_root().expect("repo root");
        let c = catalog(
            r#"
concepts:
  - concept_id: a
    canonical_example: crates/batpak-examples/src/bin/quickstart.rs
    summary: a
"#,
        );
        let registered: BTreeSet<&str> = [
            "crates/batpak-examples/src/bin/quickstart.rs",
            "crates/batpak-examples/src/bin/eight_jobs.rs",
        ]
        .into_iter()
        .collect();
        let err = check_concept_canonical_over(&repo, &c, &registered)
            .expect_err("uncovered registered example must fail");
        assert!(format!("{err:#}").contains("no concept_catalog row"));
    }

    /// RED: a canonical example that does not exist on disk.
    #[test]
    fn rejects_missing_canonical_file() {
        let repo = repo_root().expect("repo root");
        let c = catalog(
            r#"
concepts:
  - concept_id: a
    canonical_example: crates/batpak-examples/src/bin/does_not_exist.rs
    summary: a
"#,
        );
        let registered: BTreeSet<&str> = ["crates/batpak-examples/src/bin/does_not_exist.rs"]
            .into_iter()
            .collect();
        let err = check_concept_canonical_over(&repo, &c, &registered)
            .expect_err("missing canonical file must fail");
        assert!(format!("{err:#}").contains("does not exist"));
    }

    /// RED: more than two distinct durability-family canonical examples.
    #[test]
    fn rejects_durability_family_over_cap() {
        let repo = repo_root().expect("repo root");
        let c = catalog(
            r#"
concepts:
  - concept_id: a
    canonical_example: crates/batpak-examples/src/bin/append_with_gate.rs
    example_family: durability
    summary: a
  - concept_id: b
    canonical_example: crates/batpak-examples/src/bin/signed_receipts.rs
    example_family: durability
    summary: b
  - concept_id: c
    canonical_example: crates/batpak-examples/src/bin/quickstart.rs
    example_family: durability
    summary: c
"#,
        );
        let registered: BTreeSet<&str> = [
            "crates/batpak-examples/src/bin/append_with_gate.rs",
            "crates/batpak-examples/src/bin/signed_receipts.rs",
            "crates/batpak-examples/src/bin/quickstart.rs",
        ]
        .into_iter()
        .collect();
        let err = check_concept_canonical_over(&repo, &c, &registered)
            .expect_err("durability family over cap must fail");
        assert!(
            format!("{err:#}").contains("example_family"),
            "error must mention example_family cap, got: {err:#}"
        );
    }
}
