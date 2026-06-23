use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

pub(crate) const CORE_CRATE_REL: &str = "crates/core";

pub(crate) fn core_crate_root(repo_root: &Path) -> PathBuf {
    repo_root.join(CORE_CRATE_REL)
}

pub(crate) fn core_src_root(repo_root: &Path) -> PathBuf {
    core_crate_root(repo_root).join("src")
}

pub(crate) fn core_tests_root(repo_root: &Path) -> PathBuf {
    core_crate_root(repo_root).join("tests")
}

pub(crate) fn core_examples_root(repo_root: &Path) -> PathBuf {
    core_crate_root(repo_root).join("examples")
}

pub(crate) fn core_benches_root(repo_root: &Path) -> PathBuf {
    core_crate_root(repo_root).join("benches")
}

pub(crate) fn core_path(repo_root: &Path, crate_relative: impl AsRef<Path>) -> PathBuf {
    core_crate_root(repo_root).join(crate_relative)
}

pub(crate) fn resolve_repo_or_core_path(repo_root: &Path, rel: impl AsRef<Path>) -> PathBuf {
    let rel = rel.as_ref();
    let direct = repo_root.join(rel);
    if direct.exists() {
        return direct;
    }
    let project_direct = project_root(repo_root).join(rel);
    if project_direct.exists() {
        return project_direct;
    }
    if is_primary_crate_relative_path(rel) {
        return core_path(repo_root, rel);
    }
    direct
}

fn is_primary_crate_relative_path(rel: &Path) -> bool {
    let rel = rel.to_string_lossy().replace('\\', "/");
    rel == "build.rs"
        || rel.starts_with("build.rs:")
        || rel.starts_with("src/")
        || rel.starts_with("tests/")
        || rel.starts_with("examples/")
        || rel.starts_with("benches/")
        || rel.starts_with("fixtures/")
}

pub(crate) fn tracked_repo_files(repo_root: &Path) -> Result<Vec<PathBuf>> {
    // Clear the git environment a parent process (e.g. a `git commit` pre-commit
    // hook) may have exported. With an inherited `GIT_DIR`/`GIT_WORK_TREE`/
    // `GIT_INDEX_FILE`, `--exclude-standard` resolves .gitignore relative to the
    // hook's work tree rather than `current_dir(repo_root)`, which leaks ignored
    // build artifacts (e.g. `target/.rustc_info.json`) into the listing — making
    // the hygiene scan flag a path-leak only when run from a hook. Unsetting them
    // makes `git ls-files` resolve the worktree from `current_dir` deterministically,
    // so the tracked set is identical whether invoked manually or from a commit hook.
    let output = Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .current_dir(repo_root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_PREFIX")
        .output()
        .context("git ls-files")?;
    ensure(output.status.success(), "git ls-files failed")?;

    let stdout = String::from_utf8(output.stdout).context("git ls-files utf8")?;
    let mut files = Vec::new();
    for line in stdout.lines().filter(|line| !line.is_empty()) {
        // Defensively skip Cargo build output. `target/` is gitignored, so
        // `--exclude-standard` normally omits it; but when `git ls-files` runs
        // inside the pre-commit hook of a linked WORKTREE (cwd = bpk-lib, git
        // env inherited from the in-flight `git commit`), git's untracked-file
        // scan can momentarily surface `target/.rustc_info.json` and friends
        // while a concurrent build writes them. Those are never part of the
        // integrity surface, so exclude any path under a `target/` directory
        // explicitly — a robustness fix, not a gate relaxation.
        if line.split('/').any(|component| component == "target") {
            continue;
        }
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

/// The non-core production crate `src` roots (core's `src` is handled separately
/// via [`core_src_root`]). Shared by structural file-size checks and the
/// assurance manifest's unleveled-file advisory so both walk the same surface.
pub(crate) fn production_rust_roots(repo_root: &Path) -> Vec<PathBuf> {
    [
        "crates/macros/src",
        "crates/macros-support/src",
        "crates/syncbat-macros/src",
        "crates/syncbat/src",
        "crates/netbat/src",
        "crates/bvisor/src",
    ]
    .iter()
    .map(|rel| repo_root.join(rel))
    .collect()
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

pub(crate) fn project_root(repo_root: &Path) -> &Path {
    repo_root.parent().unwrap_or(repo_root)
}

pub(crate) fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .or_else(|_| path.strip_prefix(project_root(root)))
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

pub(crate) fn missing_commands<'a, I>(commands: I) -> Vec<String>
where
    I: IntoIterator<Item = (&'a str, &'a [&'a str])>,
{
    commands
        .into_iter()
        .filter(|(program, args)| !command_exists(program, args))
        .map(|(program, args)| format!("{program} {}", args.join(" ")))
        .collect()
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
