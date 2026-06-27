use anyhow::{bail, Result};
use std::path::Path;

use super::dst_corpus::{load_dst_coverage_seams, DST_CORPUS_TEST_PACKAGE};
use super::lanes::{
    critical_mutation_seams, critical_mutation_smoke_lanes, surface_name,
    CRITICAL_SEAM_MIN_CATCH_PCT, MUTANTS_OUTPUT_ROOT_LABEL, MUTANT_EXCLUDE_RES,
    REPO_WIDE_ALL_FEATURES_MUTANT_FILES, REPO_WIDE_NO_DEFAULT_MUTANT_FILES, REPO_WIDE_SMOKE_SHARD,
};
use super::lanes::{MutationLane, MutationScope};
use super::score::{cargo_mutants_receipt_path, MutationScore};
use crate::MutantSurface;

// GAUNT-MUT-4: flipped off Phase0 (RecordOnly = never fails) to make the repo-wide
// mutation lane BLOCKING at a real floor. The audit estimates ~75% live mutants, so
// we set Phase4 (floor 75%). This floor is PROVISIONAL pending the first cloud
// repo-wide smoke confirmation — if the cloud shows the measured score is below 75,
// it is a one-line drop to the highest phase <= measured. The ratchet is monotonic:
// the floor only ever climbs from here (`next_ratchet_floor` advertises the next raise).
pub(super) const REPO_MUTATION_PHASE: RepoMutationPhase = RepoMutationPhase::Phase4;
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
}

/// Whether a diff-scoped lane was actually scoped by a real, non-empty PR diff.
///
/// The zero-mutant early return ("the PR touched no mutable lines in this seam")
/// is only a legitimate PASS when an actual PR diff was applied. On a manual
/// `workflow_dispatch`/local run there is no PR, so `resolve_smoke_diff` falls
/// back to `origin/main..HEAD`, which is EMPTY on the default branch — every
/// diff-scoped seam then sees zero mutants. Treating that as a PASS would let a
/// manual mutation proof skip all critical-seam threshold gates, so an
/// empty/absent diff must NOT qualify for the zero-mutant pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DiffScope {
    /// A real, non-empty PR diff scoped this lane (`--in-diff <patch>` applied).
    PrDiff,
    /// No diff scope was applied — either the lane is not diff-scoped, or the
    /// resolved diff was empty/absent (manual dispatch / default branch).
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RepoMutationPhase {
    Phase1,
    Phase2,
    Phase3,
    Phase4,
    Phase5,
}

/// The blocking floor for a repo-wide ratchet phase. Every phase carries a real,
/// non-zero floor — the floor-less record-only sentinel was removed when the
/// repo-wide lane became unconditionally BLOCKING (GAUNT-MUT-4) — so this is an
/// exhaustive, infallible lookup. `REPO_MUTATION_THRESHOLDS` mirrors the same
/// pairs for policy printing and ratchet display; the two must stay in sync.
fn current_repo_mutation_floor() -> u32 {
    match REPO_MUTATION_PHASE {
        RepoMutationPhase::Phase1 => 35,
        RepoMutationPhase::Phase2 => 50,
        RepoMutationPhase::Phase3 => 65,
        RepoMutationPhase::Phase4 => 75,
        RepoMutationPhase::Phase5 => 85,
    }
}

pub(super) fn current_repo_mutation_enforcement() -> MutationEnforcement {
    MutationEnforcement::Threshold {
        min_catch_pct: current_repo_mutation_floor(),
    }
}

