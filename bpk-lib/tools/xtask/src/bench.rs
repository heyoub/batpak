use crate::util::{cargo, cargo_target_dir, run};
use crate::{BenchArgs, BenchSurface};
use anyhow::{bail, Result};
use std::process::Command;

pub(crate) fn bench(args: BenchArgs) -> Result<()> {
    let BenchArgs {
        surface,
        save,
        compare,
        compile,
    } = args;
    if compile {
        return bench_compile(surface);
    }

    let baseline = save
        .as_deref()
        .map(|label| explicit_baseline_name(os_slug(), surface, label))
        .unwrap_or_else(|| baseline_name(os_slug(), surface));
    let benches = bench_targets(surface);
    let criterion_args = criterion_args(surface, save.as_deref(), compare, &baseline)?;
    let mut command = Command::new("cargo");
    command.arg("bench");
    if matches!(surface, BenchSurface::Native) {
        command.arg("--all-features");
    }
    for bench in benches {
        command.arg("--bench").arg(bench);
    }
    if !criterion_args.is_empty() {
        command.arg("--").args(&criterion_args);
    }

    print_bench_plan(surface, save.as_deref(), compare, &baseline);
    run(command)?;
    if matches!(surface, BenchSurface::Native) {
        for (package, benches) in FAMILY_BENCH_TARGETS {
            run(family_bench_run_command(package, benches, &criterion_args))?;
        }
    }
    Ok(())
}

pub(crate) fn bench_compile(surface: BenchSurface) -> Result<()> {
    let target_dir = match surface {
        BenchSurface::Neutral => cargo_target_dir()?.join("xtask-bench-compile-neutral"),
        BenchSurface::Native => cargo_target_dir()?.join("xtask-bench-compile-native"),
    };
    cargo(bench_compile_args(surface, &target_dir))?;
    if matches!(surface, BenchSurface::Native) {
        for (package, benches) in FAMILY_BENCH_TARGETS {
            cargo(family_bench_compile_args(package, benches, &target_dir))?;
        }
    }
    Ok(())
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
            "fork_cost",
            "projection_latency",
            "query_materialization",
            "recovery_lanes",
            "replay_lanes",
            "subscription_fanout",
            "topology_matrix",
            "topology_write_cost",
            "unified_bench",
            "writer_batch_staging",
            "writer_coordinate_churn",
            "write_throughput",
        ],
    }
}

const FAMILY_BENCH_TARGETS: &[(&str, &[&str])] = &[
    ("syncbat", &["dispatch"]),
    ("netbat", &["boundary"]),
    ("refbat", &["live_operations"]),
];

fn family_bench_compile_args(
    package: &str,
    benches: &[&str],
    target_dir: &std::path::Path,
) -> Vec<String> {
    let mut args = vec![
        "bench".to_owned(),
        "-p".to_owned(),
        package.to_owned(),
        "--no-run".to_owned(),
        "--target-dir".to_owned(),
        target_dir.to_string_lossy().into_owned(),
    ];
    for bench in benches {
        args.push("--bench".to_owned());
        args.push((*bench).to_owned());
    }
    args
}

fn family_bench_run_command(package: &str, benches: &[&str], criterion_args: &[String]) -> Command {
    let mut command = Command::new("cargo");
    command.args(family_bench_run_args(package, benches, criterion_args));
    command
}

fn family_bench_run_args(
    package: &str,
    benches: &[&str],
    criterion_args: &[String],
) -> Vec<String> {
    let mut args = vec!["bench".to_owned(), "-p".to_owned(), package.to_owned()];
    for bench in benches {
        args.push("--bench".to_owned());
        args.push((*bench).to_owned());
    }
    if !criterion_args.is_empty() {
        args.push("--".to_owned());
        args.extend_from_slice(criterion_args);
    }
    args
}

fn criterion_args(
    surface: BenchSurface,
    save: Option<&str>,
    compare: bool,
    baseline: &str,
) -> Result<Vec<String>> {
    match (save, compare) {
        (Some(_), true) => bail!("--save and --compare are mutually exclusive"),
        (Some(_), false) => Ok(vec!["--save-baseline".to_owned(), baseline.to_owned()]),
        (None, true) => {
            if !baseline_exists(baseline)? {
                bail!(
                    "baseline {} does not exist yet. Run `cargo xtask bench --surface {} --save` first.",
                    baseline,
                    surface_name(surface)
                );
            }
            Ok(vec!["--baseline".to_owned(), baseline.to_owned()])
        }
        (None, false) => Ok(Vec::new()),
    }
}

fn print_bench_plan(surface: BenchSurface, save: Option<&str>, compare: bool, baseline: &str) {
    match (save, compare) {
        (Some(_), false) => {
            println!(
                "Running {} benchmarks and saving baseline {}...",
                surface_name(surface),
                baseline
            );
        }
        (None, true) => {
            println!(
                "Comparing {} benchmarks against baseline {}...",
                surface_name(surface),
                baseline
            );
        }
        (None, false) => {
            println!("Running {} benchmarks...", surface_name(surface));
            println!("Baseline name: {}", baseline);
        }
        (Some(_), true) => {}
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
    use super::{
        baseline_name, bench_compile_args, bench_targets, explicit_baseline_name,
        family_bench_run_args, FAMILY_BENCH_TARGETS,
    };
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
                "fork_cost",
                "projection_latency",
                "query_materialization",
                "recovery_lanes",
                "replay_lanes",
                "subscription_fanout",
                "topology_matrix",
                "topology_write_cost",
                "unified_bench",
                "writer_batch_staging",
                "writer_coordinate_churn",
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
            explicit_baseline_name("linux", BenchSurface::Neutral, "baseline-v0.8.0"),
            "linux-neutral-baseline-v0.8.0"
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
            "fork_cost",
            "--bench",
            "projection_latency",
            "--bench",
            "query_materialization",
            "--bench",
            "recovery_lanes",
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
            "writer_coordinate_churn",
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

    #[test]
    fn family_bench_run_args_include_package_and_criterion_args() {
        let args = family_bench_run_args(
            "refbat",
            &["live_operations"],
            &["--save-baseline".to_owned(), "windows-native-v3".to_owned()],
        );
        assert_eq!(
            args,
            vec![
                "bench",
                "-p",
                "refbat",
                "--bench",
                "live_operations",
                "--",
                "--save-baseline",
                "windows-native-v3"
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn every_family_bench_target_is_declared_by_its_package() {
        for (package, benches, manifest) in [
            (
                "syncbat",
                FAMILY_BENCH_TARGETS
                    .iter()
                    .find_map(|(name, benches)| (*name == "syncbat").then_some(*benches))
                    .expect("syncbat family benches are wired"),
                include_str!("../../../crates/syncbat/Cargo.toml"),
            ),
            (
                "netbat",
                FAMILY_BENCH_TARGETS
                    .iter()
                    .find_map(|(name, benches)| (*name == "netbat").then_some(*benches))
                    .expect("netbat family benches are wired"),
                include_str!("../../../crates/netbat/Cargo.toml"),
            ),
            (
                "refbat",
                FAMILY_BENCH_TARGETS
                    .iter()
                    .find_map(|(name, benches)| (*name == "refbat").then_some(*benches))
                    .expect("refbat family benches are wired"),
                include_str!("../../../crates/refbat/Cargo.toml"),
            ),
        ] {
            let manifest: toml::Value = toml::from_str(manifest).expect("parse package manifest");
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
            let wired = benches
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(
                declared, wired,
                "{package} Cargo.toml benches must stay wired into cargo xtask bench --surface native"
            );
        }
    }
}
