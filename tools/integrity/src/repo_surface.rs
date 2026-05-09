use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

pub(crate) fn tracked_repo_files(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .args(["ls-files"])
        .current_dir(repo_root)
        .output()
        .context("git ls-files")?;
    ensure(output.status.success(), "git ls-files failed")?;

    let stdout = String::from_utf8(output.stdout).context("git ls-files utf8")?;
    let mut files = Vec::new();
    for line in stdout.lines().filter(|line| !line.is_empty()) {
        let path = repo_root.join(line);
        if path.exists() {
            files.push(path);
        }
    }
    Ok(files)
}

pub(crate) fn rust_files(root: &Path) -> Vec<PathBuf> {
    files_with_extension(root, "rs")
}

pub(crate) fn files_with_extension(root: &Path, extension: &str) -> Vec<PathBuf> {
    if !root.exists() {
        return Vec::new();
    }
    WalkDir::new(root)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some(extension))
        .map(|entry| entry.into_path())
        .collect()
}

pub(crate) fn repo_root() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("failed to determine repo root"))
}

pub(crate) fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub(crate) fn load_yaml<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    yaml_serde::from_str(&content).with_context(|| format!("parse {}", path.display()))
}

pub(crate) fn ensure_unique_ids<'a>(
    ids: impl IntoIterator<Item = &'a str>,
    context: &str,
) -> Result<()> {
    let mut seen = BTreeSet::new();
    for id in ids {
        ensure(seen.insert(id.to_string()), format!("{context}: {id}"))?;
    }
    Ok(())
}

pub(crate) fn check_command(program: &str, args: &[&str]) -> Result<()> {
    ensure(
        command_exists(program, args),
        format!(
            "required command missing or failing: {program} {}",
            args.join(" ")
        ),
    )
}

pub(crate) fn command_exists(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

pub(crate) fn ensure(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        bail!(message.into())
    }
}
