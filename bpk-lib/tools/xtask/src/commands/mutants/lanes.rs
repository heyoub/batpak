use crate::util::cargo_target_dir;
use crate::MutantSurface;
use std::path::PathBuf;

use super::policy::{current_repo_mutation_enforcement, MutationEnforcement};
use super::score::MutationScore;

pub(super) const MUTANTS_OUTPUT_ROOT_LABEL: &str = "$CARGO_TARGET_DIR/xtask-mutants";
pub(super) const CRITICAL_SEAM_MIN_CATCH_PCT: u32 = 85;
pub(super) const CRITICAL_SMOKE_SHARD: &str = "0/8";
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
pub(super) const INDEX_TOPOLOGY_DEFAULT_EQUIVALENT_MUTANT: &str = r"crates/core/src/store/config\.rs:.*replace IndexTopology::aos -> Self with Default::default\(\)";
pub(super) const MUTANT_EXCLUDE_RES: &[&str] = &[INDEX_TOPOLOGY_DEFAULT_EQUIVALENT_MUTANT];
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
    // Equivalent mutant: the FIRST `&&` (col 26) in execute_external_cache_path's
    // `!is_fresh && supports_incremental_apply && incremental_projection` guard.
    // Flipping it to `||` only diverges when the cache entry IS fresh: the real
    // guard skips the incremental branch, the mutant enters it — but on a fresh
    // entry the incremental fold filters `global_sequence > cached_watermark`,
    // which selects zero events, so the returned projection value is identical.
    // ANCHORED to :540:26 on purpose — the SECOND `&&` (col 61, the
    // `&& incremental_projection` conjunct) is NOT equivalent: flipping it runs
    // incremental-apply on a type that does not support it. Re-check this line:col
    // if execute_external_cache_path moves. The load-bearing apply itself
    // (:726) is covered by incremental_projection_applies_events_after_cached_watermark.
    // TODO(0.8.3 backlog): kill :540:61 with a non-incremental-type test instead
    // of leaving it an honest survivor.
    r"crates/core/src/store/projection/flow/mod\.rs:540:26: replace && with \|\| in execute_external_cache_path",
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
        let smoke_shard = critical_seam_smoke_shard(seam.slug);
        Self {
            label: match smoke_shard {
                Some(shard) => format!(
                    "{} ({}, smoke shard {shard})",
                    seam.label,
                    surface_name(seam.surface)
                ),
                None => format!("{} ({}, smoke)", seam.label, surface_name(seam.surface)),
            },
            slug: seam.slug.to_owned(),
            description: seam.description,
            scope: MutationScope::CriticalSeam,
            surface: seam.surface,
            baseline: MutationBaseline::Run,
            shard: smoke_shard.map(str::to_owned),
            sharding: smoke_shard.map(|_| MutationSharding::RoundRobin),
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
            MutationEnforcement::Threshold { min_catch_pct } => match self.shard.as_deref() {
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
            },
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
        _ => &[],
    }
}

/// Smoke-shard selector for a critical seam.
///
/// Most seams round-robin shard `0/8` so the smoke lane stays fast. Tiny seams
/// (a single small file) can have a `0/8` slice that lands on a single unviable
/// mutant, tripping the "no scoreable mutants" threshold gate. Those run the
/// whole seam in smoke (`None`) so they always carry scoreable evidence.
fn critical_seam_smoke_shard(slug: &str) -> Option<&'static str> {
    match slug {
        // gate.rs is ~120 lines; a 1/8 round-robin slice can be all-unviable.
        "frontier-append-gate" => None,
        _ => Some(CRITICAL_SMOKE_SHARD),
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
