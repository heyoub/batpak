//! `fitness_functions.yaml` lockstep (D7) — the declarative rule surface for the
//! architecture fitness functions must stay in step with the code.
//!
//! `traceability/fitness_functions.yaml` is the human-readable catalogue of the
//! triangulation engine's independent crate-graph ORACLES and the directional
//! INVARIANT it enforces. This module makes that catalogue NON-VACUOUS: it asserts
//! in lockstep that (1) the YAML `oracles` name set equals exactly the live
//! `triangulation::default_oracle_names()` roster (no oracle added/renamed/removed
//! in code without updating the surface, and vice versa), and (2) every YAML
//! `invariants[].id` is in `triangulation::ENFORCED_INVARIANTS` AND resolves to a
//! real catalog entry in `traceability/invariants.yaml`. A drift in either
//! direction fails the gate, so the dual-ergonomic surface can never become a
//! stale mirror.
//!
//! Folded into the `triangulation` blocking gate (`structural.rs`).

use crate::repo_surface::load_yaml;
use anyhow::{bail, Result};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[cfg(test)]
#[path = "fitness_functions_tests.rs"]
mod fitness_functions_tests;

#[derive(Debug, Deserialize)]
struct OracleRow {
    name: String,
    predicate: String,
    source: String,
}

#[derive(Debug, Deserialize)]
struct InvariantRow {
    id: String,
    enforced_by: String,
}

#[derive(Debug, Deserialize)]
struct FitnessFunctions {
    oracles: Vec<OracleRow>,
    invariants: Vec<InvariantRow>,
}

pub(crate) fn fitness_functions_path(repo_root: &Path) -> PathBuf {
    repo_root.join("traceability/fitness_functions.yaml")
}

/// Load + lockstep-validate `fitness_functions.yaml` against the live code roster
/// and the invariant catalog. Split from the path so a RED fixture can drive the
/// pure check over a synthetic registry.
pub(crate) fn check(repo_root: &Path) -> Result<()> {
    let registry: FitnessFunctions = load_yaml(&fitness_functions_path(repo_root))?;
    let catalog_ids: BTreeSet<String> = crate::docs_catalog::load_catalog(repo_root)?
        .into_iter()
        .map(|inv| inv.id)
        .collect();
    check_against(
        &registry,
        &crate::triangulation::default_oracle_names(),
        crate::triangulation::ENFORCED_INVARIANTS,
        &catalog_ids,
    )
}

fn check_against(
    registry: &FitnessFunctions,
    live_oracles: &[String],
    enforced_invariants: &[&str],
    catalog_ids: &BTreeSet<String>,
) -> Result<()> {
    // Documentation completeness: every row must carry its descriptive fields, so
    // the dual-ergonomic surface stays a real catalogue (not a bag of bare names).
    for o in &registry.oracles {
        if o.predicate.trim().is_empty() || o.source.trim().is_empty() {
            bail!(
                "fitness-functions: oracle `{}` is missing its `predicate` or `source` description.",
                o.name
            );
        }
    }
    for i in &registry.invariants {
        if i.enforced_by.trim().is_empty() {
            bail!(
                "fitness-functions: invariant `{}` is missing its `enforced_by` description.",
                i.id
            );
        }
    }

    let yaml_oracles: BTreeSet<&str> = registry.oracles.iter().map(|o| o.name.as_str()).collect();
    let code_oracles: BTreeSet<&str> = live_oracles.iter().map(String::as_str).collect();
    if yaml_oracles != code_oracles {
        let only_yaml: Vec<&str> = yaml_oracles.difference(&code_oracles).copied().collect();
        let only_code: Vec<&str> = code_oracles.difference(&yaml_oracles).copied().collect();
        bail!(
            "fitness-functions: oracle roster drift between fitness_functions.yaml and the live \
             triangulation::default_oracles() — only in YAML: {only_yaml:?}; only in code: \
             {only_code:?}. Keep the declarative surface in lockstep with the code."
        );
    }

    let enforced: BTreeSet<&str> = enforced_invariants.iter().copied().collect();
    let yaml_invs: BTreeSet<&str> = registry.invariants.iter().map(|i| i.id.as_str()).collect();
    if yaml_invs != enforced {
        let only_yaml: Vec<&str> = yaml_invs.difference(&enforced).copied().collect();
        let only_code: Vec<&str> = enforced.difference(&yaml_invs).copied().collect();
        bail!(
            "fitness-functions: enforced-invariant drift between fitness_functions.yaml and \
             triangulation::ENFORCED_INVARIANTS — only in YAML: {only_yaml:?}; only in code: \
             {only_code:?}."
        );
    }

    for row in &registry.invariants {
        if !catalog_ids.contains(&row.id) {
            bail!(
                "fitness-functions: invariant `{}` in fitness_functions.yaml is not in the \
                 invariants.yaml catalog — every enforced invariant must be a real catalog entry.",
                row.id
            );
        }
    }
    Ok(())
}
