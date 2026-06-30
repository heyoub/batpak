use crate::util::cargo_target_dir;
use crate::MutantSurface;
use std::path::PathBuf;

use super::policy::{current_repo_mutation_enforcement, MutationEnforcement};
use super::score::MutationScore;

pub(super) const MUTANTS_OUTPUT_ROOT_LABEL: &str = "$CARGO_TARGET_DIR/xtask-mutants";
pub(super) const CRITICAL_SEAM_MIN_CATCH_PCT: u32 = 85;
pub(super) const REPO_WIDE_SMOKE_SHARD: &str = "0/48";

pub(super) const REPO_WIDE_ALL_FEATURES_MUTANT_FILES: &[&str] = &[
    "crates/core/src/artifact.rs",
    "crates/core/src/registry.rs",
    "crates/core/src/transition.rs",
    "crates/core/src/reservation.rs",
    "crates/core/src/schema.rs",
    "crates/core/src/store/**/*.rs",
    "crates/core/src/wire.rs",
    "crates/core/src/guard/*.rs",
    "crates/core/src/pipeline/*.rs",
];
pub(super) const REPO_WIDE_NO_DEFAULT_MUTANT_FILES: &[&str] = &[
    "crates/core/src/artifact.rs",
    "crates/core/src/registry.rs",
    "crates/core/src/transition.rs",
    "crates/core/src/reservation.rs",
    "crates/core/src/schema.rs",
    "crates/core/src/store/**/*.rs",
];
pub(super) const WRITER_COMMIT_MUTANT_FILES: &[&str] = &[
    "crates/core/src/store/write/**/*.rs",
    "crates/core/src/store/write/control/**/*.rs",
];
pub(super) const CURSOR_MUTANT_FILES: &[&str] = &[
    "crates/core/src/store/delivery/cursor.rs",
    "crates/core/src/store/delivery/observation.rs",
    "crates/core/src/store/reactor_typed.rs",
];
pub(super) const PROJECTION_MUTANT_FILES: &[&str] = &[
    "crates/core/src/store/projection/flow/**/*.rs",
    "crates/core/src/store/projection/registry.rs",
    "crates/core/src/store/projection/mod.rs",
    "crates/core/src/store/projection/watch.rs",
];
pub(super) const PROJECTION_FUSION_MUTANT_FILES: &[&str] = &[
    "crates/core/src/store/projection/flow/fusion.rs",
    "crates/core/src/store/projection/flow/mod.rs",
    "crates/core/src/store/read_api.rs",
];
pub(super) const SEGMENT_SCAN_MUTANT_FILES: &[&str] =
    &["crates/core/src/store/segment/scan/**/*.rs"];
pub(super) const HASH_CHAIN_REPLAY_MUTANT_FILES: &[&str] = &[
    "crates/core/src/store/ancestry/by_hash.rs",
    "crates/core/src/store/cold_start/rebuild.rs",
    "crates/core/src/store/chain_walk.rs",
    "crates/core/src/store/read_walk.rs",
];
pub(super) const FRONTIER_WAIT_MUTANT_FILES: &[&str] = &["crates/core/src/store/write/writer.rs"];
pub(super) const FRONTIER_APPEND_GATE_MUTANT_FILES: &[&str] = &["crates/core/src/store/gate.rs"];
pub(super) const EVENT_PAYLOAD_REGISTRY_MUTANT_FILES: &[&str] = &[
    "crates/core/src/event/payload.rs",
    "crates/core/src/store/config.rs",
    "crates/core/src/store/mod.rs",
];
pub(super) const PLATFORM_BACKEND_MUTANT_FILES: &[&str] = &[
    "crates/core/src/store/platform/**/*.rs",
    "crates/core/src/store/config.rs",
    "crates/core/src/store/mod.rs",
];
pub(super) const TESTING_LEDGER_LINT_MUTANT_FILES: &[&str] =
    &["tools/integrity/src/harness_lints.rs"];
