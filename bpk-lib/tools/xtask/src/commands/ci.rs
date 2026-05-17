use super::{deny_split, integrity, templates};
use crate::bench;
use crate::util::{cargo, cargo_target_dir_arg};
use crate::BenchSurface;
use anyhow::Result;

const FAMILY_CRATES: &[&str] = &["syncbat", "downstream-kit", "netbat"];

pub(crate) fn ci() -> Result<()> {
    integrity("doctor", ["--strict"])?;
    integrity("traceability-check", [])?;
    integrity("structural-check", [])?;
    cargo(["fmt", "--check"])?;
    cargo([
        "clippy",
        "--all-features",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ])?;
    for package in FAMILY_CRATES {
        cargo([
            "clippy",
            "-p",
            package,
            "--no-deps",
            "--all-features",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ])?;
    }
    deny_split()?;
    run_nextest_ci(["--all-features"])?;
    cargo(["test", "--doc", "--all-features"])?;
    for package in FAMILY_CRATES {
        cargo(["test", "-p", package, "--all-features"])?;
    }
    cargo(["check", "--all-features"])?;
    cargo(["check", "--no-default-features"])?;
    for package in FAMILY_CRATES {
        cargo(["check", "-p", package, "--all-features"])?;
        cargo(["check", "-p", package, "--no-default-features"])?;
    }
    templates()?;
    bench::bench_compile(BenchSurface::Neutral)?;
    bench::bench_compile(BenchSurface::Native)?;
    integrity("structural-check", [])
}

pub(crate) fn perf_gates() -> Result<()> {
    run_nextest_ci([
        "--test",
        "perf_gates",
        "--all-features",
        "--run-ignored",
        "only",
    ])
}

pub(crate) fn run_nextest_ci<const N: usize>(args: [&str; N]) -> Result<()> {
    let target_dir = cargo_target_dir_arg()?;
    let mut command = vec![
        "nextest",
        "run",
        "--target-dir",
        target_dir.as_str(),
        "--profile",
        "ci",
    ];
    command.extend(args);
    cargo(command)
}
