use super::manifest::rewrite_batpak_path_dependency;
use crate::util::{cargo_target_dir, repo_root, run};
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

pub(crate) fn templates() -> Result<()> {
    let repo_root = repo_root()?;
    let manifests = template_manifests(&repo_root)?;
    if manifests.is_empty() {
        bail!("no template Cargo.toml files found under templates/");
    }
    let target_dir = cargo_target_dir()?.join("xtask-template-smoke");

    for manifest in manifests {
        let source_dir = manifest
            .parent()
            .with_context(|| format!("template manifest has no parent: {}", manifest.display()))?;
        let rel = manifest
            .strip_prefix(&repo_root)
            .unwrap_or(&manifest)
            .display()
            .to_string();
        println!("template-smoke: {rel}");

        let smoke_root = TempDir::new().context("create template smoke tempdir")?;
        let smoke_dir = smoke_root
            .path()
            .join(source_dir.file_name().with_context(|| {
                format!("template has no directory name: {}", source_dir.display())
            })?);
        copy_template_source(source_dir, &smoke_dir)?;
        rewrite_manifest_for_smoke(&repo_root, &smoke_dir.join("Cargo.toml"))?;

        let root_lock = repo_root.join("Cargo.lock");
        if root_lock.exists() {
            fs::copy(&root_lock, smoke_dir.join("Cargo.lock"))
                .with_context(|| format!("copy {}", root_lock.display()))?;
        }

        let mut command = Command::new("cargo");
        command
            .arg("test")
            .arg("--manifest-path")
            .arg(smoke_dir.join("Cargo.toml"))
            .arg("--target-dir")
            .arg(&target_dir)
            .env("CARGO_NET_OFFLINE", "true");
        run(command).with_context(|| format!("template smoke failed for {rel}"))?;
    }

    println!("template-smoke: ok");
    Ok(())
}

fn template_manifests(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let templates_dir = repo_root.join("templates");
    let mut manifests = Vec::new();
    for entry in
        fs::read_dir(&templates_dir).with_context(|| format!("read {}", templates_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let manifest = entry.path().join("Cargo.toml");
        if manifest.exists() {
            manifests.push(manifest);
        }
    }
    manifests.sort();
    Ok(manifests)
}

fn copy_template_source(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).with_context(|| format!("create {}", to.display()))?;
    for entry in fs::read_dir(from).with_context(|| format!("read {}", from.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == "target" || name == "Cargo.lock" {
            continue;
        }

        let from_path = entry.path();
        let to_path = to.join(name.as_ref());
        if entry.file_type()?.is_dir() {
            copy_template_source(&from_path, &to_path)?;
        } else {
            fs::copy(&from_path, &to_path).with_context(|| {
                format!("copy {} to {}", from_path.display(), to_path.display())
            })?;
        }
    }
    Ok(())
}

fn rewrite_manifest_for_smoke(repo_root: &Path, manifest: &Path) -> Result<()> {
    let content =
        fs::read_to_string(manifest).with_context(|| format!("read {}", manifest.display()))?;
    let core_path = repo_root
        .join("crates/core")
        .to_string_lossy()
        .replace('\\', "/");
    let updated = rewrite_batpak_path_dependency(&content, &core_path);
    fs::write(manifest, format!("{updated}\n"))
        .with_context(|| format!("write {}", manifest.display()))
}

#[cfg(test)]
mod tests {
    use super::{copy_template_source, rewrite_manifest_for_smoke, template_manifests};
    use anyhow::{Context, Result};
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn template_manifest_inventory_is_sorted_and_ignores_non_crates() -> Result<()> {
        let tmp = TempDir::new()?;
        let templates = tmp.path().join("templates");
        fs::create_dir_all(templates.join("zeta"))?;
        fs::create_dir_all(templates.join("alpha"))?;
        fs::create_dir_all(templates.join("notes"))?;
        fs::write(templates.join("zeta/Cargo.toml"), "[package]\n")?;
        fs::write(templates.join("alpha/Cargo.toml"), "[package]\n")?;
        fs::write(templates.join("README.md"), "templates")?;

        let manifests = template_manifests(tmp.path())?;
        let mut names = Vec::new();
        for path in &manifests {
            let parent = path.parent().context("manifest path has parent")?;
            let name = parent.file_name().context("template path has name")?;
            names.push(name.to_string_lossy().into_owned());
        }

        assert_eq!(names, ["alpha", "zeta"]);
        Ok(())
    }

    #[test]
    fn template_copy_skips_generated_lock_and_target() -> Result<()> {
        let tmp = TempDir::new()?;
        let source = tmp.path().join("source");
        fs::create_dir_all(source.join("src"))?;
        fs::create_dir_all(source.join("target"))?;
        fs::write(source.join("Cargo.toml"), "[package]\n")?;
        fs::write(source.join("Cargo.lock"), "generated")?;
        fs::write(source.join("target/artifact"), "generated")?;
        fs::write(source.join("src/lib.rs"), "")?;

        let dest = tmp.path().join("dest");
        copy_template_source(&source, &dest)?;

        assert!(dest.join("Cargo.toml").exists());
        assert!(dest.join("src/lib.rs").exists());
        assert!(!dest.join("Cargo.lock").exists());
        assert!(!dest.join("target").exists());
        Ok(())
    }

    #[test]
    fn smoke_manifest_rewrites_batpak_path_to_current_checkout() -> Result<()> {
        let tmp = TempDir::new()?;
        let repo = tmp.path().join("repo");
        let manifest = tmp.path().join("Cargo.toml");
        fs::create_dir_all(repo.join("crates/core"))?;
        fs::write(
            &manifest,
            "[dependencies]\nbatpak = { path = \"../../crates/core\", features = [\"blake3\"] }\n",
        )?;

        rewrite_manifest_for_smoke(&repo, &manifest)?;
        let updated = fs::read_to_string(manifest)?;

        assert!(updated.contains("batpak = { path = \""));
        assert!(updated.contains("crates/core"));
        Ok(())
    }
}
