mod lanes;
mod plan;
mod policy;
mod run;
mod score;

use crate::MutantsArgs;
use anyhow::Result;

use self::plan::{build_mutant_execution_plan, MutantExecutionPlan};
use self::policy::print_mutation_policy;
use self::run::run_mutation_lane;

pub(crate) fn mutants(args: MutantsArgs) -> Result<()> {
    match build_mutant_execution_plan(&args)? {
        MutantExecutionPlan::DescribePolicy => {
            print_mutation_policy();
            Ok(())
        }
        MutantExecutionPlan::Run(lanes) => {
            for lane in lanes {
                run_mutation_lane(&lane)?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::lanes::{
        critical_mutation_lanes, critical_mutation_smoke_lanes, surface_excludes, MutationBaseline,
        MutationLane, MutationScope, MutationSharding, CURSOR_MUTANT_FILES,
        EVENT_PAYLOAD_REGISTRY_MUTANT_FILES, FRONTIER_APPEND_GATE_MUTANT_FILES,
        FRONTIER_WAIT_MUTANT_FILES, HARNESS_LEDGER_LINT_MUTANT_FILES,
        INDEX_TOPOLOGY_DEFAULT_EQUIVALENT_MUTANT, PLATFORM_BACKEND_MUTANT_FILES,
        PROJECTION_MUTANT_FILES, REPO_WIDE_ALL_FEATURES_MUTANT_FILES,
        REPO_WIDE_NO_DEFAULT_MUTANT_FILES, SIDX_EMPTY_FOOTER_FLOOR_EQUIVALENT_MUTANT,
        WRITER_COMMIT_MUTANT_FILES,
    };
    use super::plan::{build_mutant_execution_plan, mutants_command, MutantExecutionPlan};
    use super::policy::{
        assert_mutation_policy, next_ratchet_floor, RepoMutationPhase, REPO_MUTATION_PHASE,
    };
    use super::score::{cargo_mutants_results_dir, mutation_score, MutationScore};
    use crate::{MutantMode, MutantSurface, MutantsArgs};
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn mutants_smoke_plan_runs_critical_then_repo_wide_ratchet_lanes() {
        let plan = build_mutant_execution_plan(&MutantsArgs {
            mode: MutantMode::Smoke,
            surface: None,
            shard: None,
        })
        .expect("smoke plan");

        assert_eq!(
            plan,
            MutantExecutionPlan::Run(vec![
                critical_mutation_smoke_lanes()[0]
                    .clone()
                    .with_baseline(MutationBaseline::Run),
                critical_mutation_smoke_lanes()[1]
                    .clone()
                    .with_baseline(MutationBaseline::Skip),
                critical_mutation_smoke_lanes()[2]
                    .clone()
                    .with_baseline(MutationBaseline::Skip),
                critical_mutation_smoke_lanes()[3]
                    .clone()
                    .with_baseline(MutationBaseline::Skip),
                critical_mutation_smoke_lanes()[4]
                    .clone()
                    .with_baseline(MutationBaseline::Skip),
                critical_mutation_smoke_lanes()[5]
                    .clone()
                    .with_baseline(MutationBaseline::Skip),
                critical_mutation_smoke_lanes()[6]
                    .clone()
                    .with_baseline(MutationBaseline::Skip),
                critical_mutation_smoke_lanes()[7]
                    .clone()
                    .with_baseline(MutationBaseline::Skip),
                critical_mutation_smoke_lanes()[8]
                    .clone()
                    .with_baseline(MutationBaseline::Skip),
                critical_mutation_smoke_lanes()[9]
                    .clone()
                    .with_baseline(MutationBaseline::Skip),
                critical_mutation_smoke_lanes()[10]
                    .clone()
                    .with_baseline(MutationBaseline::Skip),
                MutationLane::repo_wide_smoke(MutantSurface::AllFeatures)
                    .with_baseline(MutationBaseline::Skip),
                MutationLane::repo_wide_smoke(MutantSurface::NoDefaultFeatures)
                    .with_baseline(MutationBaseline::Skip),
            ])
        );
    }

    #[test]
    fn mutants_full_with_overrides_stays_repo_wide_only() {
        let plan = build_mutant_execution_plan(&MutantsArgs {
            mode: MutantMode::Full,
            surface: Some(MutantSurface::AllFeatures),
            shard: Some("3/12".to_owned()),
        })
        .expect("full plan");

        assert_eq!(
            plan,
            MutantExecutionPlan::Run(vec![MutationLane::repo_wide(
                MutantSurface::AllFeatures,
                Some("3/12"),
            )])
        );
    }

    #[test]
    fn mutants_writer_commit_surface_stays_xtask_owned() {
        let lane = critical_mutation_lanes()
            .into_iter()
            .find(|lane| lane.slug == "writer-commit")
            .expect("writer commit seam");
        assert_eq!(
            mutants_command(
                &lane,
                Path::new("tools/xtask/target/mutants/writer-commit-all-features")
            ),
            vec![
                "mutants",
                "--output",
                "tools/xtask/target/mutants/writer-commit-all-features",
                "--in-place",
                "--baseline",
                "run",
                "--file",
                "crates/core/src/store/write/*.rs",
                "--exclude",
                "crates/core/src/store/ancestry/by_clock.rs",
                "--all-features",
                "--cargo-arg",
                "--locked",
                "--test-tool",
                "cargo",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn mutants_full_no_default_surface_stays_xtask_owned() {
        let lane = MutationLane::repo_wide(MutantSurface::NoDefaultFeatures, None);
        assert_eq!(
            mutants_command(
                &lane,
                Path::new("tools/xtask/target/mutants/repo-wide-no-default-features")
            ),
            vec![
                "mutants",
                "--output",
                "tools/xtask/target/mutants/repo-wide-no-default-features",
                "--in-place",
                "--baseline",
                "run",
                "--file",
                "crates/core/src/artifact.rs",
                "--file",
                "crates/core/src/registry.rs",
                "--file",
                "crates/core/src/transition.rs",
                "--file",
                "crates/core/src/reservation.rs",
                "--file",
                "crates/core/src/schema.rs",
                "--file",
                "crates/core/src/store/**/*.rs",
                "--exclude",
                "crates/core/src/store/ancestry/by_hash.rs",
                "--exclude-re",
                INDEX_TOPOLOGY_DEFAULT_EQUIVALENT_MUTANT,
                "--exclude-re",
                SIDX_EMPTY_FOOTER_FLOOR_EQUIVALENT_MUTANT,
                "--no-default-features",
                "--cargo-arg",
                "--locked",
                "--test-tool",
                "cargo",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn mutants_smoke_lane_uses_round_robin_shard_and_skip_baseline_after_first_lane() {
        let lanes = critical_mutation_smoke_lanes();
        assert_eq!(lanes[0].shard.as_deref(), Some("1/8"));
        assert_eq!(lanes[0].sharding, Some(MutationSharding::RoundRobin));
        assert_eq!(
            MutationLane::repo_wide_smoke(MutantSurface::AllFeatures).sharding,
            Some(MutationSharding::RoundRobin)
        );

        let plan = build_mutant_execution_plan(&MutantsArgs {
            mode: MutantMode::Smoke,
            surface: None,
            shard: None,
        })
        .expect("smoke plan");

        let MutantExecutionPlan::Run(lanes) = plan else {
            panic!("expected runnable smoke plan");
        };

        assert_eq!(lanes[0].baseline, MutationBaseline::Run);
        assert!(lanes[1..]
            .iter()
            .all(|lane| lane.baseline == MutationBaseline::Skip));
    }

    #[test]
    fn ratchet_floor_only_advances_to_staged_thresholds() {
        assert_eq!(next_ratchet_floor(34, None), None);
        assert_eq!(next_ratchet_floor(35, None), Some(35));
        assert_eq!(next_ratchet_floor(74, Some(50)), Some(65));
        assert_eq!(next_ratchet_floor(86, Some(75)), Some(85));
    }

    #[test]
    fn current_phase_starts_repo_wide_in_record_only_mode() {
        assert_eq!(REPO_MUTATION_PHASE, RepoMutationPhase::Phase0);
    }

    #[test]
    fn critical_seam_lane_keeps_owned_paths() {
        let lanes = critical_mutation_lanes();
        let cursor_lane = lanes
            .iter()
            .find(|lane| lane.slug == "cursor-delivery")
            .expect("cursor lane");
        let projection_lane = lanes
            .iter()
            .find(|lane| lane.slug == "projection-flow")
            .expect("projection lane");
        let wait_lane = lanes
            .iter()
            .find(|lane| lane.slug == "frontier-wait-durable")
            .expect("frontier wait lane");
        let gate_lane = lanes
            .iter()
            .find(|lane| lane.slug == "frontier-append-gate")
            .expect("frontier append gate lane");
        let registry_lane = lanes
            .iter()
            .find(|lane| lane.slug == "event-payload-registry-validator")
            .expect("event payload registry lane");
        let harness_lane = lanes
            .iter()
            .find(|lane| lane.slug == "harness-ledger-structural-lint")
            .expect("harness lint lane");
        let platform_lane = lanes
            .iter()
            .find(|lane| lane.slug == "platform-backend")
            .expect("platform backend lane");

        assert_eq!(cursor_lane.scope, MutationScope::CriticalSeam);
        assert_eq!(cursor_lane.paths, CURSOR_MUTANT_FILES);
        assert_eq!(projection_lane.paths, PROJECTION_MUTANT_FILES);
        assert_eq!(wait_lane.paths, FRONTIER_WAIT_MUTANT_FILES);
        assert_eq!(gate_lane.paths, FRONTIER_APPEND_GATE_MUTANT_FILES);
        assert_eq!(registry_lane.paths, EVENT_PAYLOAD_REGISTRY_MUTANT_FILES);
        assert_eq!(platform_lane.paths, PLATFORM_BACKEND_MUTANT_FILES);
        assert_eq!(harness_lane.paths, HARNESS_LEDGER_LINT_MUTANT_FILES);
        assert_eq!(harness_lane.package, Some("batpak-integrity"));
        assert_eq!(
            MutationLane::repo_wide(MutantSurface::AllFeatures, None).paths,
            REPO_WIDE_ALL_FEATURES_MUTANT_FILES
        );
        assert_eq!(
            MutationLane::repo_wide(MutantSurface::NoDefaultFeatures, None).paths,
            REPO_WIDE_NO_DEFAULT_MUTANT_FILES
        );
        assert_eq!(
            MutationLane::repo_wide(MutantSurface::AllFeatures, None).excludes,
            surface_excludes(MutantSurface::AllFeatures)
        );
        assert_eq!(lanes[0].paths, WRITER_COMMIT_MUTANT_FILES);
    }

    #[test]
    fn package_scoped_mutation_lane_emits_package_arg() {
        let lane = critical_mutation_lanes()
            .into_iter()
            .find(|lane| lane.slug == "harness-ledger-structural-lint")
            .expect("harness lint seam");
        let command = mutants_command(
            &lane,
            Path::new("tools/xtask/target/mutants/harness-ledger-structural-lint-all-features"),
        );
        let package_index = command
            .iter()
            .position(|arg| arg == "--package")
            .expect("package arg");
        assert_eq!(command[package_index + 1], "batpak-integrity");
    }

    fn fake_lane() -> MutationLane {
        critical_mutation_lanes()
            .into_iter()
            .find(|lane| lane.slug == "writer-commit")
            .expect("writer lane")
    }

    fn fake_output_dir() -> PathBuf {
        PathBuf::from("tools/xtask/target/mutants/fake-lane")
    }

    #[test]
    fn mutation_lane_allows_nonzero_exit_when_unviable_is_execution_evidence() {
        let lane = fake_lane();
        let score = MutationScore {
            caught: 0,
            missed: 0,
            timed_out: 0,
            unviable: 2,
            executed: 2,
            scored: 0,
            score_pct: None,
        };

        assert!(lane.allows_nonzero_exit(score));
    }

    #[test]
    fn critical_mutation_policy_rejects_unviable_only_lane_as_no_scoreable_evidence() {
        let lane = fake_lane();
        let score = MutationScore {
            caught: 0,
            missed: 0,
            timed_out: 0,
            unviable: 3,
            executed: 3,
            scored: 0,
            score_pct: None,
        };

        let err = assert_mutation_policy(&lane, &fake_output_dir(), score).expect_err("must fail");
        assert!(
            err.to_string()
                .contains("no scoreable caught/missed mutants"),
            "threshold lanes must reject unviable-only mutation output, got: {err:#}"
        );
    }

    #[test]
    fn mutation_policy_rejects_truly_empty_execution() {
        let lane = fake_lane();
        let score = MutationScore {
            caught: 0,
            missed: 0,
            timed_out: 0,
            unviable: 0,
            executed: 0,
            scored: 0,
            score_pct: None,
        };

        let err = assert_mutation_policy(&lane, &fake_output_dir(), score).expect_err("must fail");
        assert!(
            err.to_string().contains("no executed mutants"),
            "empty mutation lanes must fail as no-evidence lanes, got: {err:#}"
        );
    }

    #[test]
    fn mutation_score_reads_nested_cargo_mutants_output_dir() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        let output_dir = std::env::temp_dir().join(format!(
            "batpak-xtask-mutation-score-{}-{}",
            std::process::id(),
            unique
        ));
        let results_dir = cargo_mutants_results_dir(&output_dir);
        fs::create_dir_all(&results_dir).expect("create nested mutants.out");
        fs::write(results_dir.join("caught.txt"), "a\nb\n").expect("write caught");
        fs::write(results_dir.join("missed.txt"), "c\n").expect("write missed");
        fs::write(results_dir.join("timeout.txt"), "").expect("write timeout");
        fs::write(results_dir.join("unviable.txt"), "d\n").expect("write unviable");

        let score = mutation_score(&output_dir).expect("score nested output");
        assert_eq!(score.caught, 2);
        assert_eq!(score.missed, 1);
        assert_eq!(score.timed_out, 0);
        assert_eq!(score.unviable, 1);
        assert_eq!(score.executed, 4);
        assert_eq!(score.scored, 3);
        assert_eq!(score.score_pct, Some(66));

        let _ = fs::remove_dir_all(&output_dir);
    }

    #[test]
    fn mutation_policy_timeout_error_points_to_nested_cargo_mutants_output_dir() {
        let lane = fake_lane();
        let score = MutationScore {
            caught: 0,
            missed: 0,
            timed_out: 1,
            unviable: 0,
            executed: 1,
            scored: 0,
            score_pct: None,
        };

        let err = assert_mutation_policy(&lane, &fake_output_dir(), score).expect_err("must fail");
        assert!(
            err.to_string()
                .contains("tools/xtask/target/mutants/fake-lane/mutants.out/timeout.txt"),
            "timeout guidance must point at nested cargo-mutants receipts, got: {err:#}"
        );
    }

    #[test]
    fn mutation_policy_threshold_error_points_to_nested_cargo_mutants_output_dir() {
        let lane = fake_lane();
        let score = MutationScore {
            caught: 0,
            missed: 1,
            timed_out: 0,
            unviable: 0,
            executed: 1,
            scored: 1,
            score_pct: Some(0),
        };

        let err = assert_mutation_policy(&lane, &fake_output_dir(), score).expect_err("must fail");
        assert!(
            err.to_string()
                .contains("tools/xtask/target/mutants/fake-lane/mutants.out/missed.txt"),
            "threshold guidance must point at nested cargo-mutants receipts, got: {err:#}"
        );
    }
}
