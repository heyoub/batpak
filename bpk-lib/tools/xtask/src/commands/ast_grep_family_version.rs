use crate::util::{project_root, run};
use anyhow::{bail, Context, Result};
use std::fmt::Write as _;
use std::process::Command;

/// Version-coupled surfaces that must track `batpak`'s workspace release line.
///
/// Uses ast-grep for traceability semver checklist literals and a text scan for
/// family `Cargo.toml` files (ast-grep has no built-in TOML support).
pub(crate) fn ast_grep_family_version() -> Result<()> {
    let root = project_root()?;
    let current = read_family_version(&root)?;
    let stale = stale_patch_versions(&current)?;
    if stale.is_empty() {
        outln!("ast-grep-family-version: ok; no stale patch versions below {current}");
        return Ok(());
    }

    let stale_yaml_alt = stale.join("|");

    let mut inline_rules = String::new();
    writeln!(
        inline_rules,
        r#"id: stale-family-version-traceability
message: Stale current_version in semver checklist; sync to the workspace release line ({current}).
severity: error
language: Yaml
rule:
  kind: plain_scalar
  regex: '^current_version: ({stale_yaml_alt})$'"#
    )
    .expect("writing to an in-memory String is infallible");

    let mut command = Command::new("sg");
    command.current_dir(&root).args([
        "scan",
        "--config",
        "sgconfig.yml",
        "--inline-rules",
        &inline_rules,
        "--report-style",
        "short",
        "--globs",
        "bpk-lib/traceability/public_api/*_semver_checklist.yaml",
    ]);
    run(command).with_context(|| {
        format!(
            "ast-grep family-version calipers found stale literals below {current}; \
             install `sg` via `cargo xtask setup --install-tools`"
        )
    })?;

    scan_family_cargo_versions(&root, &stale, &current)?;

    outln!("ast-grep-family-version: ok; no stale patch versions below {current}");
    Ok(())
}

fn read_family_version(root: &std::path::Path) -> Result<String> {
    let manifest = root.join("bpk-lib/crates/core/Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .with_context(|| format!("read {}", manifest.display()))?;
    let parsed: toml::Value =
        toml::from_str(&text).with_context(|| format!("parse {}", manifest.display()))?;
    parsed
        .get("package")
        .and_then(|package| package.get("version"))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("{} missing package.version", manifest.display()))
}

fn stale_patch_versions(current: &str) -> Result<Vec<String>> {
    let (major, minor, patch) = parse_family_version(current)?;
    if major != 0 || minor != 8 {
        bail!("ast-grep-family-version: unsupported family version line `{current}`");
    }
    Ok((0..patch).map(|value| format!("0.8.{value}")).collect())
}

fn parse_family_version(version: &str) -> Result<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let major = parts
        .next()
        .context("missing major")?
        .parse::<u64>()
        .context("invalid major")?;
    let minor = parts
        .next()
        .context("missing minor")?
        .parse::<u64>()
        .context("invalid minor")?;
    let patch = parts
        .next()
        .context("missing patch")?
        .parse::<u64>()
        .context("invalid patch")?;
    if parts.next().is_some() {
        bail!("unexpected extra semver segments in `{version}`");
    }
    Ok((major, minor, patch))
}

fn scan_family_cargo_versions(
    root: &std::path::Path,
    stale: &[String],
    current: &str,
) -> Result<()> {
    let family_roots = [
        "bpk-lib/crates/bench-support",
        "bpk-lib/crates/core",
        "bpk-lib/crates/macros-support",
        "bpk-lib/crates/macros",
        "bpk-lib/crates/netbat",
        "bpk-lib/crates/syncbat",
        "bpk-lib/tools/xtask",
    ];
    let mut hits = Vec::new();
    for family_root in family_roots {
        let manifest = root.join(family_root).join("Cargo.toml");
        let text = std::fs::read_to_string(&manifest)
            .with_context(|| format!("read {}", manifest.display()))?;
        for line in text.lines() {
            let trimmed = line.trim();
            if !trimmed.starts_with("version = \"") {
                continue;
            }
            for stale_version in stale {
                if trimmed == format!("version = \"{stale_version}\"") {
                    hits.push(format!(
                        "{}: stale package.version `{stale_version}` (expected `{current}`)",
                        manifest.display()
                    ));
                }
            }
        }
    }
    if !hits.is_empty() {
        bail!("family Cargo.toml version drift:\n{}", hits.join("\n"));
    }
    Ok(())
}
