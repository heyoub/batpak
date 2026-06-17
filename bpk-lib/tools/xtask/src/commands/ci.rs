use super::{deny_split, integrity, templates};
use crate::bench;
use crate::publish::FAMILY_CRATES;
use crate::util::{cargo, cargo_target_dir_arg};
use crate::{BenchSurface, PackageLeakScanArgs, PublicApiArgs};
use anyhow::Result;

/// Early PR signal: format, clippy, checks, tests, dependency gates, machine law.
pub(crate) fn ci_fast() -> Result<()> {
    super::check_version_pins()?;
    cargo(["fmt", "--check"])?;
    run_workspace_clippy()?;
    run_family_clippy()?;
    run_workspace_and_family_checks()?;
    deny_split()?;
    run_nextest_ci(["--all-features"])?;
    cargo(["test", "--doc", "--all-features"])?;
    run_family_tests()?;
    integrity("traceability-check", [])?;
    integrity("structural-check", [])
}

/// Full merge bundle: fast lane plus release-oriented and compile-heavy gates.
pub(crate) fn ci() -> Result<()> {
    ci_fast()?;
    integrity("doctor", ["--strict"])?;
    templates()?;
    doc_deny_warnings()?;
    crate::public_api::public_api(PublicApiArgs {
        strict: true,
        check_baseline: true,
        bless_baseline: false,
    })?;
    super::package_leak_scan(PackageLeakScanArgs {
        allow_dirty: false,
        strict_language: true,
    })?;
    bench::bench_compile(BenchSurface::Neutral)?;
    bench::bench_compile(BenchSurface::Native)?;
    unused_deps_advisory();
    integrity("structural-check", [])
}

/// Native Windows surface compatibility: checks, tests, and platform-sensitive fixtures.
pub(crate) fn ci_windows_surface() -> Result<()> {
    super::check_version_pins()?;
    cargo(["fmt", "--check"])?;
    run_workspace_and_family_checks()?;
    run_nextest_ci(["--all-features"])?;
    run_kind_collision_composer_fixture()
}

fn run_workspace_clippy() -> Result<()> {
    cargo([
        "clippy",
        "--all-features",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ])
}

fn run_family_clippy() -> Result<()> {
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
    Ok(())
}

fn run_workspace_and_family_checks() -> Result<()> {
    cargo(["check", "--all-features"])?;
    cargo(["check", "--no-default-features"])?;
    for package in FAMILY_CRATES {
        cargo(["check", "-p", package, "--all-features"])?;
        cargo(["check", "-p", package, "--no-default-features"])?;
    }
    Ok(())
}

fn run_family_tests() -> Result<()> {
    for package in FAMILY_CRATES {
        cargo(["test", "-p", package, "--all-features"])?;
    }
    Ok(())
}

fn run_kind_collision_composer_fixture() -> Result<()> {
    cargo([
        "test",
        "--release",
        "--manifest-path",
        "crates/core/fixtures/kind-collision-composer/Cargo.toml",
    ])
}

/// Build rustdoc for every workspace member with `-D warnings`.
///
/// Uses `--workspace` (not a hardcoded crate list) so any new crate is doc-lint
/// gated automatically. `--no-deps` keeps the gate to first-party crates.
fn doc_deny_warnings() -> Result<()> {
    let mut cmd = std::process::Command::new("cargo");
    cmd.args(["doc", "--workspace", "--no-deps"])
        .env("RUSTDOCFLAGS", "-D warnings");
    crate::util::run(cmd)
}

fn unused_deps_advisory() {
    if let Err(error) = super::unused_deps() {
        eprintln!("xtask ci: unused-deps advisory pass reported: {error}");
    }
}

pub(crate) fn perf_gates() -> Result<()> {
    run_nextest_ci([
        "--test",
        "perf_gates",
        "--test",
        "perf_gates_throughput_latency",
        "--test",
        "perf_gates_cold_start",
        "--test",
        "perf_gates_correctness",
        "--all-features",
        "--run-ignored",
        "only",
    ])
}

pub(crate) fn run_nextest_ci<'a, I>(args: I) -> Result<()>
where
    I: IntoIterator<Item = &'a str>,
{
    let target_dir = cargo_target_dir_arg()?;
    let mut command: Vec<String> = vec![
        "nextest".to_owned(),
        "run".to_owned(),
        "--target-dir".to_owned(),
        target_dir,
        "--profile".to_owned(),
        "ci".to_owned(),
    ];
    command.extend(args.into_iter().map(str::to_owned));
    cargo(command.iter().map(String::as_str))
}
