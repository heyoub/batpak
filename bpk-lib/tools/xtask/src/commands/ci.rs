use super::{deny_split, integrity, templates};
use crate::bench;
use crate::coverage;
use crate::publish::FAMILY_CRATES;
use crate::util::{cargo, cargo_target_dir_arg};
use crate::{BenchSurface, CoverArgs, PackageLeakScanArgs, PublicApiArgs};
use anyhow::Result;

/// Early PR signal: format, clippy, checks, tests, dependency gates, machine law.
pub(crate) fn ci_fast() -> Result<()> {
    super::check_version_pins()?;
    cargo(["fmt", "--check"])?;
    run_workspace_clippy()?;
    run_family_clippy()?;
    run_workspace_and_family_checks()?;
    deny_split()?;
    // `--workspace` is mandatory here for the same reason as the clippy gate:
    // without it nextest runs only `default-members` (crates/core), leaving
    // tools/integrity, refbat, and the macro crates outside the test net. That
    // hole let a stale integrity self-test fixture rot undetected until a
    // workspace-wide run surfaced it.
    run_nextest_ci(["--workspace", "--all-features"])?;
    cargo(["test", "--doc", "--all-features"])?;
    run_family_tests()?;
    integrity("traceability-check", [])?;
    integrity("structural-check", [])?;
    // L2+ contract gates on the DEFAULT PR path (P1-1). These used to live only
    // in `ci()`/`preflight()` behind the label-gated verify-linux job, which
    // meant a public-api drift or sub-floor coverage shipped without any label.
    // They now run in `ci-fast` (the always-on default lane) so EVERY
    // Rust-touching PR enforces them. The devcontainer ships the required tools,
    // so each gate is invoked STRICT here (cargo-public-api availability is
    // still handled gracefully inside `public_api`, mirroring its existing
    // strict/advisory split). Re-burying any of these is blocked by the
    // anti-rebury assertion in tools/integrity/src/ci_parity.rs.
    coverage::cover(CoverArgs {
        ci: true,
        json: false,
        threshold: Some(coverage::COVERAGE_FLOOR_PCT),
    })?;
    crate::public_api::public_api(PublicApiArgs {
        strict: true,
        check_baseline: true,
        bless_baseline: false,
    })?;
    super::package_leak_scan(PackageLeakScanArgs {
        allow_dirty: false,
        strict_language: true,
    })?;
    integrity("doctor", ["--strict"])?;
    // Tool-qualification anti-vacuity gate (P1-3): every registered gate must
    // have left a non-vacuous execution receipt. Runs AFTER structural-check so
    // the receipts it validates were just (re)generated in this same invocation.
    integrity("gauntlet-receipts-present", [])
}

/// Full merge bundle: fast lane plus release-oriented and compile-heavy gates.
///
/// The L2+ contract gates (coverage floor, public-api baseline,
/// package-leak-scan, doctor --strict) now live in `ci_fast()` so they run on
/// the default PR path; this bundle keeps only the genuinely compile-heavy /
/// release-oriented extras on top of the fast lane.
pub(crate) fn ci() -> Result<()> {
    ci_fast()?;
    templates()?;
    doc_deny_warnings()?;
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
    // `--workspace` is mandatory: without it cargo lints only `default-members`
    // (crates/core), silently leaving tools/integrity and tools/xtask outside
    // the clippy net. A 0.9.0 PR1 review caught 14 clippy errors hiding in
    // tools/integrity precisely because this gate skipped them.
    cargo([
        "clippy",
        "--workspace",
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
    // `--workspace` for the same reason as run_workspace_clippy: the bare form
    // only checks default-members (crates/core), leaving the tooling crates
    // unchecked under both the default and no-default-features axes.
    cargo(["check", "--workspace", "--all-features"])?;
    cargo(["check", "--workspace", "--no-default-features"])?;
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
        errln!("xtask ci: unused-deps advisory pass reported: {error}");
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
