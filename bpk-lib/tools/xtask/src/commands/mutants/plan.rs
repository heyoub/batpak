use anyhow::{bail, Result};

use crate::{MutantMode, MutantSurface, MutantsArgs};

use super::lanes::{
    critical_mutation_lanes, critical_mutation_smoke_lanes, repo_wide_mutation_lanes,
    MutationBaseline, MutationLane, MutationSharding,
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

pub(super) fn mutants_command(lane: &MutationLane, output_dir: &std::path::Path) -> Vec<String> {
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

    match lane.surface {
        MutantSurface::AllFeatures => args.push("--all-features".to_owned()),
        MutantSurface::NoDefaultFeatures => args.push("--no-default-features".to_owned()),
    }
    args.push("--cargo-arg".to_owned());
    args.push("--locked".to_owned());
    args.push("--test-tool".to_owned());
    args.push("cargo".to_owned());

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

    args
}

pub(super) fn build_mutant_execution_plan(args: &MutantsArgs) -> Result<MutantExecutionPlan> {
    match args.mode {
        MutantMode::Policy => {
            if args.surface.is_some() || args.shard.is_some() {
                bail!(
                    "`cargo xtask mutants policy` only describes repo-owned policy; do not pass \
                     --surface or --shard"
                );
            }
            Ok(MutantExecutionPlan::DescribePolicy)
        }
        MutantMode::Smoke => {
            if args.surface.is_some() || args.shard.is_some() {
                bail!(
                    "`cargo xtask mutants smoke` owns its fixed policy lanes; do not pass \
                     --surface or --shard"
                );
            }

            let mut lanes = critical_mutation_smoke_lanes();
            lanes.extend([
                MutationLane::repo_wide_smoke(MutantSurface::AllFeatures),
                MutationLane::repo_wide_smoke(MutantSurface::NoDefaultFeatures),
            ]);
            Ok(MutantExecutionPlan::Run(with_batched_baseline(lanes)))
        }
        MutantMode::Full => {
            let surfaces = args.surface.map_or_else(
                || vec![MutantSurface::AllFeatures, MutantSurface::NoDefaultFeatures],
                |surface| vec![surface],
            );

            if args.surface.is_some() || args.shard.is_some() {
                return Ok(MutantExecutionPlan::Run(repo_wide_mutation_lanes(
                    surfaces,
                    args.shard.as_deref(),
                )));
            }

            let mut lanes = critical_mutation_lanes();
            lanes.extend(repo_wide_mutation_lanes(surfaces, None));
            Ok(MutantExecutionPlan::Run(with_batched_baseline(lanes)))
        }
    }
}
