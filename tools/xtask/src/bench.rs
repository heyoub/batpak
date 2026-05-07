use crate::util::{cargo, run};
use crate::{BenchArgs, BenchSurface};
use anyhow::{bail, Result};
use std::process::Command;

pub(crate) fn bench(args: BenchArgs) -> Result<()> {
    if args.compile {
        return bench_compile(args.surface);
    }

    let baseline = baseline_name(os_slug(), args.surface);
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
        (true, true) => bail!("--save and --compare are mutually exclusive"),
        (true, false) => {
            println!(
                "Running {} benchmarks and saving baseline {}...",
                surface_name(args.surface),
                baseline
            );
            command.arg("--").arg("--save-baseline").arg(baseline);
        }
        (false, true) => {
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
        (false, false) => {
            println!("Running {} benchmarks...", surface_name(args.surface));
            println!("Baseline name: {}", baseline);
        }
    }

    run(command)
}

pub(crate) fn bench_compile(surface: BenchSurface) -> Result<()> {
    cargo(bench_compile_args(surface))
}

fn bench_compile_args(surface: BenchSurface) -> Vec<&'static str> {
    let target_dir = match surface {
        BenchSurface::Neutral => "target/xtask-bench-compile-neutral",
        BenchSurface::Native => "target/xtask-bench-compile-native",
    };
    let mut args = vec!["bench", "--no-run", "--target-dir", target_dir];
    if matches!(surface, BenchSurface::Native) {
        args.push("--all-features");
    }
    for bench in bench_targets(surface) {
        args.push("--bench");
        args.push(bench);
    }
    args
}

pub(crate) fn bench_targets(surface: BenchSurface) -> &'static [&'static str] {
    match surface {
        BenchSurface::Neutral => &[
            "cold_start",
            "compaction",
            "frontier_waiters",
            "projection_latency",
            "subscription_fanout",
            "write_throughput",
        ],
        BenchSurface::Native => &[
            "batch_throughput",
            "cold_start",
            "compaction",
            "frontier_waiters",
            "projection_latency",
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

fn baseline_exists(baseline: &str) -> Result<bool> {
    let target_dir = std::path::Path::new("target").join("criterion");
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
    use super::{baseline_name, bench_compile_args, bench_targets};
    use crate::BenchSurface;

    #[test]
    fn neutral_surface_matches_expected_targets() {
        assert_eq!(
            bench_targets(BenchSurface::Neutral),
            &[
                "cold_start",
                "compaction",
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
                "batch_throughput",
                "cold_start",
                "compaction",
                "frontier_waiters",
                "projection_latency",
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
    fn compile_args_match_neutral_surface() {
        assert_eq!(
            bench_compile_args(BenchSurface::Neutral),
            vec![
                "bench",
                "--no-run",
                "--target-dir",
                "target/xtask-bench-compile-neutral",
                "--bench",
                "cold_start",
                "--bench",
                "compaction",
                "--bench",
                "frontier_waiters",
                "--bench",
                "projection_latency",
                "--bench",
                "subscription_fanout",
                "--bench",
                "write_throughput",
            ]
        );
    }

    #[test]
    fn compile_args_match_native_surface() {
        assert_eq!(
            bench_compile_args(BenchSurface::Native),
            vec![
                "bench",
                "--no-run",
                "--target-dir",
                "target/xtask-bench-compile-native",
                "--all-features",
                "--bench",
                "batch_throughput",
                "--bench",
                "cold_start",
                "--bench",
                "compaction",
                "--bench",
                "frontier_waiters",
                "--bench",
                "projection_latency",
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
        );
    }
}
