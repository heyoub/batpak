mod docs_contract;
mod platform_boundary;
mod public_api_truth;
mod repo_hygiene;
mod source_citations;
mod syncbat_boundary;
mod tooling_contract;

use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

pub fn check(repo_root: &Path, tracked_files: &[PathBuf]) -> Result<()> {
    repo_hygiene::check(repo_root, tracked_files)?;
    platform_boundary::check(repo_root, tracked_files)?;
    syncbat_boundary::check(repo_root, tracked_files)?;
    tooling_contract::check(repo_root)?;
    docs_contract::check(repo_root)?;
    source_citations::check(repo_root)?;
    public_api_truth::check(repo_root)?;
    Ok(())
}

pub(super) fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .or_else(|_| path.strip_prefix(root.parent().unwrap_or(root)))
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub(super) fn ensure(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(anyhow!(message.into()))
    }
}
