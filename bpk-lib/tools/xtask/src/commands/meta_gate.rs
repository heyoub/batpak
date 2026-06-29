//! `cargo xtask meta-gate` — the thin git/CI shell around the pure meta-gate
//! classifier in `tools/integrity/src/meta_gate.rs` (P1-4).
//!
//! Resolves the base ref, produces the `base..HEAD` unified diff and the PR's
//! commit messages (author + body), gathers PR labels, and hands all of it to
//! `batpak-integrity meta-gate-check`, which runs the pure classifier and the
//! approval logic. All detection lives in the integrity crate so it is unit- and
//! mutation-tested; this shell is intentionally I/O-only.

use crate::util::{cargo_target_dir, git_output, git_output_lossy, repo_root};
use crate::MetaGateArgs;
use anyhow::{Context, Result};
use std::fs;
use std::process::Command;

/// Resolve the base commit to diff against: `--base` if given, else the
/// merge-base with `origin/main` (falling back to `main`, then to `HEAD~1`).
fn resolve_base(root: &std::path::Path, explicit: Option<&str>) -> Result<String> {
    if let Some(base) = explicit {
        return Ok(base.to_string());
    }
    for candidate in ["origin/main", "main"] {
        if let Ok(merge_base) = git_output(root, ["merge-base", candidate, "HEAD"]) {
            if !merge_base.is_empty() {
                return Ok(merge_base);
            }
        }
    }
    // Last resort: the previous commit, so the gate still has something to diff.
    git_output(root, ["rev-parse", "HEAD~1"]).context(
        "resolve meta-gate base: no --base, no origin/main or main merge-base, and no HEAD~1",
    )
}

/// Labels from `--label` flags, or (when none given) from the
/// `GAUNTLET_PR_LABELS` env var (comma- or newline-separated). This lets CI pass
/// `github.event.pull_request.labels.*.name` through one variable.
fn resolve_labels(flag_labels: &[String]) -> Vec<String> {
    if !flag_labels.is_empty() {
        return flag_labels.to_vec();
    }
    std::env::var("GAUNTLET_PR_LABELS")
        .ok()
        .map(|raw| {
            raw.split([',', '\n'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn meta_gate(args: &MetaGateArgs) -> Result<()> {
    let root = repo_root()?;
    let base = resolve_base(&root, args.base.as_deref())?;
    outln!("meta-gate: diffing {base}..HEAD");

    // The unified diff for the classifier. `--no-color`/no `--stat` so the text
    // is the raw unified-diff shape `parse_unified_diff` expects.
    let diff = git_output_lossy(&root, ["diff", "--no-color", &format!("{base}..HEAD")])
        .context("git diff base..HEAD for meta-gate")?;
    // Commit messages with author + body, record-separated so the integrity side
    // can attribute `GAUNTLET-WEAKEN-OK:` trailers to their commit author.
    let commits = git_output(
        &root,
        ["log", &format!("{base}..HEAD"), "--format=%an%x00%B%x01"],
    )
    .context("git log base..HEAD for meta-gate trailers")?;

    let work_dir = cargo_target_dir()?.join("meta-gate");
    fs::create_dir_all(&work_dir)
        .with_context(|| format!("create meta-gate work dir {}", work_dir.display()))?;
    let diff_path = work_dir.join("diff.patch");
    let commits_path = work_dir.join("commits.txt");
    fs::write(&diff_path, &diff).context("write meta-gate diff file")?;
    fs::write(&commits_path, &commits).context("write meta-gate commits file")?;

    let labels = resolve_labels(&args.labels);
    let pr_author = args
        .pr_author
        .clone()
        .or_else(|| std::env::var("GAUNTLET_PR_AUTHOR").ok())
        .filter(|s| !s.is_empty());

    let mut command = Command::new("cargo");
    command.current_dir(&root).args([
        "run",
        "--package",
        "batpak-integrity",
        "--",
        "meta-gate-check",
        "--diff-file",
    ]);
    command.arg(&diff_path);
    command.args(["--commits-file"]);
    command.arg(&commits_path);
    for label in &labels {
        command.args(["--label", label]);
    }
    if let Some(author) = &pr_author {
        command.args(["--pr-author", author]);
    }
    crate::util::run(command)
}
