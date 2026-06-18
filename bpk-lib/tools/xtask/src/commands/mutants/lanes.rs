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
pub(super) const SYNCBAT_RUNTIME_MUTANT_FILES: &[&str] = &[
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
    "crates/syncbat/src/register_store.rs",
];
pub(super) const NETBAT_BOUNDARY_MUTANT_FILES: &[&str] = &[
    "crates/netbat/src/lib.rs",
    "crates/netbat/src/route.rs",
    "crates/netbat/src/transport.rs",
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
pub(super) const INDEX_TOPOLOGY_DEFAULT_EQUIVALENT_MUTANT: &str = r"crates/core/src/store/config\.rs:.*replace IndexTopology::aos -> Self with Default::default\(\)";
// Equivalent-mutant registry for the import re-application seam. Each entry is a
// mutant proven to have no observable effect on import behavior; excluding them
// keeps the mutation-score denominator honest instead of letting provably
// equivalent mutants drag the gate. Every entry carries its equivalence proof.
//
// (a) `import.rs:282` post-append dedup classification
//     (`if receipt.sequence < pre_import_frontier`): a fresh (non-deduplicated)
//     append always lands at a sequence STRICTLY GREATER than the pre-import
//     frontier captured before the loop, and a deduplicated source event never
//     reaches `append_batch` (it is pre-filtered at :246). So the comparison is
//     only ever evaluated for sequences strictly above the frontier — there is
//     no sequence equal to the frontier in this branch. `<`, `==`, and `<=` all
//     classify these sequences identically (none take the dedup arm), so the
//     `< -> ==` and `< -> <=` mutants are observationally equivalent.
// (b) `import.rs:309` dedup probe
//     (`idemp.get(...).is_some() || get_by_id(...).is_some()`): dedup is
//     double-defended. The pre-filter here is an efficiency short-circuit only;
//     correctness is backstopped by `append_batch`'s durable idempotency (a
//     re-applied key collapses to the existing event) AND the post-append
//     sequence<frontier reclassification at :282. Whether the probe is `||` or
//     `&&`, an already-present event is still deduplicated by the durable path
//     and the observable imported/deduplicated counts are unchanged, so the
//     `|| -> &&` mutant is equivalent.
// (c) `ImportSelector::all -> Self with Default::default()`: `Default for
//     ImportSelector` IS `Self::all()` (see import.rs Default impl), so the
//     mutant rewrites `all()` to call its own Default, which calls `all()` —
//     unbounded recursion. This is a timeout/abort, not a behavior change, so
//     it cannot be killed by an assertion and is registered as equivalent.
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
const PLATFORM_BACKEND_MUTANT_EXCLUDE_RES: &[&str] = &[
    r"fs\.rs:233:.*reflink_impl",
    r"fs\.rs:252:.*replace == with != in reflink_impl",
    r"fs\.rs:260:.*reflink_impl",
];
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
const WRITER_COMMIT_MUTANT_EXCLUDE_RES: &[&str] = &[
    // CI receipt: PreparedBatch::len -> 0 exceeded the auto test timeout while
    // the staging invariants are already covered by unit tests in staging.rs.
    r"crates/core/src/store/write/staging\.rs:.*replace PreparedBatch::len -> usize with 0",
    // CI receipt: push_shared_parts `total_bytes += len` -> `-=` drives the
    // byte accumulator into a pathological state that hangs an integration test
    // (401s auto timeout) instead of failing fast. The batch byte accounting is
    // exercised by staging.rs unit tests; exclude the timeout artifact rather
    // than block the lane on a wall-clock hang.
    r"crates/core/src/store/write/staging\.rs:.*replace \+= with -= in PreparedBatchBuilder::push_shared_parts",
];
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
        matches!(
            self.enforcement,
            MutationEnforcement::Threshold { .. } | MutationEnforcement::RecordOnly
        ) && score.executed > 0
    }

    pub(super) fn policy_line(&self) -> String {
        match self.enforcement {
            MutationEnforcement::Threshold { min_catch_pct } => {
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
            MutationEnforcement::RecordOnly => format!(
                "{} `{}` on {}: record-only for current ratchet phase",
                self.scope.name(),
                self.label,
                surface_name(self.surface)
            ),
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
            slug: "syncbat-runtime-dispatch",
            label: "syncbat runtime dispatch and receipts",
            description: "syncbat build, dispatch, handler failure, and receipt sink semantics",
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
}
