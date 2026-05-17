use crate::util::{cargo_target_dir, repo_root, run_output};
use crate::{PublicApiArgs, SemverCheckArgs};
use anyhow::{bail, Context, Result};
use std::fs;
use std::process::Command;

pub(crate) fn public_api(args: PublicApiArgs) -> Result<()> {
    let root = repo_root()?;
    let target_dir = cargo_target_dir()?.join("public-api");
    fs::create_dir_all(&target_dir).with_context(|| format!("create {}", target_dir.display()))?;

    if !cargo_public_api_is_available() {
        let message = "public-api: cargo-public-api is not installed; advisory run skipped";
        if args.strict {
            bail!("{message}");
        }
        eprintln!("{message}");
        return Ok(());
    }

    let mut command = Command::new("cargo");
    command
        .current_dir(&root)
        .env("CARGO_TARGET_DIR", cargo_target_dir()?)
        .args([
            "public-api",
            "--package",
            "batpak",
            "--all-features",
            "--manifest-path",
            "Cargo.toml",
        ]);

    let output = match run_output(command) {
        Ok(output) => output,
        Err(error) if !args.strict => {
            eprintln!("public-api: advisory run failed: {error:#}");
            return Ok(());
        }
        Err(error) => return Err(error),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    fs::write(target_dir.join("batpak.txt"), stdout.as_bytes())
        .context("write target/public-api/batpak.txt")?;
    fs::write(target_dir.join("batpak.stderr.txt"), stderr.as_bytes())
        .context("write target/public-api/batpak.stderr.txt")?;
    println!(
        "public-api: wrote {}",
        target_dir.join("batpak.txt").display()
    );
    Ok(())
}

pub(crate) fn semver_check(args: SemverCheckArgs) -> Result<()> {
    let root = repo_root()?;
    let target_dir = cargo_target_dir()?.join("semver-public-api");
    fs::create_dir_all(&target_dir).with_context(|| format!("create {}", target_dir.display()))?;

    if !cargo_semver_checks_is_available() {
        let message = "semver-check: cargo-semver-checks is not installed; advisory run skipped";
        if args.strict {
            bail!("{message}");
        }
        eprintln!("{message}");
        return Ok(());
    }

    let mut command = Command::new("cargo");
    command
        .current_dir(&root)
        .env("CARGO_TARGET_DIR", cargo_target_dir()?)
        .args([
            "semver-checks",
            "--manifest-path",
            "crates/core/Cargo.toml",
            "--package",
            "batpak",
            "--all-features",
        ]);

    let output = match run_output(command) {
        Ok(output) => output,
        Err(error) if !args.strict => {
            let report = target_dir.join("semver-checks.txt");
            fs::write(&report, format!("{error:#}\n")).context("write semver advisory error")?;
            eprintln!(
                "semver-check: advisory run reported incompatibility or failed; see {}",
                report.display()
            );
            return Ok(());
        }
        Err(error) => return Err(error),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    fs::write(target_dir.join("semver-checks.txt"), stdout.as_bytes())
        .context("write target/semver-public-api/semver-checks.txt")?;
    fs::write(
        target_dir.join("semver-checks.stderr.txt"),
        stderr.as_bytes(),
    )
    .context("write target/semver-public-api/semver-checks.stderr.txt")?;
    println!(
        "semver-check: wrote {}",
        target_dir.join("semver-checks.txt").display()
    );
    Ok(())
}

fn cargo_public_api_is_available() -> bool {
    Command::new("cargo-public-api")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn cargo_semver_checks_is_available() -> bool {
    Command::new("cargo-semver-checks")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
