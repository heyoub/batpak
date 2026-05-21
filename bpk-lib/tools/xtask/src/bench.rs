use crate::util::{cargo, cargo_target_dir, run};
use crate::{BenchArgs, BenchSurface};
use anyhow::{bail, Result};
use std::process::Command;

pub(crate) fn bench(args: BenchArgs) -> Result<()> {
    if args.compile {
        return bench_compile(args.surface);
    }

    let baseline = args
        .save
        .as_deref()
        .map(|label| explicit_baseline_name(os_slug(), args.surface, label))
        .unwrap_or_else(|| baseline_name(os_slug(), args.surface));
    let benches = bench_targets(args.surface);
    let mut command = Command::new("cargo");
    command.arg("bench");
    if matches!(args.surface, BenchSurface::Native) {
        command.arg("--all-features");
    }
    for bench in benches {
        command.arg("--bench").arg(bench);
    }

    match (args.save, args.compare) {
        (Some(_), true) => bail!("--save and --compare are mutually exclusive"),
        (Some(_), false) => {
            println!(
                "Running {} benchmarks and saving baseline {}...",
                surface_name(args.surface),
                baseline
            );
            command.arg("--").arg("--save-baseline").arg(baseline);
        }
        (None, true) => {
            if !baseline_exists(&baseline)? {
                bail!(
                    "baseline {} does not exist yet. Run `cargo xtask bench --surface {} --save` first.",
                    baseline,
                    surface_name(args.surface)
                );
            }
            println!(
                "Comparing {} benchmarks against baseline {}...",
                surface_name(args.surface),
                baseline
            );
            command.arg("--").arg("--baseline").arg(baseline);
        }
        (None, false) => {
            println!("Running {} benchmarks...", surface_name(args.surface));
            println!("Baseline name: {}", baseline);
        }
    }

    run(command)
}

pub(crate) fn bench_compile(surface: BenchSurface) -> Result<()> {
    let target_dir = match surface {
        BenchSurface::Neutral => cargo_target_dir()?.join("xtask-bench-compile-neutral"),
        BenchSurface::Native => cargo_target_dir()?.join("xtask-bench-compile-native"),
    };
    cargo(bench_compile_args(surface, &target_dir))
}

fn bench_compile_args(surface: BenchSurface, target_dir: &std::path::Path) -> Vec<String> {
    let mut args = vec![
        "bench".to_owned(),
        "--no-run".to_owned(),
        "--target-dir".to_owned(),
        target_dir.to_string_lossy().into_owned(),
    ];
    if matches!(surface, BenchSurface::Native) {
        args.push("--all-features".to_owned());
    }
    for bench in bench_targets(surface) {
        args.push("--bench".to_owned());
        args.push((*bench).to_owned());
    }
    args
}

pub(crate) fn bench_targets(surface: BenchSurface) -> &'static [&'static str] {
    match surface {
        BenchSurface::Neutral => &[
            "cold_start",
            "compaction",
            "evidence_reports",
            "frontier_waiters",
            "projection_latency",
            "subscription_fanout",
            "write_throughput",
        ],
        BenchSurface::Native => &[
            "ancestry_walk",
            "batch_throughput",
            "cold_start",
            "columnar_query",
            "compaction",
            "evidence_reports",
            "frontier_waiters",
            "projection_latency",
            "query_materialization",
            "replay_lanes",
            "subscription_fanout",
            "topology_matrix",
            "topology_write_cost",
            "unified_bench",
            "writer_batch_staging",
            "writer_staging",
            "write_throughput",
        ],
    }
}

fn surface_name(surface: BenchSurface) -> &'static str {
    match surface {
        BenchSurface::Neutral => "neutral",
        BenchSurface::Native => "native",
    }
}

fn os_slug() -> &'static str {
    match std::env::consts::OS {
        "windows" => "windows",
        "linux" => "linux",
        "macos" => "macos",
        _ => "other",
    }
}

fn baseline_name(os: &str, surface: BenchSurface) -> String {
    format!("{os}-{}-v3", surface_name(surface))
}

fn explicit_baseline_name(os: &str, surface: BenchSurface, label: &str) -> String {
    format!("{os}-{}-{label}", surface_name(surface))
}

