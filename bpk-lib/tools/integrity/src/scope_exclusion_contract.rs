//! INV-CROSS-DIRECTORY-CONSISTENCY-PRODUCT-OWNED: Store must not claim
//! cross-data_dir or multi-journal consistency as a substrate invariant.

use crate::repo_surface::{ensure, project_root, relative, rust_files};
use crate::source_cache::SourceCache;
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const BANNED_CLAIMS: &[&str] = &[
    "store guarantees cross-data_dir consistency",
    "store provides cross-data_dir consistency",
    "store enforces cross-data_dir consistency",
    "batpak guarantees cross-data_dir consistency",
    "batpak provides cross-data_dir consistency",
    "batpak enforces cross-data_dir consistency",
    "cross-data_dir consistency is a store invariant",
    "cross-data_dir consistency is a batpak invariant",
    "store guarantees cross-directory consistency",
    "store provides cross-directory consistency",
    "store enforces cross-directory consistency",
    "batpak guarantees cross-directory consistency",
    "batpak provides cross-directory consistency",
    "batpak enforces cross-directory consistency",
    "cross-directory consistency is a store invariant",
    "cross-directory consistency is a batpak invariant",
    "multi-journal consistency is a store invariant",
    "multi-journal consistency is a batpak invariant",
];

pub(crate) fn check(repo_root: &Path, source_cache: &mut SourceCache) -> Result<BTreeSet<PathBuf>> {
    let mut inputs = BTreeSet::new();
    let project_root = project_root(repo_root);

    for path in root_markdown_files(project_root)? {
        inputs.insert(path.clone());
        let rel = relative(repo_root, &path);
        let content = fs::read_to_string(&path).with_context(|| format!("read {rel}"))?;
        let findings = positive_scope_claim_findings(&rel, &content);
        enforce_findings(&findings)?;
    }

    for path in rust_files(&repo_root.join("crates/core/src")) {
        inputs.insert(path.clone());
        let rel = relative(repo_root, &path);
        let content = source_cache
            .read_to_string(&path)
            .with_context(|| format!("read {rel}"))?;
        let findings = positive_scope_claim_findings(&rel, &content);
        enforce_findings(&findings)?;
    }

    Ok(inputs)
}

fn root_markdown_files(project_root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in
        fs::read_dir(project_root).with_context(|| format!("read {}", project_root.display()))?
    {
        let entry = entry.context("read project-root dir entry")?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn positive_scope_claim_findings(rel: &str, content: &str) -> Vec<String> {
    let lowered = content.to_lowercase();
    BANNED_CLAIMS
        .iter()
        .filter(|claim| lowered.contains(**claim))
        .map(|claim| format!("{rel}: banned positive scope claim `{claim}`"))
        .collect()
}

fn enforce_findings(findings: &[String]) -> Result<()> {
    ensure(
        findings.is_empty(),
        format!(
            "cross-directory-scope-contract (INV-CROSS-DIRECTORY-CONSISTENCY-PRODUCT-OWNED): \
             Store/BatPak must not claim product-owned cross-data_dir consistency as a substrate \
             invariant:\n  {}",
            findings.join("\n  ")
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cross_directory_scope_contract_rejects_positive_store_overclaim() {
        let findings = positive_scope_claim_findings(
            "README.md",
            "Store guarantees cross-data_dir consistency across multiple journals.",
        );
        assert!(!findings.is_empty());
        assert!(
            enforce_findings(&findings).is_err(),
            "positive cross-data_dir consistency claim must be rejected"
        );
    }

    #[test]
    fn cross_directory_scope_contract_accepts_scope_exclusion() {
        let findings = positive_scope_claim_findings(
            "README.md",
            "Cross-data_dir consistency is outside Store invariants; products compose it above the substrate.",
        );
        assert!(findings.is_empty());
    }
}
