//! `xtask msrv-check` — verify the publish crates compile under their
//! declared `rust-version` MSRV.
//!
//! Without this gate, `rust-version` in `Cargo.toml` is purely
//! declarative — a `let-else` from a newer toolchain slips through
//! until a downstream crates.io consumer trips over it.
//!
//! The command:
//!   1. Reads each publish-crate `Cargo.toml` and parses
//!      `package.rust-version`.
//!   2. For every unique declared MSRV, ensures the toolchain is
//!      installed via `rustup toolchain list`.
//!   3. Runs `cargo +<msrv> check -p <crate> --no-default-features`
//!      and `cargo +<msrv> check -p <crate> --all-features` for each
//!      publish crate.
//!
//! If the toolchain is missing, the command fails with an install
//! hint rather than auto-installing — consulting clients run release
//! gates inside clean containers and want deterministic tool
//! versioning.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use crate::publish::PUBLISH_CRATES;
use crate::util::{repo_root, run};

/// Verify each publish crate compiles under its declared
/// `rust-version`.
pub(crate) fn msrv_check() -> Result<()> {
    let root = repo_root()?;
    let bpk_lib = root.join("bpk-lib");
    let mut by_msrv: BTreeMap<String, Vec<&'static str>> = BTreeMap::new();
    for package in PUBLISH_CRATES {
        let manifest = bpk_lib
            .join("crates")
            .join(crate_dir_for(package))
            .join("Cargo.toml");
        let msrv = read_rust_version(&manifest)
            .with_context(|| format!("read rust-version from {}", manifest.display()))?;
        by_msrv.entry(msrv).or_default().push(package);
    }

    for (msrv, packages) in &by_msrv {
        ensure_toolchain_installed(msrv)?;
        let toolchain = format!("+{msrv}");
        for package in packages {
            for feature_args in [&["--no-default-features"][..], &["--all-features"][..]] {
                let mut cmd = Command::new("cargo");
                cmd.current_dir(&bpk_lib)
                    .arg(&toolchain)
                    .arg("check")
                    .arg("-p")
                    .arg(package);
                for arg in feature_args {
                    cmd.arg(arg);
                }
                println!(
                    "xtask msrv-check: cargo {toolchain} check -p {package} {}",
                    feature_args.join(" ")
                );
                run(cmd).with_context(|| {
                    format!("MSRV check failed for {package} under {msrv} with {feature_args:?}")
                })?;
            }
        }
    }
    println!("xtask msrv-check: all publish crates compile under their declared rust-version");
    Ok(())
}

/// Map a published-crate name (e.g. `"batpak"`) to the directory under
/// `bpk-lib/crates/` (e.g. `"core"` for batpak).
fn crate_dir_for(package: &str) -> &'static str {
    match package {
        "batpak" => "core",
        "syncbat" => "syncbat",
        "netbat" => "netbat",
        other => panic!("msrv-check: unknown publish crate {other}"),
    }
}

/// Parse `rust-version = "X.Y"` out of a Cargo manifest. Returns
/// the version string without surrounding quotes.
fn read_rust_version(manifest: &Path) -> Result<String> {
    let raw = std::fs::read_to_string(manifest)
        .with_context(|| format!("read {}", manifest.display()))?;
    for line in raw.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("rust-version") {
            let rest = rest.trim_start();
            let rest = rest
                .strip_prefix('=')
                .ok_or_else(|| anyhow!("rust-version line malformed in {}", manifest.display()))?;
            let stripped = rest.trim().trim_matches(|c: char| c == '"' || c == '\'');
            if stripped.is_empty() {
                bail!("rust-version is empty in {}", manifest.display());
            }
            return Ok(stripped.to_owned());
        }
    }
    bail!(
        "no `rust-version =` declaration found in {}",
        manifest.display()
    )
}

/// Verify that `rustup` reports the requested toolchain as installed.
/// Fails with an install hint when missing.
fn ensure_toolchain_installed(msrv: &str) -> Result<()> {
    let output = Command::new("rustup")
        .args(["toolchain", "list"])
        .output()
        .with_context(|| "invoke rustup toolchain list")?;
    let listing = String::from_utf8_lossy(&output.stdout);
    if listing.lines().any(|line| line.starts_with(msrv)) {
        return Ok(());
    }
    bail!(
        "rustup toolchain {msrv} is not installed.\n\
         Install with: rustup toolchain install {msrv} --profile minimal"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_rust_version_from_a_synthesized_manifest() {
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(
            tmp.path(),
            "[package]\nname = \"x\"\nversion = \"0\"\nrust-version = \"1.92\"\n",
        )
        .expect("write tmp manifest");
        let parsed = read_rust_version(tmp.path()).expect("parse");
        assert_eq!(parsed, "1.92");
    }

    #[test]
    fn errors_when_rust_version_missing() {
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(tmp.path(), "[package]\nname = \"x\"\n").expect("write tmp manifest");
        let err = read_rust_version(tmp.path()).expect_err("must fail");
        assert!(err.to_string().contains("no `rust-version"));
    }

    #[test]
    fn handles_single_quoted_rust_version() {
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(
            tmp.path(),
            "[package]\nname = \"x\"\nrust-version = '1.92'\n",
        )
        .expect("write tmp manifest");
        let parsed = read_rust_version(tmp.path()).expect("parse");
        assert_eq!(parsed, "1.92");
    }

    #[test]
    fn crate_dir_lookup_covers_publish_crates() {
        assert_eq!(crate_dir_for("batpak"), "core");
        assert_eq!(crate_dir_for("syncbat"), "syncbat");
        assert_eq!(crate_dir_for("netbat"), "netbat");
    }
}