// The integrity graders are the machine that grades everything else, so the
// machine itself is mutation-graded (metacircular). A surviving mutant in any
// of these grader bodies means a verification rule is untested and could be
// silently disabled. Threshold-enforced, never record-only.
pub(super) const INTEGRITY_GRADERS_MUTANT_FILES: &[&str] = &[
    "tools/integrity/src/ci_parity.rs",
    "tools/integrity/src/invariant_bridge.rs",
    "tools/integrity/src/public_surface.rs",
    "tools/integrity/src/structural.rs",
    "tools/integrity/src/typed_waivers.rs",
    "tools/integrity/src/assurance.rs",
    "tools/integrity/src/meta_gate.rs",
];
pub(super) const SYNCBAT_RUNTIME_MUTANT_FILES: &[&str] = &[
    "crates/syncbat/src/admission.rs",
    "crates/syncbat/src/builder.rs",
    "crates/syncbat/src/core.rs",
    "crates/syncbat/src/error.rs",
    "crates/syncbat/src/handler.rs",
    "crates/syncbat/src/operation.rs",
    "crates/syncbat/src/receipt.rs",
    "crates/syncbat/src/store_sink.rs",
];
pub(super) const SYNCBAT_CATALOG_MUTANT_FILES: &[&str] = &[
    "crates/syncbat/src/register.rs",
    "crates/syncbat/src/register_store/**/*.rs",
];
pub(super) const SYNCBAT_SUBSCRIPTION_RUNTIME_MUTANT_FILES: &[&str] = &[
    "crates/syncbat/src/subscription_runtime/**/*.rs",
    "crates/syncbat/src/operation_status.rs",
    "crates/syncbat/src/operation_status_sink.rs",
];
pub(super) const NETBAT_BOUNDARY_MUTANT_FILES: &[&str] = &[
    "crates/netbat/src/lib.rs",
    "crates/netbat/src/route.rs",
    "crates/netbat/src/transport/**/*.rs",
];
// fork seam: the CoW fork decision (active-vs-sealed split) + evidence report.
// fs.rs holds the cow_copy_file ladder but also unrelated helpers, so it is
// proven via targeted `mutants --re` rather than a whole-file seam glob.
pub(super) const FORK_MUTANT_FILES: &[&str] = &[
    "crates/core/src/store/file_classification.rs",
    "crates/core/src/store/fork_report.rs",
];
// import seam: import.rs is fully owned by Store::import_events (key derivation,
// chunking, dedup classification, reserved-kind skip, provenance).
pub(super) const IMPORT_MUTANT_FILES: &[&str] = &["crates/core/src/store/import.rs"];
pub(super) const LANE_BRANCH_MUTANT_FILES: &[&str] = &[
    "crates/core/src/coordinate/mod.rs",
    "crates/core/src/store/append.rs",
    "crates/core/src/store/read_api.rs",
    "crates/core/src/store/index/entry.rs",
    "crates/core/src/store/index/mod.rs",
    "crates/core/src/store/index/query.rs",
    "crates/core/src/store/write/control/submission.rs",
    "crates/core/src/store/write/fanout.rs",
    "crates/core/src/store/write/writer/append.rs",
    "crates/core/src/store/write/writer/batch.rs",
];
// bvisor-admission seam: the fail-closed boundary planner. `plan` probes the
// chosen backend, classifies every requirement, and `admit_one` admits
// (Enforced/Mediated) or fails closed (Unsupported -> PlanError), plus the plan
// canonicalization/identity hash. registry.rs is fully owned by the planner +
// runner; the runner half is covered by the report-seal seam below.
pub(super) const BVISOR_ADMISSION_MUTANT_FILES: &[&str] =
    &["crates/bvisor/src/contract/registry.rs"];