pub(super) fn assert_mutation_policy(
    lane: &MutationLane,
    output_dir: &Path,
    score: MutationScore,
    diff_scope: DiffScope,
) -> Result<()> {
    if lane.diff_scoped && diff_scope == DiffScope::PrDiff && score.executed == 0 {
        // A diff-scoped lane scoped by a REAL PR diff that produced zero mutants
        // means the PR touched no mutable lines inside this seam's --file globs.
        // That is a legitimate PASS: there is nothing for the gate to prove.
        //
        // This is gated on BOTH `diff_scoped` AND `DiffScope::PrDiff`: a manual
        // `workflow_dispatch`/local run has no PR, so `resolve_smoke_diff` falls
        // back to `origin/main..HEAD`, which is EMPTY on the default branch. That
        // empty diff is reported as `DiffScope::None`, so it falls THROUGH to the
        // hard-fail below instead of silently passing every critical seam with
        // zero mutants — which would let a manual mutation proof skip all
        // critical-seam threshold gates.
        outln!(
            "mutants: `{}` => no mutable lines in PR diff for this seam; nothing to prove.",
            lane.label
        );
        return Ok(());
    }

    if score.executed == 0 {
        bail!(
            "mutants: `{}` produced no executed mutants in {}. Treating this as a failure because \
             the mutation surface produced no evidence.",
            lane.label,
            output_dir.display()
        );
    }

    match score.score_pct {
        Some(score_pct) => outln!(
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
        None => outln!(
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
        outln!(
            "mutants: `{}` recorded {} unviable mutants in {}.",
            lane.label,
            score.unviable,
            cargo_mutants_receipt_path(output_dir, "unviable.txt").display()
        );
    }

    let MutationEnforcement::Threshold { min_catch_pct } = lane.enforcement;
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
            outln!(
                "mutants: `{}` is above the current repo-wide ratchet floor; a future raise to {}% is available.",
                lane.label, next_floor
            );
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
    outln!("Mutation policy:");
    outln!(
        "- `cargo xtask mutants smoke`: run diff-scoped (--in-diff against PR base) mutation of every critical seam at {}%, then repo-wide {} lanes using the current ratchet phase. Only the first lane runs a fresh baseline; later lanes reuse it with `--baseline skip`.",
        CRITICAL_SEAM_MIN_CATCH_PCT,
        REPO_WIDE_SMOKE_SHARD,
    );
    outln!(
        "- Critical seams with `dst_coverage: true` in traceability/seam_registry.yaml also run the graduated DST corpus additively in the per-mutant workload via `--test-package {DST_CORPUS_TEST_PACKAGE}` (including `dst_corpus_currency`); the normal seam mutation files and tests remain in the lane."
    );
    if let Ok(dst_seams) = load_dst_coverage_seams() {
        outln!(
            "- DST corpus-backed critical seams today: {}.",
            dst_seams.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    outln!(
        "- `cargo xtask mutants full`: with no overrides, run the full policy; with `--surface` and/or `--shard`, run only the requested repo-wide ratchet lane."
    );
    outln!(
        "- Repo-wide ratchet phase: {:?} (current floor: {}%).",
        REPO_MUTATION_PHASE,
        current_repo_mutation_floor()
    );
    outln!("- Repo-wide ratchet phases staged in code:");
    for (phase, floor) in REPO_MUTATION_THRESHOLDS {
        outln!("  {:?} => {floor}%", phase);
    }
    for lane in critical_mutation_smoke_lanes() {
        outln!("- {}", lane.policy_line());
    }
    for lane in [
        MutationLane::repo_wide_smoke(MutantSurface::AllFeatures),
        MutationLane::repo_wide_smoke(MutantSurface::NoDefaultFeatures),
    ] {
        outln!("- {}", lane.policy_line());
    }
    outln!("- Critical seam surfaces:");
    for seam in critical_mutation_seams() {
        outln!(
            "  {} [{} on {}]: {}",
            seam.label,
            seam.slug,
            surface_name(seam.surface),
            seam.description
        );
        for pattern in seam.paths {
            outln!("    {pattern}");
        }
    }
    outln!("- Repo-wide patterns:");
    for pattern in REPO_WIDE_ALL_FEATURES_MUTANT_FILES {
        outln!("  all-features: {pattern}");
    }
    for pattern in REPO_WIDE_NO_DEFAULT_MUTANT_FILES {
        outln!("  no-default-features: {pattern}");
    }
    outln!(
        "- Mutation regex excludes: {}.",
        MUTANT_EXCLUDE_RES.join(", ")
    );
    outln!(
        "- Mutation artifacts live under `{MUTANTS_OUTPUT_ROOT_LABEL}` so xtask owns the scratch surface."
    );
}
