use crate::util::repo_root;
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn check_version_pins() -> Result<()> {
    let root = repo_root()?;
    let members = workspace_members(&root)?;
    let mut versions_by_manifest = BTreeMap::new();

    for member in &members {
        let manifest = root.join(member).join("Cargo.toml");
        let parsed = read_manifest(&manifest)?;
        let package = parsed
            .get("package")
            .and_then(toml::Value::as_table)
            .with_context(|| format!("{} missing [package]", manifest.display()))?;
        let name = string_value(package, "name", &manifest)?;
        let version = string_value(package, "version", &manifest)?;
        versions_by_manifest.insert(canonical_manifest(&manifest)?, (name, version));
    }

    let mut mismatches = Vec::new();
    for member in &members {
        let manifest = root.join(member).join("Cargo.toml");
        let parsed = read_manifest(&manifest)?;
        for table_name in ["dependencies", "dev-dependencies", "build-dependencies"] {
            let Some(table) = parsed.get(table_name).and_then(toml::Value::as_table) else {
                continue;
            };
            for (dep_name, dep_value) in table {
                let Some(dep_table) = dep_value.as_table() else {
                    continue;
                };
                let (Some(path), Some(actual)) = (
                    dep_table.get("path").and_then(toml::Value::as_str),
                    dep_table.get("version").and_then(toml::Value::as_str),
                ) else {
                    continue;
                };
                let dep_manifest = manifest
                    .parent()
                    .expect("manifest has parent")
                    .join(path)
                    .join("Cargo.toml");
                let dep_manifest = canonical_manifest(&dep_manifest)?;
                let Some((declared_name, expected)) = versions_by_manifest.get(&dep_manifest)
                else {
                    continue;
                };
                if declared_name != dep_name {
                    mismatches.push(format!(
                        "{}: dependency `{dep_name}` points to package `{declared_name}`",
                        manifest.display()
                    ));
                }
                if actual != expected {
                    mismatches.push(format!(
                        "{}:{}: dependency `{dep_name}` pins `{actual}`, expected `{expected}`",
                        manifest.display(),
                        dependency_line(&manifest, dep_name).unwrap_or(0)
                    ));
                }
            }
        }
    }

    if !mismatches.is_empty() {
        bail!("version pin drift:\n{}", mismatches.join("\n"));
    }

    outln!(
        "check-version-pins: ok; checked {} workspace member(s)",
        members.len()
    );
    Ok(())
}

fn workspace_members(root: &Path) -> Result<Vec<PathBuf>> {
    let manifest = root.join("Cargo.toml");
    let parsed = read_manifest(&manifest)?;
    let members = parsed
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .with_context(|| format!("{} missing workspace.members", manifest.display()))?;
    Ok(members
        .iter()
        .filter_map(toml::Value::as_str)
        .map(PathBuf::from)
        .collect())
}

fn read_manifest(path: &Path) -> Result<toml::Value> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

fn canonical_manifest(path: &Path) -> Result<PathBuf> {
    fs::canonicalize(path).with_context(|| format!("canonicalize {}", path.display()))
}

fn string_value(
    table: &toml::map::Map<String, toml::Value>,
    key: &str,
    manifest: &Path,
) -> Result<String> {
    table
        .get(key)
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("{} missing package.{key}", manifest.display()))
}

fn dependency_line(manifest: &Path, dependency: &str) -> Result<usize> {
    let text =
        fs::read_to_string(manifest).with_context(|| format!("read {}", manifest.display()))?;
    let needle = format!("{dependency} =");
    Ok(text
        .lines()
        .position(|line| line.trim_start().starts_with(&needle))
        .map(|index| index + 1)
        .unwrap_or(0))
}
