use anyhow::{bail, Result};

use crate::{MutantMode, MutantSurface, MutantsArgs};

use super::dst_corpus::apply_graduated_dst_corpus_augmentation;
use super::lanes::{
    critical_mutation_lanes, critical_mutation_smoke_lane_for_seam, critical_mutation_smoke_lanes,
    critical_seam_slugs, repo_wide_mutation_lanes, MutationBaseline, MutationLane,
    MutationSharding,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum MutantExecutionPlan {
    DescribePolicy,
    Run(Vec<MutationLane>),
}

fn with_batched_baseline(mut lanes: Vec<MutationLane>) -> Vec<MutationLane> {
    let mut first = true;
    for lane in &mut lanes {
        lane.baseline = if first {
            first = false;
            MutationBaseline::Run
        } else {
            MutationBaseline::Skip
        };
    }
    lanes
}

fn with_run_baseline(mut lanes: Vec<MutationLane>) -> Vec<MutationLane> {
    for lane in &mut lanes {
        lane.baseline = MutationBaseline::Run;
    }
    lanes
}

fn smoke_selection_flags(args: &MutantsArgs) -> Result<()> {
    if args.seam.is_some() && args.repo_wide_only {
        bail!("`cargo xtask mutants smoke` accepts `--seam` or `--repo-wide-only`, not both");
    }
    if args.surface.is_some() || args.shard.is_some() {
        bail!(
            "`cargo xtask mutants smoke` does not accept `--surface` or `--shard`; use \
             `--seam <slug>` for one critical seam or `--repo-wide-only` for repo-wide smoke"
        );
    }
    Ok(())
}

pub(super) fn mutants_command(
    lane: &MutationLane,
    output_dir: &std::path::Path,
    diff_path: Option<&std::path::Path>,
) -> Vec<String> {
    let mut args = vec![
        "mutants".to_owned(),
        "--output".to_owned(),
        output_dir.display().to_string(),
        "--in-place".to_owned(),
    ];

    args.push("--baseline".to_owned());
    args.push(match lane.baseline {
        MutationBaseline::Run => "run".to_owned(),
        MutationBaseline::Skip => "skip".to_owned(),
    });

    for pattern in lane.paths {
        args.push("--file".to_owned());
        args.push((*pattern).to_owned());
    }

    for exclude in lane.excludes {
        args.push("--exclude".to_owned());
        args.push((*exclude).to_owned());
    }

    for exclude_re in lane.exclude_res {
        args.push("--exclude-re".to_owned());
        args.push((*exclude_re).to_owned());
    }

    if let Some(package) = lane.package {
        args.push("--package".to_owned());
        args.push(package.to_owned());
    }

    for package in &lane.test_packages {
        args.push("--test-package".to_owned());
        args.push((*package).to_owned());
    }

    match lane.surface {
        MutantSurface::AllFeatures => args.push("--all-features".to_owned()),
        MutantSurface::NoDefaultFeatures => args.push("--no-default-features".to_owned()),
    }
    args.push("--cargo-arg".to_owned());
    args.push("--locked".to_owned());
    args.push("--test-tool".to_owned());
    // nextest, not raw `cargo test`: per-test process isolation + the `ci`
    // profile's `terminate-after` (pinned via NEXTEST_PROFILE in run.rs) reap a
    // mutation-induced livelock as a bounded per-test timeout, so a single hung
    // test can no longer mask the fast-failing assertions that actually convict
    // the mutant. This aligns the mutation lane with every other test lane in the
    // house (run_nextest_ci), which has always run under nextest.
    args.push("nextest".to_owned());

    if lane.diff_scoped {
        // Diff-scoped lanes mutate only the lines the PR touched, intersected
        // with the seam `--file` globs already pushed above. This makes the
        // gated mutant population deterministic w.r.t. the PR instead of
        // drifting with the content-derived round-robin shard index.
        if let Some(diff_path) = diff_path {
            args.push("--in-diff".to_owned());
            args.push(diff_path.display().to_string());
        }
    } else {
        if let Some(shard) = lane.shard.as_deref() {
            args.push("--shard".to_owned());
            args.push(shard.to_owned());
        }

        if let Some(sharding) = lane.sharding {
            args.push("--sharding".to_owned());
            args.push(match sharding {
                MutationSharding::RoundRobin => "round-robin".to_owned(),
            });
        }
    }

    args
}

fn finish_run_plan(mut lanes: Vec<MutationLane>) -> Result<MutantExecutionPlan> {
    apply_graduated_dst_corpus_augmentation(&mut lanes)?;
    Ok(MutantExecutionPlan::Run(lanes))
}

pub(super) fn build_mutant_execution_plan(args: &MutantsArgs) -> Result<MutantExecutionPlan> {
    match args.mode {
        MutantMode::Policy => {
            if args.surface.is_some()
                || args.shard.is_some()
                || args.seam.is_some()
                || args.repo_wide_only
            {
                bail!(
                    "`cargo xtask mutants policy` only describes repo-owned policy; do not pass \
                     --surface, --shard, --seam, or --repo-wide-only"
                );
            }
            Ok(MutantExecutionPlan::DescribePolicy)
        }
        MutantMode::Smoke => {
            smoke_selection_flags(args)?;

            if let Some(slug) = args.seam.as_deref() {
                let lane = match critical_mutation_smoke_lane_for_seam(slug) {
                    Some(lane) => lane,
                    None => {
                        let valid = critical_seam_slugs().join(", ");
                        bail!("unknown critical seam `{slug}`; valid slugs: {valid}");
                    }
                };
                return finish_run_plan(with_run_baseline(vec![lane]));
            }

            if args.repo_wide_only {
                return finish_run_plan(with_run_baseline(vec![
                    MutationLane::repo_wide_smoke(MutantSurface::AllFeatures),
                    MutationLane::repo_wide_smoke(MutantSurface::NoDefaultFeatures),
                ]));
            }

            let mut lanes = critical_mutation_smoke_lanes();
            lanes.extend([
                MutationLane::repo_wide_smoke(MutantSurface::AllFeatures),
                MutationLane::repo_wide_smoke(MutantSurface::NoDefaultFeatures),
            ]);
            finish_run_plan(with_batched_baseline(lanes))
        }
        MutantMode::Full => {
            if args.seam.is_some() || args.repo_wide_only {
                bail!(
                    "`cargo xtask mutants full` does not accept `--seam` or `--repo-wide-only`; \
                     use `cargo xtask mutants smoke`"
                );
            }

            let surfaces = args.surface.map_or_else(
                || vec![MutantSurface::AllFeatures, MutantSurface::NoDefaultFeatures],
                |surface| vec![surface],
            );

            if args.surface.is_some() || args.shard.is_some() {
                return finish_run_plan(repo_wide_mutation_lanes(surfaces, args.shard.as_deref()));
            }

            let mut lanes = critical_mutation_lanes();
            lanes.extend(repo_wide_mutation_lanes(surfaces, None));
            finish_run_plan(with_batched_baseline(lanes))
        }
    }
}
