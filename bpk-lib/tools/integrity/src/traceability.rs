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

    outln!("traceability-check: ok");
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
        .filter(|path| path.starts_with("crates/core/examples/") && path.ends_with(".rs"))
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
            "ART-EXAMPLES must list every runnable crates/core/examples/*.rs file exactly once; declared={declared:?}, actual={actual:?}"
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