// bvisor-report-seal seam: report sealing. `BoundaryReportBody::body_hash`
// (report.rs) sorts findings, canonical-encodes, and blake3-hashes the body;
// `BoundaryRunner::run` (registry.rs) executes via the bound backend and SEALS
// the observed body, failing closed if sealing cannot canonical-encode.
pub(super) const BVISOR_REPORT_SEAL_MUTANT_FILES: &[&str] = &[
    "crates/bvisor/src/contract/report.rs",
    "crates/bvisor/src/contract/registry.rs",
];
pub(super) const LANE_FRONTIER_MUTANT_FILES: &[&str] = &[
    "crates/core/src/store/hidden_ranges.rs",
    "crates/core/src/store/index/mod.rs",
    "crates/core/src/store/index/projection_bridge.rs",
    "crates/core/src/store/index/query.rs",
    "crates/core/src/store/index/visibility.rs",
    "crates/core/src/store/lifecycle_api.rs",
    "crates/core/src/store/open.rs",
    "crates/core/src/store/projection/flow/fusion.rs",
    "crates/core/src/store/projection/flow/mod.rs",
    "crates/core/src/store/projection/registry.rs",
    "crates/core/src/store/stats.rs",
    "crates/core/src/store/write/writer/publish.rs",
    "crates/core/src/store/write/writer/watermark.rs",
];
// Repo-wide exclusion (category: TIMEOUT/ABORT, not equivalence — the historical
// `EQUIVALENT_MUTANT` name is retained only so meta_gate keeps watching additions
// to this allowlist). `IndexTopology::aos -> Default::default()` is a recursion
// artifact, NOT an observational equivalent: `Default for IndexTopology` returns
// `Self::aos()` (config/types.rs:132-134), so rewriting `aos()`'s body to
// `Default::default()` makes `aos -> default -> aos` recurse until stack abort.
// The abort is caught by cargo-mutants; excluding it just skips the degenerate
// recursion run. PATH FIX: `aos()` is defined in `config/types.rs:57`, NOT
// `config.rs`, so the previous `config\.rs:` anchor matched zero mutants (a
// vacuous exclusion). Anchored to the real definition file below.
pub(super) const INDEX_TOPOLOGY_DEFAULT_EQUIVALENT_MUTANT: &str = r"crates/core/src/store/config/types\.rs:.*replace IndexTopology::aos -> Self with Default::default\(\)";
// Exclusion registry for the import re-application seam. Entries fall into two
// categories, each with its own proof. (Category names below are documentation;
// the typed, ast-anchored category lives in the structured registry consumed by
// the `mutation-exclusion-registry` integrity gate.) The `EQUIVALENT_MUTANTS`
// const name is retained so meta_gate keeps watching additions to this array.
//
// CATEGORY: EQUIVALENT (first-order). No single mutation can change an
// observable (committed state or imported/deduplicated counts).
//
// (a) `< -> ==` / `< -> <=` in `import_events` post-append accounting
//     (`if receipt.global_sequence < pre_import_frontier`): every item that
//     reaches `append_batch` is a genuinely new append, because already-present
//     keys are pre-filtered by `import_key_already_present` BEFORE the batch is
//     built. A fresh append always lands at `pre_import_frontier + 1` or higher,
//     so for every receipt in this loop `global_sequence > pre_import_frontier`
//     — never `==` and never `<`. `<`, `==`, `<=` are therefore all false for
//     every receipt and classify identically (all `imported`). The `deduplicated`
//     arm is unreachable under first-order mutation, so these two are equivalent.
//     EMPIRICAL WITNESS: applying `< -> ==` and running the full import suite
//     (`tests/import_events.rs` + `tests/import_same_store_ceiling.rs`, 12 tests)
//     leaves every test green — no test distinguishes the operators.
// (b) `|| -> &&` in `import_key_already_present`: the post-append `< frontier`
//     reclassification in (a) is a correctness BACKSTOP, not dead code. Under
//     `|| -> &&` the pre-filter stops catching already-present keys, those dups
//     reach `append_batch`, collapse via durable idempotency to their existing
//     (`< frontier`) sequence, and the reclassification arm then counts them as
//     `deduplicated` — so the observable counts are unchanged. The mutant is
//     equivalent BECAUSE the backstop fires; note this is the second-order path
//     that (a)'s arm guards, which is why (a) is unreachable under first-order
//     mutation but the branch is not dead.
//
// CATEGORY: TIMEOUT/ABORT. Mutant changes control flow into non-termination;
// caught by cargo-mutants' timeout, not by an assertion.
//
// (c) `ImportSelector::all -> Self with Default::default()`: `Default for
//     ImportSelector` IS `Self::all()` (import.rs Default impl), so the mutant
//     rewrites `all()` to call its own `Default`, which calls `all()` — unbounded
//     recursion to stack abort. Not an observational equivalent; a degenerate
//     recursion artifact registered to skip the abort run.
pub(super) const IMPORT_EQUIVALENT_MUTANTS: &[&str] = &[
    r"crates/core/src/store/import\.rs:.*replace < with == in import_events",
    r"crates/core/src/store/import\.rs:.*replace < with <= in import_events",
    r"crates/core/src/store/import\.rs:.*replace \|\| with && in import_key_already_present",
    r"crates/core/src/store/import\.rs:.*replace ImportSelector::all -> Self with Default::default",
];
pub(super) const MUTANT_EXCLUDE_RES: &[&str] = &[INDEX_TOPOLOGY_DEFAULT_EQUIVALENT_MUTANT];
// Platform-backend seam: cfg-gated reflink_impl variants. The Linux FICLONE
// reflink_impl (fs.rs:215) IS compiled and IS killed on the Linux CI runner, so
// it is NOT excluded. These entries are the macOS (clonefile) and unsupported
// (Err Unsupported) variants of `reflink_impl`, which are NOT compiled on the
// Linux CI runner, so cargo-mutants can neither apply nor test them there. Their
// cargo-mutants descriptions are identical to the Linux variant, so they are
// distinguished by LINE number (verified against fs.rs):
//   * fs.rs:233 — `#[cfg(target_os = "macos")] reflink_impl -> Ok(())`
//   * fs.rs:252 — the macOS `result == 0` flipped to `!=`
//   * fs.rs:260 — `#[cfg(not(any(linux, macos)))] reflink_impl -> Ok(())`
// The macOS and non-Linux `reflink_impl` cfg variants (fs.rs ~lines 232-265,
// below the Linux FICLONE variant at ~215) are not compiled on the Linux CI
// runner, so cargo-mutants cannot exercise them there. Match the 230-269 line
// band — robust to small line shifts, and never the Linux variant at line 21x.
const PLATFORM_BACKEND_MUTANT_EXCLUDE_RES: &[&str] = &[r"fs\.rs:2[3-6][0-9]:.*reflink_impl"];
// Fork-isolation seam equivalent-mutant registry. Each entry is proven to have
// no observable effect on fork classification; excluding them keeps the
// mutation-score denominator honest. Every entry carries its equivalence proof.
//
// (a) `file_classification.rs:111` match guard
//     `segment_id.as_u64() == active_segment_id -> true` in fork_strategy:
//     `active_segment_id` is always the MAX live segment id (the latest-segment
//     watermark), so NO segment has `id > active`. The first arm takes
//     `id < active`, the second `id == active`, and `id > active` is impossible.
//     Replacing the guard with `true` (i.e. `>= active`) therefore selects the
//     SAME arm for every reachable segment — `== active` and `true` are
//     observationally identical, so the mutant is equivalent.
const FORK_ISOLATION_MUTANT_EXCLUDE_RES: &[&str] = &[
    r"file_classification\.rs:.*replace match guard segment_id.as_u64\(\) == active_segment_id with true in StoreFileKind::fork_strategy",
];
const SEGMENT_SCAN_MUTANT_EXCLUDE_RES: &[&str] = &[];
// No equivalent or unkillable mutants registered for the writer-commit seam.
// The two staging.rs mutants previously excluded here (`PreparedBatch::len -> 0`
// and `+= -> -=` in `push_shared_parts`) were NOT equivalent: both are CAUGHT in
// under a second by the existing unit test
// `store::write::staging::tests::prepared_batch_dedupes_entity_and_scope_strings`
// (it asserts `len() == 3` and `total_bytes() == sum(payload_len)`; `len -> 0`
// fails the count assertion and `+= -> -=` panics on usize subtract overflow at
// the mutation site). They were excluded only because, under the old raw
// `cargo test` runner, an integration path in the SAME test binary hung to the
// lane timeout: `cargo test` never exits until every test finishes, so one hung
// test masked these fast assertions and the whole binary read as a TIMEOUT
// survivor (our policy treats a timeout as a FAILURE, not as caught). The lane
// now runs under `--test-tool nextest` with the `mutants` profile's
// `fail-fast = true` (see run.rs / .config/nextest.toml): the run is convicted the
// instant the fast unit test fails — before any sibling test the mutation
// livelocked can hang it — with terminate-after as a pure-hang backstop. So the
// honest classification really is "killable"
// and these stay un-excluded.
const WRITER_COMMIT_MUTANT_EXCLUDE_RES: &[&str] = &[];
// Equivalent-mutant registry for the projection-flow seam. Each entry is a
// mutant proven to have no observable effect on projection output; excluding
// them keeps the mutation-score denominator honest instead of letting provably
// equivalent mutants drag the gate. Every entry must carry its equivalence proof.
const PROJECTION_MUTANT_EXCLUDE_RES: &[&str] = &[
    // Equivalent mutant: deleting `!` in `result.is_none() && !events.is_empty()`
    // only changes whether a `tracing::debug!` diagnostic is emitted in
    // execute_full_replay — there is no functional behavior to assert without a
    // brittle log-capture test, so the mutant is unkillable by design.
    r"crates/core/src/store/projection/flow/mod\.rs:.*delete ! in execute_full_replay",
    // The guard's three `&&` conjuncts in execute_external_cache_path's
    // incremental-apply gate (`!is_fresh && meta.watermark <= replay.watermark &&
    // supports_incremental_apply() && incremental_projection`) are all killed by
    // external_cache_path_full_replays_for_non_incremental_type and verified
    // CAUGHT by `cargo mutants --re 'replace && with ||'` (3/3 caught), so none
    // are excluded. That test uses a non-incremental, stale Consistent entry:
    // the real guard is false (-> full replay -> 3), while flipping ANY `&&` to
    // `||` makes `!is_fresh` (true) carry the guard, wrongly entering the
    // incremental branch whose no-op apply returns the stale cached 2.
    // The previously-registered :540:26 equivalence exclusion was removed: it was
    // only equivalent for supports_incremental_apply()==true types, and the new
    // non-incremental test proves it is killable, so excluding it was over-broad.
    //
    // No exclusion for the `==` checks in execute_external_cache_path: BOTH are
    // value-/label-affecting and killable.
    //   * :520 (the Consistent `is_fresh` check) is value-affecting.
    //   * :607 (`meta.watermark == execution.replay.watermark`) selects the
    //     reported ProjectionObservedFreshness (Fresh vs StaleAllowed). That label
    //     is NOT log-only: it flows through projection_run::map_observed_freshness
    //     onto `body.observed_freshness` on the project_run_evidence outcome, so
    //     the `== -> !=` mutant is killed by
    //     external_cache_hit_observed_freshness_distinguishes_fresh_from_stale_allowed,
    //     which asserts Fresh (watermarks equal) AND StaleAllowed (watermarks
    //     differ) on that observable field.
    // The value-affecting age comparison here (`age_us < max_stale_ms * 1000`) is
    // pinned by maybe_stale_external_cache_age_boundary_is_pinned.
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MutationScope {
    CriticalSeam,
    RepoWide,
}

impl MutationScope {
    pub(super) fn name(self) -> &'static str {
        match self {
            MutationScope::CriticalSeam => "critical seam",
            MutationScope::RepoWide => "repo-wide",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MutationBaseline {
    Run,
    Skip,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MutationSharding {
    RoundRobin,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CriticalMutationSeam {
    pub(super) slug: &'static str,
    pub(super) label: &'static str,
    pub(super) description: &'static str,
    pub(super) surface: MutantSurface,
    pub(super) package: Option<&'static str>,
    pub(super) paths: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MutationTestAugment {
    /// Additive per-mutant workload: BatPak graduated DST corpus tests.
    GraduatedDstCorpus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct MutationLane {
    pub(super) label: String,
    pub(super) slug: String,
    pub(super) description: &'static str,
    pub(super) scope: MutationScope,
    pub(super) surface: MutantSurface,
    pub(super) baseline: MutationBaseline,
    pub(super) shard: Option<String>,
    pub(super) sharding: Option<MutationSharding>,
    /// When true, this lane scopes mutation to the lines changed in the PR diff
    /// (`cargo mutants --in-diff <patch>`) instead of a content-derived
    /// round-robin shard. The gated mutant population is then deterministic with
    /// respect to the PR rather than drifting on unrelated source edits.
    pub(super) diff_scoped: bool,
    pub(super) enforcement: MutationEnforcement,
    pub(super) package: Option<&'static str>,
    pub(super) paths: &'static [&'static str],
    pub(super) excludes: &'static [&'static str],
    pub(super) exclude_res: &'static [&'static str],
    /// Additive per-mutant test workload beyond the lane's normal seam tests.
    pub(super) test_augments: Vec<MutationTestAugment>,
    /// Extra `--test-package` values cargo-mutants runs per mutant.
    pub(super) test_packages: Vec<&'static str>,
}

impl MutationLane {
    fn critical(seam: CriticalMutationSeam) -> Self {
        Self {
            label: format!("{} ({})", seam.label, surface_name(seam.surface)),
            slug: seam.slug.to_owned(),
            description: seam.description,
            scope: MutationScope::CriticalSeam,
            surface: seam.surface,
            baseline: MutationBaseline::Run,
            shard: None,
            sharding: None,
            diff_scoped: false,
            enforcement: MutationEnforcement::Threshold {
                min_catch_pct: CRITICAL_SEAM_MIN_CATCH_PCT,
            },
            package: seam.package,
            paths: seam.paths,
            excludes: &[],
            exclude_res: critical_seam_exclude_res(seam.slug),
            test_augments: Vec::new(),
            test_packages: Vec::new(),
        }
    }

    fn critical_smoke(seam: CriticalMutationSeam) -> Self {
        Self {
            label: format!(
                "{} ({}, diff-scoped)",
                seam.label,
                surface_name(seam.surface)
            ),
            slug: seam.slug.to_owned(),
            description: seam.description,
            scope: MutationScope::CriticalSeam,
            surface: seam.surface,
            baseline: MutationBaseline::Run,
            // Diff-scoped smoke lanes never carry a fixed fractional shard: the
            // mutant set is the intersection of the seam `--file` globs and the
            // PR diff, so the round-robin slice (and its frontier-append-gate
            // special case) disappear entirely.
            shard: None,
            sharding: None,
            diff_scoped: true,
            enforcement: MutationEnforcement::Threshold {
                min_catch_pct: CRITICAL_SEAM_MIN_CATCH_PCT,
            },
            package: seam.package,
            paths: seam.paths,
            excludes: &[],
            exclude_res: critical_seam_exclude_res(seam.slug),
            test_augments: Vec::new(),
            test_packages: Vec::new(),
        }
    }

    pub(super) fn repo_wide(surface: MutantSurface, shard: Option<&str>) -> Self {
        Self {
            label: match shard {
                Some(shard) => format!("repo-wide ({}, shard {shard})", surface_name(surface)),
                None => format!("repo-wide ({})", surface_name(surface)),
            },
            slug: "repo-wide".to_owned(),
            description: "repo-wide mutation ratchet lane",
            scope: MutationScope::RepoWide,
            surface,
            baseline: MutationBaseline::Run,
            shard: shard.map(str::to_owned),
            sharding: None,
            diff_scoped: false,
            enforcement: current_repo_mutation_enforcement(),
            package: None,
            paths: repo_wide_paths(surface),
            excludes: surface_excludes(surface),
            exclude_res: surface_exclude_res(surface),
            test_augments: Vec::new(),
            test_packages: Vec::new(),
        }
    }

    pub(super) fn repo_wide_smoke(surface: MutantSurface) -> Self {
        Self {
            label: format!(
                "repo-wide ({}, smoke shard {REPO_WIDE_SMOKE_SHARD})",
                surface_name(surface)
            ),
            slug: "repo-wide".to_owned(),
            description: "repo-wide mutation ratchet lane",
            scope: MutationScope::RepoWide,
            surface,
            baseline: MutationBaseline::Run,
            shard: Some(REPO_WIDE_SMOKE_SHARD.to_owned()),
            sharding: Some(MutationSharding::RoundRobin),
            diff_scoped: false,
            enforcement: current_repo_mutation_enforcement(),
            package: None,
            paths: repo_wide_paths(surface),
            excludes: surface_excludes(surface),
            exclude_res: surface_exclude_res(surface),
            test_augments: Vec::new(),
            test_packages: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(super) fn with_baseline(self, baseline: MutationBaseline) -> Self {
        Self { baseline, ..self }
    }

    pub(super) fn output_dir(&self) -> PathBuf {
        mutants_output_root().join(self.slug())
    }

    fn slug(&self) -> String {
        let surface = surface_slug(self.surface);
        match self.shard.as_deref() {
            Some(shard) => format!("{}-{surface}-{}", self.slug, shard.replace('/', "-of-")),
            None => format!("{}-{surface}", self.slug),
        }
    }

    pub(super) fn allows_nonzero_exit(&self, score: MutationScore) -> bool {
        // Every lane is threshold-enforced today; a nonzero exit is only
        // meaningful once at least one mutant actually executed.
        let MutationEnforcement::Threshold { .. } = self.enforcement;
        score.executed > 0
    }

    pub(super) fn policy_line(&self) -> String {
        let MutationEnforcement::Threshold { min_catch_pct } = self.enforcement;
        if self.diff_scoped {
            format!(
                "{} `{}` on {} diff-scoped (--in-diff against PR base): threshold {}%",
                self.scope.name(),
                self.label,
                surface_name(self.surface),
                min_catch_pct,
            )
        } else {
            match self.shard.as_deref() {
                Some(shard) => format!(
                    "{} `{}` on {} shard {shard}: threshold {}%",
                    self.scope.name(),
                    self.label,
                    surface_name(self.surface),
                    min_catch_pct,
                ),
                None => format!(
                    "{} `{}` on {}: threshold {}%",
                    self.scope.name(),
                    self.label,
                    surface_name(self.surface),
                    min_catch_pct,
                ),
            }
        }
    }
}

pub(super) fn mutants_output_root() -> PathBuf {
    cargo_target_dir()
        .unwrap_or_else(|_| PathBuf::from("target"))
        .join("xtask-mutants")
}

pub(super) fn surface_name(surface: MutantSurface) -> &'static str {
    match surface {
        MutantSurface::AllFeatures => "all-features",
        MutantSurface::NoDefaultFeatures => "no-default-features",
    }
}

fn surface_slug(surface: MutantSurface) -> &'static str {
    surface_name(surface)
}

fn repo_wide_paths(surface: MutantSurface) -> &'static [&'static str] {
    match surface {
        MutantSurface::AllFeatures => REPO_WIDE_ALL_FEATURES_MUTANT_FILES,
        MutantSurface::NoDefaultFeatures => REPO_WIDE_NO_DEFAULT_MUTANT_FILES,
    }
}

pub(super) fn surface_excludes(_surface: MutantSurface) -> &'static [&'static str] {
    // blake3 is mandatory, so the surface-specific by_hash/by_clock excludes
    // are gone. The two surfaces only differ by the dangerous-test-hooks
    // feature now, and neither exposes additional file-level excludes.
    &[]
}

fn surface_exclude_res(_surface: MutantSurface) -> &'static [&'static str] {
    MUTANT_EXCLUDE_RES
}

fn critical_seam_exclude_res(slug: &str) -> &'static [&'static str] {
    match slug {
        "segment-scan" => SEGMENT_SCAN_MUTANT_EXCLUDE_RES,
        "writer-commit" => WRITER_COMMIT_MUTANT_EXCLUDE_RES,
        "projection-flow" => PROJECTION_MUTANT_EXCLUDE_RES,
        "import-reapply" => IMPORT_EQUIVALENT_MUTANTS,
        "platform-backend" => PLATFORM_BACKEND_MUTANT_EXCLUDE_RES,
        "fork-isolation" => FORK_ISOLATION_MUTANT_EXCLUDE_RES,
        _ => &[],
    }
}

