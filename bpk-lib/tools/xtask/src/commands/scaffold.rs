use super::manifest::rewrite_batpak_path_dependency;
use crate::util::{copy_dir, repo_root};
use crate::{ScaffoldArgs, ScaffoldPattern};
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::Path;

pub(crate) fn scaffold(args: ScaffoldArgs) -> Result<()> {
    let repo_root = repo_root()?;
    let template = repo_root
        .join("templates")
        .join(args.pattern.template_dir());
    if !template.exists() {
        bail!(
            "scaffold template `{}` is missing at {}",
            args.pattern.template_dir(),
            template.display()
        );
    }

    let package_name = safe_package_name(&args.name)?;
    let base = match args.path {
        Some(path) => path,
        None => std::env::current_dir().context("get cwd")?,
    };
    let dest = base.join(&package_name);
    if dest.exists() && !args.force {
        bail!(
            "destination exists: {} (pass --force to copy over existing files)",
            dest.display()
        );
    }

    copy_dir(&template, &dest)?;
    rewrite_template_references(&dest, args.pattern.template_dir(), &package_name)?;
    rewrite_manifest(&repo_root, &dest.join("Cargo.toml"), &package_name)?;
    outln!("scaffolded {} at {}", args.pattern.as_str(), dest.display());
    outln!("next:");
    outln!("  cd {}", dest.display());
    outln!("  cargo test");
    Ok(())
}

fn rewrite_template_references(dest: &Path, template_dir: &str, package_name: &str) -> Result<()> {
    let old_crate = format!("batpak_template_{}", template_dir.replace('-', "_"));
    let new_crate = package_name.replace('-', "_");
    rewrite_crate_reference_dir(dest, &old_crate, &new_crate)
}

fn rewrite_crate_reference_dir(dir: &Path, old_crate: &str, new_crate: &str) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            rewrite_crate_reference_dir(&path, old_crate, new_crate)?;
            continue;
        }
        let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
            continue;
        };
        if ext != "rs" && ext != "md" {
            continue;
        }
        let content =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let updated = content.replace(old_crate, new_crate);
        if updated != content {
            fs::write(&path, updated).with_context(|| format!("write {}", path.display()))?;
        }
    }
    Ok(())
}

fn rewrite_manifest(repo_root: &Path, manifest: &Path, package_name: &str) -> Result<()> {
    let mut content =
        fs::read_to_string(manifest).with_context(|| format!("read {}", manifest.display()))?;
    content = replace_package_name(&content, package_name);
    let core_path = repo_root
        .join("crates/core")
        .to_string_lossy()
        .replace('\\', "/");
    content = rewrite_batpak_path_dependency(&content, &core_path);
    content.push('\n');
    fs::write(manifest, content).with_context(|| format!("write {}", manifest.display()))
}

fn replace_package_name(content: &str, package_name: &str) -> String {
    let mut replaced = false;
    content
        .lines()
        .map(|line| {
            if !replaced && line.trim_start().starts_with("name = ") {
                replaced = true;
                format!("name = \"{package_name}\"")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn safe_package_name(raw: &str) -> Result<String> {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.trim().chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if ch == '-' || ch == '_' || ch.is_whitespace() {
            Some('-')
        } else {
            None
        };
        let Some(ch) = mapped else {
            bail!("package name contains unsupported character `{ch}`");
        };
        if ch == '-' {
            if prev_dash || out.is_empty() {
                continue;
            }
            prev_dash = true;
        } else {
            prev_dash = false;
        }
        out.push(ch);
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty()
        || !out
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic())
    {
        bail!("package name must start with an ASCII letter after normalization");
    }
    Ok(out)
}

impl ScaffoldPattern {
    pub(crate) fn template_dir(self) -> &'static str {
        match self {
            ScaffoldPattern::TypedStore => "minimal-store",
            ScaffoldPattern::Reactor => "typed-reactor",
            ScaffoldPattern::EvidenceRead => "audit-read-report",
            ScaffoldPattern::ProjectionCache => "projection-cache",
            ScaffoldPattern::ArtifactEnvelope => "artifact-envelope",
            ScaffoldPattern::RegistryRow => "registry-row",
            ScaffoldPattern::BackupEnvelope => "backup-envelope",
            ScaffoldPattern::StateTransition => "state-transition",
            ScaffoldPattern::ReservationLedger => "reservation-ledger",
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ScaffoldPattern::TypedStore => "typed-store",
            ScaffoldPattern::Reactor => "reactor",
            ScaffoldPattern::EvidenceRead => "evidence-read",
            ScaffoldPattern::ProjectionCache => "projection-cache",
            ScaffoldPattern::ArtifactEnvelope => "artifact-envelope",
            ScaffoldPattern::RegistryRow => "registry-row",
            ScaffoldPattern::BackupEnvelope => "backup-envelope",
            ScaffoldPattern::StateTransition => "state-transition",
            ScaffoldPattern::ReservationLedger => "reservation-ledger",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{replace_package_name, safe_package_name};

    #[test]
    fn safe_package_name_normalizes_agent_input() {
        assert_eq!(
            safe_package_name("My App").expect("agent input should normalize"),
            "my-app"
        );
        assert_eq!(
            safe_package_name("my__app").expect("agent input should normalize"),
            "my-app"
        );
        assert!(safe_package_name("99-app").is_err());
    }

    #[test]
    fn manifest_rewrite_only_replaces_package_name() {
        let input = "[package]\nname = \"old\"\nversion = \"0.1.0\"\n";
        let out = replace_package_name(input, "new-name");
        assert!(out.contains("name = \"new-name\""));
        assert!(out.contains("version = \"0.1.0\""));
    }

    #[test]
    fn template_crate_name_follows_package_name() {
        let package = "agent-smoke";
        assert_eq!(package.replace('-', "_"), "agent_smoke");
    }
}
