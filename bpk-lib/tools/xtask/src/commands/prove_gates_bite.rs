//! `cargo xtask prove-gates-bite` — the "prove the gates bite" lane (GAUNT-TQL).
//!
//! The gate registry records, per blocking gate, an anti-vacuous RED fixture.
//! For `ProductionFlip` fixtures (S2/S3 sentinels, perf-alloc budget) anti-vacuity
//! means: under `--cfg gauntlet_red_fixture` the fixture's expectation flips to the
//! ILLEGAL/old behavior, so the test MUST FAIL against the cured code. This lane
//! proves that in automation: it rebuilds the ProductionFlip fixtures under the cfg
//! and asserts each test actually reds. A fixture that PASSES (or never runs) under
//! the cfg has no real red half — its gate's blocking authority is laundered, so we
//! fail. The fixture list comes from the registry (`batpak-integrity
//! production-flip-fixtures`), the single source of truth, so this lane and the
//! registry can never drift.
//!
//! A dedicated `CARGO_TARGET_DIR` keeps the red-cfg build from thrashing the normal
//! build cache (the cfg changes every fingerprint).

use crate::util::{self, cargo_target_dir, run_output};
use anyhow::{bail, Context, Result};
use std::process::Command;

/// The cfg that flips ProductionFlip fixtures to their illegal-behavior assertion.
const RED_CFG: &str = "--cfg gauntlet_red_fixture";

/// Minimum number of ProductionFlip fixtures we expect (the core S2/S3/perf-alloc
/// sentinels plus the bvisor C1 grid + reconciliation oracles). If the registry
/// ever returns fewer, the lane fails closed rather than vacuously pass.
const MIN_FIXTURES: usize = 5;

/// Resolve the cargo `--package` that owns a registry fixture reference from its
/// `<repo-rel-file>::<test_fn>` path. ProductionFlip fixtures live under either
/// `crates/core/tests/...` (the `batpak` crate) or `crates/<pkg>/tests/...` (a
/// sibling crate, e.g. `bvisor`). Defaulting to `batpak` keeps the historical
/// core fixtures working without per-entry annotation.
fn package_for(reference: &str) -> &'static str {
    if reference.starts_with("crates/bvisor/") {
        "bvisor"
    } else {
        "batpak"
    }
}

pub(crate) fn run() -> Result<()> {
    let fixtures = production_flip_fixtures()?;
    if fixtures.len() < MIN_FIXTURES {
        bail!(
            "prove-gates-bite: expected >= {MIN_FIXTURES} ProductionFlip fixtures from the \
             registry, got {} ({:?}) — the registry list shrank unexpectedly",
            fixtures.len(),
            fixtures
        );
    }
    println!(
        "prove-gates-bite: {} ProductionFlip fixture(s) to bite:",
        fixtures.len()
    );
    for reference in &fixtures {
        println!("  {reference}");
    }

    // Separate target dir: the red-cfg build must not pollute the normal cache.
    let bite_target = cargo_target_dir()?.join("gauntlet-bite");

    // Each ProductionFlip fixture is owned by a cargo package, derived from its
    // path prefix. Build the test targets for every distinct package under the cfg
    // (core S2/S3/perf-alloc live in `batpak`; bvisor C1's grid + reconciliation
    // live in `bvisor` behind `--all-features`). Building per-package keeps the
    // bite lane honest for sibling crates instead of silently skipping them.
    let mut packages: Vec<&'static str> = fixtures.iter().map(|r| package_for(r)).collect();
    packages.sort_unstable();
    packages.dedup();
    for package in &packages {
        println!("prove-gates-bite: building {package} test targets under {RED_CFG} ...");
        let mut build = Command::new("cargo");
        build
            .env("CARGO_TARGET_DIR", &bite_target)
            .env("RUSTFLAGS", RED_CFG)
            .args(["test", "--package", package, "--all-features", "--no-run"]);
        util::run(build).with_context(|| {
            format!("test build for {package} under --cfg gauntlet_red_fixture failed to compile")
        })?;
    }

    let mut laundered = Vec::new();
    for reference in &fixtures {
        let test_fn = reference.rsplit("::").next().unwrap_or(reference.as_str());
        let package = package_for(reference);
        println!("prove-gates-bite: biting {reference} (package {package})");
        // Raw output() (NOT util::run_output, which bails on nonzero): we EXPECT a
        // nonzero exit here — a failing test is the success condition.
        let output = Command::new("cargo")
            .env("CARGO_TARGET_DIR", &bite_target)
            .env("RUSTFLAGS", RED_CFG)
            .args([
                "test",
                "--package",
                package,
                "--all-features",
                test_fn,
                "--",
                "--exact",
            ])
            .output()
            .with_context(|| format!("run red-cfg test for {reference}"))?;
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        // A failing test prints `test result: FAILED`. "ran and passed" or
        // "matched no test" both print `test result: ok` (or no result line) —
        // both are laundering (the fixture's red half did not red).
        if combined.contains("test result: FAILED") {
            println!("  OK: {reference} RED under {RED_CFG}");
        } else {
            println!(
                "  LAUNDERED: {reference} did NOT red under {RED_CFG} (passed or did not run)"
            );
            laundered.push(reference.clone());
        }
    }

    if laundered.is_empty() {
        println!(
            "prove-gates-bite: ok — all {} ProductionFlip red fixture(s) bite under {RED_CFG}",
            fixtures.len()
        );
        Ok(())
    } else {
        bail!(
            "prove-gates-bite: {} gate(s) have a red fixture that cannot red under {RED_CFG} \
             (laundered blocking authority — make the fixture flip to the illegal behavior, or \
             downgrade the gate):\n  {}",
            laundered.len(),
            laundered.join("\n  ")
        )
    }
}

/// Ask the integrity binary for the registry's ProductionFlip fixture references
/// (`<file>::<test_fn>`, one per line). Uses the NORMAL target dir so it reuses the
/// existing integrity build.
fn production_flip_fixtures() -> Result<Vec<String>> {
    let mut cmd = Command::new("cargo");
    cmd.env("CARGO_TARGET_DIR", cargo_target_dir()?).args([
        "run",
        "-q",
        "--package",
        "batpak-integrity",
        "--",
        "production-flip-fixtures",
    ]);
    let output = run_output(cmd).context("list production-flip fixtures from the gate registry")?;
    let fixtures = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    Ok(fixtures)
}