pub(super) fn critical_mutation_seams() -> &'static [CriticalMutationSeam] {
    &[
        CriticalMutationSeam {
            slug: "writer-commit",
            label: "writer commit protocol",
            description: "writer commit protocol and staging/publish ordering",
            surface: MutantSurface::AllFeatures,
            package: None,
            paths: WRITER_COMMIT_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "cursor-delivery",
            label: "cursor delivery, checkpoints, and witnesses",
            description: "cursor delivery/checkpoint/witness logic",
            surface: MutantSurface::AllFeatures,
            package: None,
            paths: CURSOR_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "projection-flow",
            label: "projection replay and freshness",
            description: "projection replay/freshness logic",
            surface: MutantSurface::AllFeatures,
            package: None,
            paths: PROJECTION_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "projection-fusion",
            label: "projection fusion equivalence",
            description: "fused tuple replay, per-projection filtering, and single-pass read behavior",
            surface: MutantSurface::AllFeatures,
            package: None,
            paths: PROJECTION_FUSION_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "segment-scan",
            label: "segment scan corruption handling",
            description: "segment scan and corruption handling",
            surface: MutantSurface::AllFeatures,
            package: None,
            paths: SEGMENT_SCAN_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "hash-chain-replay",
            label: "hash-chain and replay consistency",
            description: "hash-chain / replay consistency logic",
            surface: MutantSurface::AllFeatures,
            package: None,
            paths: HASH_CHAIN_REPLAY_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "frontier-wait-durable",
            label: "frontier durable wait",
            description: "wait_for_durable poison, target, timeout, and condvar loop",
            surface: MutantSurface::AllFeatures,
            package: None,
            paths: FRONTIER_WAIT_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "frontier-append-gate",
            label: "frontier append gate",
            description: "AppendOptions::gate kind matching, timeout propagation, receipt HLC target conversion, and batch per-item gate ignore",
            surface: MutantSurface::AllFeatures,
            package: None,
            paths: FRONTIER_APPEND_GATE_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "event-payload-registry-validator",
            label: "event payload registry validator",
            description: "EventPayload registry collision detection, open-time warn/fail-fast policy, and cache refresh",
            surface: MutantSurface::AllFeatures,
            package: None,
            paths: EVENT_PAYLOAD_REGISTRY_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "platform-backend",
            label: "platform backend admission and reverify",
            description: "platform evidence, admission tokens, profile parsing, and reverify fail-closed behavior",
            surface: MutantSurface::AllFeatures,
            package: None,
            paths: PLATFORM_BACKEND_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "testing-ledger-structural-lint",
            label: "testing ledger structural lint",
            description: "testing ledger schema, location, command, header, and line-cap enforcement",
            surface: MutantSurface::AllFeatures,
            package: Some("batpak-integrity"),
            paths: TESTING_LEDGER_LINT_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "integrity-graders",
            label: "integrity graders (grade the graders)",
            description: "ci-parity, invariant-bridge, public-surface, structural, typed-waiver, and assurance grader logic — the verification machine is itself mutation-graded",
            surface: MutantSurface::AllFeatures,
            package: Some("batpak-integrity"),
            paths: INTEGRITY_GRADERS_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "syncbat-runtime-dispatch",
            label: "syncbat runtime dispatch and receipts",
            description: "syncbat build, dispatch, pre-handler admission guard (deny -> Denied receipt), Ctx receipt-metadata collector, handler failure, and receipt sink semantics",
            surface: MutantSurface::AllFeatures,
            package: Some("syncbat"),
            paths: SYNCBAT_RUNTIME_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "syncbat-register-catalog",
            label: "syncbat durable register catalog",
            description: "syncbat descriptor validation, catalog row lifecycle, and deterministic rebuild",
            surface: MutantSurface::AllFeatures,
            package: Some("syncbat"),
            paths: SYNCBAT_CATALOG_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "syncbat-subscription-runtime",
            label: "syncbat subscription event stream runtime",
            description: "syncbat subscription registry, cursor v1, replay/live wake, ACK/backpressure, watermark, and delivery envelopes",
            surface: MutantSurface::AllFeatures,
            package: Some("syncbat"),
            paths: SYNCBAT_SUBSCRIPTION_RUNTIME_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "netbat-boundary-protocol",
            label: "netbat boundary protocol",
            description: "netbat request/response framing, limit checks, error mapping, and syncbat dispatch boundary",
            surface: MutantSurface::AllFeatures,
            package: Some("netbat"),
            paths: NETBAT_BOUNDARY_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "fork-isolation",
            label: "fork CoW isolation classification",
            description: "fork strategy active-vs-sealed split, share-vs-deep-copy classification, and fork evidence report",
            surface: MutantSurface::AllFeatures,
            package: None,
            paths: FORK_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "import-reapply",
            label: "import re-application idempotency",
            description: "import key derivation, all-or-nothing chunking, dedup-vs-import classification, reserved-kind skip, and provenance",
            surface: MutantSurface::AllFeatures,
            package: None,
            paths: IMPORT_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "lane-branch",
            label: "lane branch isolation",
            description: "per-(entity,lane) heads, writer branch-root positions, lane-scoped reads, and lane-filtered fanout",
            surface: MutantSurface::AllFeatures,
            package: None,
            paths: LANE_BRANCH_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "lane-frontier",
            label: "per-lane frontier visibility",
            description: "per-lane watermark lattice, sequence-gate lane cursors, lane waits, projection applied progress, and cold-start lane bootstrap",
            surface: MutantSurface::AllFeatures,
            package: None,
            paths: LANE_FRONTIER_MUTANT_FILES,
        },
        // bvisor C1 seams. `--all-features` enables `dangerous-test-hooks`, which
        // compiles the SimBackend monster + GroundTruth oracle that catch the
        // mutants. `bvisor-policy-lowering` is intentionally DEFERRED to C2: the
        // real backend policy lowering does not exist yet, so a seam glob for it
        // would match no code and vacuously pass — it is added when C2 lands.
        CriticalMutationSeam {
            slug: "bvisor-admission",
            label: "bvisor fail-closed boundary admission",
            description: "boundary planner probe/classify, fail-closed admit_one (Unsupported -> PlanError), and canonical plan identity hashing",
            surface: MutantSurface::AllFeatures,
            package: Some("bvisor"),
            paths: BVISOR_ADMISSION_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "bvisor-report-seal",
            label: "bvisor report sealing",
            description: "report body_hash canonicalization (sort findings, canonical encode, blake3) and the runner's seal-or-fail-closed execution path",
            surface: MutantSurface::AllFeatures,
            package: Some("bvisor"),
            paths: BVISOR_REPORT_SEAL_MUTANT_FILES,
        },
    ]
}