fn baseline_exists(baseline: &str) -> Result<bool> {
    let target_dir = cargo_target_dir()?.join("criterion");
    if !target_dir.exists() {
        return Ok(false);
    }
    for entry in walk_dir(&target_dir)? {
        if entry.file_type()?.is_dir()
            && entry
                .file_name()
                .to_str()
                .map(|name| name == baseline)
                .unwrap_or(false)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn walk_dir(root: &std::path::Path) -> Result<Vec<std::fs::DirEntry>> {
    let mut stack = vec![root.to_path_buf()];
    let mut entries = Vec::new();
    while let Some(path) = stack.pop() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                stack.push(entry.path());
            }
            entries.push(entry);
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::{baseline_name, bench_compile_args, bench_targets, explicit_baseline_name};
    use crate::BenchSurface;
    use std::path::Path;

    #[test]
    fn neutral_surface_matches_expected_targets() {
        assert_eq!(
            bench_targets(BenchSurface::Neutral),
            &[
                "cold_start",
                "compaction",
                "evidence_reports",
                "frontier_waiters",
                "projection_latency",
                "subscription_fanout",
                "write_throughput",
            ]
        );
    }

    #[test]
    fn native_surface_matches_expected_targets() {
        assert_eq!(
            bench_targets(BenchSurface::Native),
            &[
                "ancestry_walk",
                "batch_throughput",
                "cold_start",
                "columnar_query",
                "compaction",
                "evidence_reports",
                "frontier_waiters",
                "projection_latency",
                "query_materialization",
                "replay_lanes",
                "subscription_fanout",
                "topology_matrix",
                "topology_write_cost",
                "unified_bench",
                "writer_batch_staging",
                "writer_staging",
                "write_throughput",
            ]
        );
    }

    #[test]
    fn baseline_name_is_stable() {
        assert_eq!(
            baseline_name("linux", BenchSurface::Neutral),
            "linux-neutral-v3"
        );
        assert_eq!(
            baseline_name("windows", BenchSurface::Native),
            "windows-native-v3"
        );
    }

    #[test]
    fn explicit_baseline_name_uses_supplied_label() {
        assert_eq!(
            explicit_baseline_name("linux", BenchSurface::Neutral, "baseline-v0.7.6"),
            "linux-neutral-baseline-v0.7.6"
        );
    }

    #[test]
    fn compile_args_match_neutral_surface() {
        let expected = vec![
            "bench",
            "--no-run",
            "--target-dir",
            "/repo/bpk-lib/target/xtask-bench-compile-neutral",
            "--bench",
            "cold_start",
            "--bench",
            "compaction",
            "--bench",
            "evidence_reports",
            "--bench",
            "frontier_waiters",
            "--bench",
            "projection_latency",
            "--bench",
            "subscription_fanout",
            "--bench",
            "write_throughput",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();

        assert_eq!(
            bench_compile_args(
                BenchSurface::Neutral,
                Path::new("/repo/bpk-lib/target/xtask-bench-compile-neutral")
            ),
            expected
        );
    }

    #[test]
    fn compile_args_match_native_surface() {
        let expected = vec![
            "bench",
            "--no-run",
            "--target-dir",
            "/repo/bpk-lib/target/xtask-bench-compile-native",
            "--all-features",
            "--bench",
            "ancestry_walk",
            "--bench",
            "batch_throughput",
            "--bench",
            "cold_start",
            "--bench",
            "columnar_query",
            "--bench",
            "compaction",
            "--bench",
            "evidence_reports",
            "--bench",
            "frontier_waiters",
            "--bench",
            "projection_latency",
            "--bench",
            "query_materialization",
            "--bench",
            "replay_lanes",
            "--bench",
            "subscription_fanout",
            "--bench",
            "topology_matrix",
            "--bench",
            "topology_write_cost",
            "--bench",
            "unified_bench",
            "--bench",
            "writer_batch_staging",
            "--bench",
            "writer_staging",
            "--bench",
            "write_throughput",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();

        assert_eq!(
            bench_compile_args(
                BenchSurface::Native,
                Path::new("/repo/bpk-lib/target/xtask-bench-compile-native")
            ),
            expected
        );
    }

    #[test]
    fn every_cargo_bench_target_is_wired_to_a_surface() {
        let manifest: toml::Value = toml::from_str(include_str!("../../../crates/core/Cargo.toml"))
            .expect("parse batpak package manifest");
        let declared = manifest
            .get("bench")
            .and_then(toml::Value::as_array)
            .expect("Cargo.toml declares bench targets")
            .iter()
            .map(|bench| {
                bench
                    .get("name")
                    .and_then(toml::Value::as_str)
                    .expect("bench target has name")
            })
            .collect::<std::collections::BTreeSet<_>>();
        let wired = bench_targets(BenchSurface::Neutral)
            .iter()
            .chain(bench_targets(BenchSurface::Native))
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            declared, wired,
            "every Cargo.toml [[bench]] target must be wired into at least one cargo xtask bench surface"
        );
    }
}
