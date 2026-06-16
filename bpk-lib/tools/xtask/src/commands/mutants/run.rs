use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Command;

use crate::util::{git_output, repo_root};

use super::lanes::MutationLane;
use super::plan::mutants_command;
use super::policy::assert_mutation_policy;
use super::score::mutation_score;

/// Environment variable carrying the path to the PR base..head patch that
/// scopes diff-scoped smoke lanes. CI writes the patch onto the mounted
/// workspace and exports this so the value resolves identically inside the
/// devcontainer.
const MUTANTS_DIFF_ENV: &str = "BATPAK_MUTANTS_DIFF";

/// Resolve the diff patch that scopes `--in-diff` smoke lanes.
///
/// Resolution order:
/// 1. `BATPAK_MUTANTS_DIFF` (CI plumbs the PR base..head patch here). The value
///    is used verbatim and must point at a readable file.
/// 2. Local best-effort fallback: write `git diff <merge-base>...HEAD` for the
///    current branch against `origin/main` (then `main`) to a scratch file under
///    the cargo target dir and return that path.
///
/// Returns `Ok(None)` only when no diff can be resolved at all (e.g. a local run
/// with no upstream); callers treat that as "no in-diff scope available".
fn resolve_smoke_diff() -> Result<Option<PathBuf>> {
    if let Some(path) = std::env::var_os(MUTANTS_DIFF_ENV) {
        let path = PathBuf::from(path);
        if !path.exists() {
            bail!(
                "{MUTANTS_DIFF_ENV} points at `{}` but that file does not exist; \
                 the diff-scoped smoke gate cannot select PR mutants without it.",
                path.display()
            );
        }
        return Ok(Some(path));
    }

    // Local best-effort: derive a merge-base against a sensible upstream and
    // materialize the patch so cargo-mutants can read it. Never hard-fails the
    // lane on its own — if we cannot resolve an upstream, the lane simply runs
    // with no in-diff scope and the policy early-return handles an empty result.
    let root = repo_root()?;
    let base = ["origin/main", "main"]
        .into_iter()
        .find_map(|upstream| git_output(&root, ["merge-base", upstream, "HEAD"]).ok())
        .filter(|sha| !sha.is_empty());
    let Some(base) = base else {
        return Ok(None);
    };
    // `--relative` emits paths relative to `bpk-lib/` (the git cwd here), which
    // is exactly the directory cargo-mutants runs in. Without it git emits
    // repo-root-relative `bpk-lib/...` paths that cargo-mutants' --in-diff
    // matcher silently drops, selecting zero mutants.
    let diff = git_output(&root, ["diff", "--relative", &base, "HEAD"])?;
    let diff_path = super::lanes::mutants_output_root().join("smoke-diff.patch");
    if let Some(parent) = diff_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "create scratch directory {} for smoke diff",
                parent.display()
            )
        })?;
    }
    std::fs::write(&diff_path, diff)
        .with_context(|| format!("write smoke diff to {}", diff_path.display()))?;
    Ok(Some(diff_path))
}

pub(super) fn run_mutation_lane(lane: &MutationLane) -> Result<()> {
    let output_dir = lane.output_dir();
    let _ = std::fs::remove_dir_all(&output_dir);
    if let Some(parent) = output_dir.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "create cargo-mutants output parent directory {} for `{}`",
                parent.display(),
                lane.label
            )
        })?;
    }

    let mut command = Command::new("cargo");
    // cargo-mutants in `--in-place` mode can interact badly with incremental
    // artifacts and produce linker-only failures that disappear under a fresh
    // rebuild. Keep mutation receipts honest by forcing clean codegen for the
    // lane instead of inheriting ambient incremental state.
    command.env("CARGO_INCREMENTAL", "0");
    let diff_path = if lane.diff_scoped {
        resolve_smoke_diff()?
    } else {
        None
    };
    command.args(mutants_command(lane, &output_dir, diff_path.as_deref()));
    let status = command
        .status()
        .with_context(|| format!("run cargo-mutants lane `{}`", lane.label))?;

    let score = mutation_score(&output_dir).with_context(|| {
        format!(
            "read cargo-mutants results for `{}` from {}",
            lane.label,
            output_dir.display()
        )
    })?;

    let policy_result = assert_mutation_policy(lane, &output_dir, score);
    if status.success() || lane.allows_nonzero_exit(score) {
        return policy_result;
    }

    match policy_result {
        Ok(()) => bail!(
            "cargo-mutants exited with status {status} for `{}` even though the xtask policy \
             checks passed. Inspect {}.",
            lane.label,
            output_dir.display()
        ),
        Err(err) => Err(err).context(format!(
            "cargo-mutants exited with status {status} for `{}`; inspect {}",
            lane.label,
            output_dir.display()
        )),
    }
}