pub(super) fn critical_mutation_lanes() -> Vec<MutationLane> {
    critical_mutation_seams()
        .iter()
        .copied()
        .map(MutationLane::critical)
        .collect()
}

pub(super) fn critical_mutation_smoke_lanes() -> Vec<MutationLane> {
    critical_mutation_seams()
        .iter()
        .copied()
        .map(MutationLane::critical_smoke)
        .collect()
}

pub(super) fn critical_mutation_smoke_lane_for_seam(slug: &str) -> Option<MutationLane> {
    critical_mutation_seams()
        .iter()
        .copied()
        .find(|seam| seam.slug == slug)
        .map(MutationLane::critical_smoke)
}

pub(super) fn critical_seam_slugs() -> Vec<&'static str> {
    critical_mutation_seams()
        .iter()
        .map(|seam| seam.slug)
        .collect()
}

pub(super) fn repo_wide_mutation_lanes(
    surfaces: Vec<MutantSurface>,
    shard: Option<&str>,
) -> Vec<MutationLane> {
    surfaces
        .into_iter()
        .map(|surface| MutationLane::repo_wide(surface, shard))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::critical_seam_slugs;
    use std::collections::BTreeSet;

    /// Anti-fragility guard: the hard-coded `seam:` matrix in the CI workflow must
    /// stay in lockstep with `critical_mutation_seams()`. Without this coupling a
    /// seam added in one place but not the other silently goes ungated — exactly
    /// how the 0.9.0 fork/import seams were initially missing from the CI matrix.
    #[test]
    fn ci_mutation_seam_matrix_matches_registry() {
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("xtask lives at <repo>/bpk-lib/tools/xtask; three parents reach the repo root");
        let ci_yml = repo_root.join(".github/workflows/ci.yml");
        let text = std::fs::read_to_string(&ci_yml)
            .expect("read .github/workflows/ci.yml for the seam-matrix drift guard");

        // Collect the `- <slug>` entries directly under the single `seam:` matrix key.
        let mut ci_seams: BTreeSet<String> = BTreeSet::new();
        let mut in_seam_list = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed == "seam:" {
                in_seam_list = true;
                continue;
            }
            if in_seam_list {
                if let Some(slug) = trimmed.strip_prefix("- ") {
                    ci_seams.insert(slug.trim().to_owned());
                } else if !trimmed.is_empty() {
                    break;
                }
            }
        }
        assert!(
            !ci_seams.is_empty(),
            "no `seam:` matrix entries found in {} — the drift guard needs the matrix",
            ci_yml.display()
        );

        let registry: BTreeSet<String> = critical_seam_slugs()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let missing_in_ci: Vec<&String> = registry.difference(&ci_seams).collect();
        let missing_in_registry: Vec<&String> = ci_seams.difference(&registry).collect();
        assert!(
            missing_in_ci.is_empty() && missing_in_registry.is_empty(),
            "CI mutation `seam:` matrix drifted from critical_mutation_seams().\n  \
             in registry but missing from ci.yml: {missing_in_ci:?}\n  \
             in ci.yml but missing from registry: {missing_in_registry:?}"
        );
    }

    /// #64-D single-source guard: xtask is now a real CONSUMER of the
    /// authoritative `traceability/seam_registry.yaml`.
    ///
    /// The registry was authoritative on the integrity side only
    /// (`assurance::check_seam_registry_lockstep`); xtask carried an
    /// independent in-code array with zero registry references, so a registry
    /// seam that xtask did not actually grade could drift in silently. This
    /// test closes that consumer gap: every assurance-leveled seam declared in
    /// the registry MUST be a real `critical_mutation_seams()` slug — the
    /// registry cannot name a seam the mutation tooling does not run.
    ///
    /// Direction is deliberate (registry ⊆ xtask, not equality): the registry
    /// tracks the 15 assurance-LEVELED seams (one per `assurance_levels.yaml`
    /// entry), while `critical_mutation_seams()` additionally carries extra
    /// smoke-only mutation lanes (e.g. `lane-branch`, `integrity-graders`,
    /// `bvisor-*`) that have a lane but no formal assurance level. Demanding
    /// equality would falsely force those lanes into the assurance manifest.
    /// Glob-granularity reconciliation between the registry and the xtask
    /// `*_MUTANT_FILES` consts (the registry's globs are coarser by design) is
    /// a separate concern owned by `assurance::check_seam_registry_lockstep`.
    #[test]
    fn seam_registry_seams_are_real_xtask_seams() {
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("xtask lives at <repo>/bpk-lib/tools/xtask; two parents reach the workspace");
        let registry_yaml = repo_root.join("traceability/seam_registry.yaml");
        let text = std::fs::read_to_string(&registry_yaml).expect(
            "read traceability/seam_registry.yaml for the seam-registry single-source guard",
        );

        // The schema is one flat entry per seam: a top-level `- slug: <name>`
        // line. Parse those slugs directly — no new YAML dependency, the same
        // lightweight approach the ci.yml guard uses.
        let mut registry_seams: BTreeSet<String> = BTreeSet::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("- slug:") {
                registry_seams.insert(rest.trim().to_owned());
            }
        }
        assert!(
            !registry_seams.is_empty(),
            "no `- slug:` entries parsed from {} — the single-source guard needs the registry",
            registry_yaml.display()
        );

        let xtask_seams: BTreeSet<String> = critical_seam_slugs()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let registry_only: Vec<&String> = registry_seams.difference(&xtask_seams).collect();
        assert!(
            registry_only.is_empty(),
            "seam_registry.yaml names seam(s) that critical_mutation_seams() does not grade: \
             {registry_only:?}.\n  The registry is the single source for assurance-leveled \
             seams; every registry slug must map to a real xtask mutation seam."
        );
    }
}
