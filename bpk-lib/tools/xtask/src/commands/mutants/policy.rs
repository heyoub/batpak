use anyhow::{bail, Result};
use std::path::Path;

use super::lanes::{
    critical_mutation_seams, critical_mutation_smoke_lanes, surface_name,
    ALL_FEATURES_MUTANT_EXCLUDES, ALL_FEATURES_MUTANT_EXCLUDE_RES, CRITICAL_SEAM_MIN_CATCH_PCT,
    MUTANTS_OUTPUT_ROOT_LABEL, NO_DEFAULT_FEATURES_MUTANT_EXCLUDES,
    NO_DEFAULT_FEATURES_MUTANT_EXCLUDE_RES, REPO_WIDE_ALL_FEATURES_MUTANT_FILES,
    REPO_WIDE_NO_DEFAULT_MUTANT_FILES, REPO_WIDE_SMOKE_SHARD,
};
use super::lanes::{MutationLane, MutationScope};
use super::score::{cargo_mutants_receipt_path, MutationScore};
use crate::MutantSurface;

pub(super) const REPO_MUTATION_PHASE: RepoMutationPhase = RepoMutationPhase::Phase0;
pub(super) const REPO_MUTATION_THRESHOLDS: &[(RepoMutationPhase, u32)] = &[
    (RepoMutationPhase::Phase1, 35),
    (RepoMutationPhase::Phase2, 50),
    (RepoMutationPhase::Phase3, 65),
    (RepoMutationPhase::Phase4, 75),
    (RepoMutationPhase::Phase5, 85),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MutationEnforcement {
    Threshold { min_catch_pct: u32 },
    RecordOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RepoMutationPhase {
    Phase0,
    Phase1,
    Phase2,
    Phase3,
    Phase4,
    Phase5,
}

fn current_repo_mutation_floor() -> Option<u32> {
    match REPO_MUTATION_PHASE {
        RepoMutationPhase::Phase0 => None,
        RepoMutationPhase::Phase1 => Some(35),
        RepoMutationPhase::Phase2 => Some(50),
        RepoMutationPhase::Phase3 => Some(65),
        RepoMutationPhase::Phase4 => Some(75),
        RepoMutationPhase::Phase5 => Some(85),
    }
}

pub(super) fn current_repo_mutation_enforcement() -> MutationEnforcement {
    match current_repo_mutation_floor() {
        Some(min_catch_pct) => MutationEnforcement::Threshold { min_catch_pct },
        None => MutationEnforcement::RecordOnly,
    }
}

pub(super) fn assert_mutation_policy(
    lane: &MutationLane,
    output_dir: &Path,
    score: MutationScore,
) -> Result<()> {
    if score.executed == 0 {
        bail!(
            "mutants: `{}` produced no executed mutants in {}. Treating this as a failure because \
             the mutation surface produced no evidence.",
            lane.label,
            output_dir.display()
        );
    }

    match score.score_pct {
        Some(score_pct) => println!(
            "mutants: `{}` => {} caught / {} scored = {}% (executed: {}, missed: {}, timed out: {}, unviable: {})",
            lane.label,
            score.caught,
            score.scored,
            score_pct,
            score.executed,
            score.missed,
            score.timed_out,
            score.unviable,
        ),
        None => println!(
            "mutants: `{}` => no scoreable mutants (executed: {}, timed out: {}, unviable: {})",
            lane.label,
            score.executed,
            score.timed_out,
            score.unviable,
        ),
    }

    if score.timed_out > 0 {
        bail!(
            "mutation lane `{}` timed out on {} mutants. Investigate {}.",
            lane.label,
            score.timed_out,
            cargo_mutants_receipt_path(output_dir, "timeout.txt").display()
        );
    }

    if score.unviable > 0 {
        println!(
            "mutants: `{}` recorded {} unviable mutants in {}.",
            lane.label,
            score.unviable,
            cargo_mutants_receipt_path(output_dir, "unviable.txt").display()
        );
    }

    match lane.enforcement {
        MutationEnforcement::Threshold { min_catch_pct } => {
            let Some(score_pct) = score.score_pct else {
                bail!(
                    "mutation lane `{}` produced no scoreable caught/missed mutants \
                     ({} executed total; {} unviable). Threshold gates require at least one \
                     scoreable mutant so unviable-only output cannot pass as evidence.",
                    lane.label,
                    score.executed,
                    score.unviable,
                );
            };
            if score_pct < min_catch_pct as usize {
                bail!(
                    "mutation score for `{}` is {}%, below the required {}% \
                     ({} caught, {} missed out of {} scored mutants; {} executed total). Add tests that catch the \
                     mutations listed in {}.",
                    lane.label,
                    score_pct,
                    min_catch_pct,
                    score.caught,
                    score.missed,
                    score.scored,
                    score.executed,
                    cargo_mutants_receipt_path(output_dir, "missed.txt").display()
                );
            }
            if lane.scope == MutationScope::RepoWide {
                if let Some(next_floor) = next_ratchet_floor(score_pct, Some(min_catch_pct)) {
                    println!(
                        "mutants: `{}` is above the current repo-wide ratchet floor; a future raise to {}% is available.",
                        lane.label, next_floor
                    );
                }
            }
        }
        MutationEnforcement::RecordOnly => {
            let Some(score_pct) = score.score_pct else {
                println!(
                    "mutants: `{}` produced execution evidence but no scoreable caught/missed mutants, so ratchet math is not applied for this record-only lane.",
                    lane.label
                );
                return Ok(());
            };
            if let Some(next_floor) = next_ratchet_floor(score_pct, None) {
                println!(
                    "mutants: `{}` is in repo-wide record-only mode for this phase. Current score {}% supports a future ratchet to {}%.",
                    lane.label, score_pct, next_floor
                );
            }
        }
    }

    Ok(())
}

pub(super) fn next_ratchet_floor(score_pct: usize, current_floor: Option<u32>) -> Option<u32> {
    REPO_MUTATION_THRESHOLDS
        .iter()
        .map(|(_, floor)| *floor)
        .filter(|floor| Some(*floor) > current_floor && score_pct >= *floor as usize)
        .max()
}

pub(super) fn print_mutation_policy() {
    println!("Mutation policy:");
    println!(
        "- `cargo xtask mutants smoke`: run representative round-robin shards of every critical seam at {}%, then repo-wide {} lanes using the current ratchet phase. Only the first lane runs a fresh baseline; later lanes reuse it with `--baseline skip`.",
        CRITICAL_SEAM_MIN_CATCH_PCT,
        REPO_WIDE_SMOKE_SHARD,
    );
    println!(
        "- `cargo xtask mutants full`: with no overrides, run the full policy; with `--surface` and/or `--shard`, run only the requested repo-wide ratchet lane."
    );
    match current_repo_mutation_floor() {
        Some(floor) => println!(
            "- Repo-wide ratchet phase: {:?} (current floor: {floor}%).",
            REPO_MUTATION_PHASE
        ),
        None => println!(
            "- Repo-wide ratchet phase: {:?} (record-only; no floor enforced yet).",
            REPO_MUTATION_PHASE
        ),
    }
    println!("- Repo-wide ratchet phases staged in code:");
    for (phase, floor) in REPO_MUTATION_THRESHOLDS {
        println!("  {:?} => {floor}%", phase);
    }
    for lane in critical_mutation_smoke_lanes() {
        println!("- {}", lane.policy_line());
    }
    for lane in [
        MutationLane::repo_wide_smoke(MutantSurface::AllFeatures),
        MutationLane::repo_wide_smoke(MutantSurface::NoDefaultFeatures),
    ] {
        println!("- {}", lane.policy_line());
    }
    println!("- Critical seam surfaces:");
    for seam in critical_mutation_seams() {
        println!(
            "  {} [{} on {}]: {}",
            seam.label,
            seam.slug,
            surface_name(seam.surface),
            seam.description
        );
        for pattern in seam.paths {
            println!("    {pattern}");
        }
    }
    println!("- Repo-wide patterns:");
    for pattern in REPO_WIDE_ALL_FEATURES_MUTANT_FILES {
        println!("  all-features: {pattern}");
    }
    for pattern in REPO_WIDE_NO_DEFAULT_MUTANT_FILES {
        println!("  no-default-features: {pattern}");
    }
    println!(
        "- Surface-specific excludes: all-features => {}, no-default-features => {}.",
        ALL_FEATURES_MUTANT_EXCLUDES.join(", "),
        NO_DEFAULT_FEATURES_MUTANT_EXCLUDES.join(", ")
    );
    println!(
        "- Surface-specific mutation regex excludes: all-features => {}, no-default-features => {}.",
        ALL_FEATURES_MUTANT_EXCLUDE_RES.join(", "),
        NO_DEFAULT_FEATURES_MUTANT_EXCLUDE_RES.join(", ")
    );
    println!(
        "- Mutation artifacts live under `{MUTANTS_OUTPUT_ROOT_LABEL}` so xtask owns the scratch surface."
    );
}
