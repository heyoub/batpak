use anyhow::{bail, Context, Result};
use std::process::Command;

use super::lanes::MutationLane;
use super::plan::mutants_command;
use super::policy::assert_mutation_policy;
use super::score::mutation_score;

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
    command.args(mutants_command(lane, &output_dir));
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
