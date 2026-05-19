use super::{deny_split, integrity, templates};
use crate::bench;
use crate::publish::FAMILY_CRATES;
use crate::util::{cargo, cargo_target_dir_arg};
use crate::{BenchSurface, PackageLeakScanArgs, PublicApiArgs};
use anyhow::Result;

pub(crate) fn ci() -> Result<()> {
    super::check_version_pins()?;
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
    // tier-1.4: per-PR rustdoc gate. Promotes batpak/syncbat/netbat
    // missing-doc warnings to errors so docs are kept tight on every
    // PR — not just on the main-branch `docs` job.
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
    // tier-3: cargo-machete advisory pass. Failure is non-blocking
    // because cargo-machete is a separate install; the audit's
    // recommendation was to wire it in advisory-first and harden to
    // blocking after one clean release.
    unused_deps_advisory();
    integrity("structural-check", [])
}

/// Build rustdoc for the three publish crates with `-D warnings`.
/// Catches missing-docs lints, broken intra-doc links, and stale
/// doc references between commits — not just on the main-branch
/// `docs` job.
fn doc_deny_warnings() -> Result<()> {
    let mut cmd = std::process::Command::new("cargo");
    cmd.args([
        "doc",
        "--no-deps",
        "-p",
        "batpak",
        "-p",
        "syncbat",
        "-p",
        "netbat",
    ])
    .env("RUSTDOCFLAGS", "-D warnings");
    crate::util::run(cmd)
}

/// Run `xtask unused-deps` in advisory mode — log the result, never
/// fail the CI gate. Hardens to blocking after the next clean release
/// per the gate audit's P1 plan.
fn unused_deps_advisory() {
    if let Err(error) = super::unused_deps() {
        eprintln!("xtask ci: unused-deps advisory pass reported: {error}");
    }
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
