use crate::util::{project_root, run_output};
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

pub(crate) fn staged_diff() -> Result<()> {
    let root = project_root()?;
    let entries = staged_entries(&root)?;
    if entries.is_empty() {
        println!("staged-diff: ok; no staged files");
        return Ok(());
    }

    let mut counts = BTreeMap::new();
    let mut violations = Vec::new();
    for entry in &entries {
        *counts.entry(entry.status.as_str()).or_insert(0usize) += 1;
        if !entry.status.starts_with('D') {
            if let Some(reason) = forbidden_staged_path(&entry.path) {
                violations.push(format!("{} ({reason})", entry.path));
            }
        }
        if staged_worktree_file_has_conflict_markers(&root, &entry.path)? {
            violations.push(format!("{} (conflict marker)", entry.path));
        }
    }

    for (status, count) in counts {
        println!("staged-diff: {status}: {count}");
    }

    if !violations.is_empty() {
        for violation in &violations {
            eprintln!("staged-diff: forbidden staged path: {violation}");
        }
        bail!("staged-diff found {} issue(s)", violations.len());
    }

    println!("staged-diff: ok; {} staged file(s)", entries.len());
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
struct StagedEntry {
    status: String,
    path: String,
}

fn staged_entries(root: &Path) -> Result<Vec<StagedEntry>> {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .args(["diff", "--cached", "--name-status"]);
    let output = run_output(command)?;
    let stdout = String::from_utf8(output.stdout).context("git diff --cached utf8")?;
    let mut entries = Vec::new();
    for line in stdout.lines() {
        let mut parts = line.split('\t');
        let status = parts.next().unwrap_or_default();
        let path = parts.last().unwrap_or_default();
        if status.is_empty() || path.is_empty() {
            continue;
        }
        entries.push(StagedEntry {
            status: status.to_owned(),
            path: path.to_owned(),
        });
    }
    Ok(entries)
}

fn forbidden_staged_path(path: &str) -> Option<&'static str> {
    if path == "Cargo.toml" || path == "Cargo.lock" {
        return Some("workspace manifest/lock belongs under bpk-lib");
    }
    if path == ".cargo/config.toml" || path == ".cargo/mutants.toml" {
        return Some("Cargo tool config belongs under bpk-lib/.cargo");
    }
    if path.starts_with("docs/") {
        return Some("ordered root docs and cookbook replaced docs/");
    }
    if path.starts_with("crates/") || path.starts_with("tools/") || path.starts_with("templates/") {
        return Some("implementation workspace belongs under bpk-lib/");
    }
    if path.contains("/target/") || path.starts_with("target/") || path.ends_with("/target") {
        return Some("generated Cargo target artifact");
    }
    if path.starts_with("bpk-lib/templates/") && path.ends_with("/Cargo.lock") {
        return Some("generated template lockfile");
    }
    None
}

fn staged_worktree_file_has_conflict_markers(root: &Path, path: &str) -> Result<bool> {
    let full = root.join(path);
    if !full.is_file() || !is_text_like_path(path) {
        return Ok(false);
    }
    let content = fs::read_to_string(&full).with_context(|| format!("read {}", full.display()))?;
    Ok(content.lines().any(|line| {
        line.starts_with("<<<<<<< ") || line.starts_with("=======") || line.starts_with(">>>>>>> ")
    }))
}

fn is_text_like_path(path: &str) -> bool {
    let Some((_, extension)) = path.rsplit_once('.') else {
        return path == "justfile";
    };
    matches!(
        extension,
        "rs" | "toml" | "md" | "yml" | "yaml" | "json" | "sh" | "ps1" | "txt"
    )
}

#[cfg(test)]
mod tests {
    use super::{forbidden_staged_path, is_text_like_path};

    #[test]
    fn rejects_generated_and_retired_layout_paths() {
        assert!(forbidden_staged_path("target/debug/lib.rlib").is_some());
        assert!(forbidden_staged_path("bpk-lib/templates/minimal/Cargo.lock").is_some());
        assert!(forbidden_staged_path("docs/README.md").is_some());
        assert!(forbidden_staged_path("crates/core/src/lib.rs").is_some());
        assert!(forbidden_staged_path("bpk-lib/crates/core/src/lib.rs").is_none());
    }

    #[test]
    fn text_like_paths_cover_repo_surfaces() {
        assert!(is_text_like_path("src/lib.rs"));
        assert!(is_text_like_path("AGENTS.md"));
        assert!(is_text_like_path("justfile"));
        assert!(!is_text_like_path("image.png"));
    }
}
