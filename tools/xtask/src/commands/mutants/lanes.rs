use crate::MutantSurface;
use std::path::{Path, PathBuf};

use super::policy::{current_repo_mutation_enforcement, MutationEnforcement};
use super::score::MutationScore;

pub(super) const MUTANTS_OUTPUT_ROOT: &str = "tools/xtask/target/mutants";
pub(super) const CRITICAL_SEAM_MIN_CATCH_PCT: u32 = 85;
pub(super) const CRITICAL_SMOKE_SHARD: &str = "1/8";
pub(super) const REPO_WIDE_SMOKE_SHARD: &str = "1/48";

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
pub(super) const WRITER_COMMIT_MUTANT_FILES: &[&str] = &["crates/core/src/store/write/*.rs"];
pub(super) const CURSOR_MUTANT_FILES: &[&str] = &[
    "crates/core/src/store/delivery/cursor.rs",
    "crates/core/src/store/delivery/observation.rs",
    "crates/core/src/store/reactor_typed.rs",
];
pub(super) const PROJECTION_MUTANT_FILES: &[&str] = &[
    "crates/core/src/store/projection/flow/**/*.rs",
    "crates/core/src/store/projection/registry.rs",
];
pub(super) const SEGMENT_SCAN_MUTANT_FILES: &[&str] =
    &["crates/core/src/store/segment/scan/**/*.rs"];
pub(super) const HASH_CHAIN_REPLAY_ALL_FEATURES_MUTANT_FILES: &[&str] = &[
    "crates/core/src/store/ancestry/by_hash.rs",
    "crates/core/src/store/cold_start/rebuild.rs",
];
pub(super) const HASH_CHAIN_REPLAY_NO_DEFAULT_MUTANT_FILES: &[&str] = &[
    "crates/core/src/store/ancestry/by_clock.rs",
    "crates/core/src/store/cold_start/rebuild.rs",
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
pub(super) const HARNESS_LEDGER_LINT_MUTANT_FILES: &[&str] =
    &["tools/integrity/src/harness_lints.rs"];
pub(super) const ALL_FEATURES_MUTANT_EXCLUDES: &[&str] =
    &["crates/core/src/store/ancestry/by_clock.rs"];
pub(super) const NO_DEFAULT_FEATURES_MUTANT_EXCLUDES: &[&str] =
    &["crates/core/src/store/ancestry/by_hash.rs"];
pub(super) const INDEX_TOPOLOGY_DEFAULT_EQUIVALENT_MUTANT: &str = r"crates/core/src/store/config\.rs:.*replace IndexTopology::aos -> Self with Default::default\(\)";
pub(super) const SIDX_EMPTY_FOOTER_FLOOR_EQUIVALENT_MUTANT: &str = r"crates/core/src/store/segment/scan/recovery\.rs:.*replace \+ with . in Reader::sidx_covers_segment_tail";
pub(super) const ALL_FEATURES_MUTANT_EXCLUDE_RES: &[&str] = &[
    INDEX_TOPOLOGY_DEFAULT_EQUIVALENT_MUTANT,
    SIDX_EMPTY_FOOTER_FLOOR_EQUIVALENT_MUTANT,
];
pub(super) const NO_DEFAULT_FEATURES_MUTANT_EXCLUDE_RES: &[&str] = &[
    INDEX_TOPOLOGY_DEFAULT_EQUIVALENT_MUTANT,
    SIDX_EMPTY_FOOTER_FLOOR_EQUIVALENT_MUTANT,
];
const SEGMENT_SCAN_MUTANT_EXCLUDE_RES: &[&str] = &[SIDX_EMPTY_FOOTER_FLOOR_EQUIVALENT_MUTANT];

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
            excludes: surface_excludes(seam.surface),
            exclude_res: critical_seam_exclude_res(seam.slug),
        }
    }

    fn critical_smoke(seam: CriticalMutationSeam) -> Self {
        Self {
            label: format!(
                "{} ({}, smoke shard {CRITICAL_SMOKE_SHARD})",
                seam.label,
                surface_name(seam.surface)
            ),
            slug: seam.slug.to_owned(),
            description: seam.description,
            scope: MutationScope::CriticalSeam,
            surface: seam.surface,
            baseline: MutationBaseline::Run,
            shard: Some(CRITICAL_SMOKE_SHARD.to_owned()),
            sharding: Some(MutationSharding::RoundRobin),
            enforcement: MutationEnforcement::Threshold {
                min_catch_pct: CRITICAL_SEAM_MIN_CATCH_PCT,
            },
            package: seam.package,
            paths: seam.paths,
            excludes: surface_excludes(seam.surface),
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
        Path::new(MUTANTS_OUTPUT_ROOT).join(self.slug())
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

pub(super) fn surface_excludes(surface: MutantSurface) -> &'static [&'static str] {
    match surface {
        MutantSurface::AllFeatures => ALL_FEATURES_MUTANT_EXCLUDES,
        MutantSurface::NoDefaultFeatures => NO_DEFAULT_FEATURES_MUTANT_EXCLUDES,
    }
}

fn surface_exclude_res(surface: MutantSurface) -> &'static [&'static str] {
    match surface {
        MutantSurface::AllFeatures => ALL_FEATURES_MUTANT_EXCLUDE_RES,
        MutantSurface::NoDefaultFeatures => NO_DEFAULT_FEATURES_MUTANT_EXCLUDE_RES,
    }
}

fn critical_seam_exclude_res(slug: &str) -> &'static [&'static str] {
    match slug {
        "segment-scan" => SEGMENT_SCAN_MUTANT_EXCLUDE_RES,
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
            slug: "hash-chain-replay-all-features",
            label: "hash-chain and replay consistency",
            description: "hash-chain / replay consistency logic (blake3 lane)",
            surface: MutantSurface::AllFeatures,
            package: None,
            paths: HASH_CHAIN_REPLAY_ALL_FEATURES_MUTANT_FILES,
        },
        CriticalMutationSeam {
            slug: "hash-chain-replay-no-default",
            label: "hash-chain and replay consistency",
            description: "hash-chain / replay consistency logic (no-default lane)",
            surface: MutantSurface::NoDefaultFeatures,
            package: None,
            paths: HASH_CHAIN_REPLAY_NO_DEFAULT_MUTANT_FILES,
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
            slug: "harness-ledger-structural-lint",
            label: "harness ledger structural lint",
            description: "HARNESS_LEDGER schema, location, command, header, and line-cap enforcement",
            surface: MutantSurface::AllFeatures,
            package: Some("batpak-integrity"),
            paths: HARNESS_LEDGER_LINT_MUTANT_FILES,
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

pub(super) fn repo_wide_mutation_lanes(
    surfaces: Vec<MutantSurface>,
    shard: Option<&str>,
) -> Vec<MutationLane> {
    surfaces
        .into_iter()
        .map(|surface| MutationLane::repo_wide(surface, shard))
        .collect()
}
