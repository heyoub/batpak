use crate::coverage;
use crate::devcontainer::run_in_devcontainer;
use crate::util::repo_root;
use crate::{CoverArgs, DocsArgs};
use anyhow::{bail, Context, Result};
use std::fs;
use std::process::Command;
use toml::Value as TomlValue;

/// Reproduce the canonical verification bundle inside the devcontainer.
///
/// The host enters the container once, then the container runs CI, coverage,
/// and docs in-process so we do not pay repeated container-entry ceremony for
/// one logical proof.
pub(crate) fn preflight() -> Result<()> {
    if std::env::var_os("DEVCONTAINER").is_some() {
        return preflight_inner();
    }

    run_in_devcontainer(&["cargo", "xtask", "preflight"])
}

fn preflight_inner() -> Result<()> {
    assert_rustc_matches_toolchain_pin()?;
    crate::commands::ci()?;
    coverage::cover(CoverArgs {
        ci: true,
        json: false,
        threshold: Some(80),
    })?;
    crate::docs::docs(DocsArgs { open: false })
}

/// Read `rust-toolchain.toml`, parse the pinned `channel`, shell out to
/// `rustc --version`, and fail the build if the two disagree. This catches
/// devcontainer images that were rebuilt with a drifted rustc and any host
/// environment that tries to `cargo xtask preflight` with the wrong toolchain.
pub(crate) fn assert_rustc_matches_toolchain_pin() -> Result<()> {
    let root = repo_root()?;
    let toolchain_path = root.join("rust-toolchain.toml");
    let toolchain_toml = fs::read_to_string(&toolchain_path)
        .with_context(|| format!("read {}", toolchain_path.display()))?;
    let pinned_channel = parse_toolchain_channel(&toolchain_toml).with_context(|| {
        format!(
            "could not parse `[toolchain].channel` from {}",
            toolchain_path.display()
        )
    })?;

    let output = Command::new("rustc")
        .arg("--version")
        .output()
        .context("invoke rustc --version to verify toolchain pin")?;
    if !output.status.success() {
        bail!(
            "rustc --version failed with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let active_version = parse_rustc_version(&stdout)
        .with_context(|| format!("could not parse rustc version from `{stdout}`"))?;

    if !channels_match(&pinned_channel, &active_version) {
        bail!(
            "rustc version mismatch: rust-toolchain.toml pins `{pinned_channel}` but active rustc reports `{active_version}` (full output: `{stdout}`).\n\
             Rebuild the devcontainer so the pinned toolchain is installed, or run `rustup override set {pinned_channel}` on the host."
        );
    }
    Ok(())
}

fn parse_toolchain_channel(toml: &str) -> Result<String> {
    let parsed: TomlValue = toml::from_str(toml).context("parse rust-toolchain.toml as TOML")?;
    let channel = parsed
        .get("toolchain")
        .and_then(TomlValue::as_table)
        .and_then(|toolchain| toolchain.get("channel"))
        .and_then(TomlValue::as_str)
        .filter(|channel| !channel.is_empty())
        .context("missing or non-string `[toolchain].channel`")?;
    Ok(channel.to_owned())
}

fn parse_rustc_version(output: &str) -> Option<String> {
    // Expected shape: "rustc 1.92.0 (abcdef0 2026-04-17)".
    let mut iter = output.split_whitespace();
    let _ = iter.next()?;
    let version = iter.next()?;
    Some(version.to_string())
}

fn channels_match(pinned: &str, active: &str) -> bool {
    // Treat "1.92" as matching any "1.92.x" and "1.92.0" as requiring exact.
    // Likewise "stable"/"beta"/"nightly" match their own name as a prefix of
    // the active version (rustc never reports those literal strings, so this
    // branch is informational).
    if pinned == active {
        return true;
    }
    if pinned.chars().all(|ch| ch.is_ascii_digit() || ch == '.') {
        let pinned_parts: Vec<&str> = pinned.split('.').collect();
        let active_parts: Vec<&str> = active.split('.').collect();
        if pinned_parts.len() <= active_parts.len()
            && pinned_parts
                .iter()
                .zip(active_parts.iter())
                .all(|(p, a)| p == a)
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{channels_match, parse_rustc_version, parse_toolchain_channel};

    #[test]
    fn parse_channel_from_toolchain_toml() {
        let toml = "[toolchain]\nchannel = \"1.92.0\"\nprofile = \"minimal\"\n";
        // justifies: INV-TEST-PANIC-AS-ASSERTION; test-only in tools/xtask/src/preflight.rs; panic on setup failure is the test's signal of broken fixtures
        let channel = parse_toolchain_channel(toml).expect("channel present in fixture");
        assert_eq!(channel, "1.92.0");
    }

    #[test]
    fn parse_channel_from_toolchain_toml_with_comments_and_spacing() {
        let toml = r#"
            [toolchain]
            profile = "minimal"
            channel = "1.92.0" # canonical pin
            components = ["rustfmt", "clippy"]
        "#;
        let channel = parse_toolchain_channel(toml).expect("channel present in fixture");
        assert_eq!(channel, "1.92.0");
    }

    #[test]
    fn parse_channel_requires_toolchain_table_and_string_channel() {
        let missing_table = "channel = \"1.92.0\"\n";
        let wrong_type = "[toolchain]\nchannel = 192\n";

        assert!(parse_toolchain_channel(missing_table).is_err());
        assert!(parse_toolchain_channel(wrong_type).is_err());
    }

    #[test]
    fn parse_rustc_version_from_output() {
        // justifies: INV-TEST-PANIC-AS-ASSERTION; test-only in tools/xtask/src/preflight.rs; panic on setup failure is the test's signal of broken fixtures
        let version = parse_rustc_version("rustc 1.92.0 (abcdef0 2026-04-17)")
            .expect("version present in fixture");
        assert_eq!(version, "1.92.0");
    }

    #[test]
    fn channels_match_exact_and_prefix() {
        assert!(channels_match("1.92.0", "1.92.0"));
        assert!(channels_match("1.92", "1.92.0"));
        assert!(!channels_match("1.92.1", "1.92.0"));
        assert!(!channels_match("1.91", "1.92.0"));
    }
}
