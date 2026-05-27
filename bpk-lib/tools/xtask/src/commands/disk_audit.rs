use crate::util::{project_root, repo_root};
use crate::CleanGeneratedArgs;
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn disk_audit() -> Result<()> {
    let workspace_root = repo_root()?;
    let project_root = project_root()?;
    let workspace_target = workspace_root.join("target");
    let project_target = project_root.join("target");

    if workspace_target.exists() {
        let bytes = dir_size(&workspace_target)
            .with_context(|| format!("measure {}", workspace_target.display()))?;
        println!(
            "disk-audit: workspace artifact target `{}`: {}",
            rel(&project_root, &workspace_target),
            human_bytes(bytes)
        );
    } else {
        println!("disk-audit: workspace artifact target `bpk-lib/target/`: missing");
    }

    let mut violations = Vec::new();
    if project_target.exists() {
        violations.push(format!(
            "repo-root target `{}` is generated cache; use bpk-lib/target/ only",
            rel(&project_root, &project_target)
        ));
    }

    for target in nested_targets(&project_root, &workspace_target)?
        .into_iter()
        .filter(|target| target != &project_target)
    {
        let relative = rel(&project_root, &target);
        if dir_has_entries(&target)? {
            let bytes =
                dir_size(&target).with_context(|| format!("measure {}", target.display()))?;
            violations.push(format!(
                "nested target `{relative}` contains artifacts ({})",
                human_bytes(bytes)
            ));
        } else {
            println!("disk-audit: nested target `{relative}` is empty");
        }
    }

    for lockfile in template_lockfiles(&workspace_root)? {
        violations.push(format!(
            "template lockfile `{}` is generated cache",
            rel(&project_root, &lockfile)
        ));
    }

    for profile in raw_profile_files(&project_root)? {
        violations.push(format!(
            "raw coverage profile `{}` is generated cache",
            rel(&project_root, &profile)
        ));
    }

    if !violations.is_empty() {
        for violation in &violations {
            eprintln!("disk-audit: {violation}");
        }
        bail!(
            "disk-audit found {} generated artifact issue(s)",
            violations.len()
        );
    }

    println!("disk-audit: ok");
    Ok(())
}

pub(crate) fn clean_generated(args: CleanGeneratedArgs) -> Result<()> {
    let workspace_root = repo_root()?;
    let project_root = project_root()?;
    let artifacts = generated_sprawl(&project_root, &workspace_root)?;

    if artifacts.is_empty() {
        println!("clean-generated: nothing to remove");
        return Ok(());
    }

    for artifact in &artifacts {
        let rel = rel(&project_root, artifact.path());
        let action = if args.apply { "remove" } else { "would remove" };
        println!(
            "clean-generated: {action} {} `{rel}`",
            artifact.kind_label()
        );
    }

    if !args.apply {
        println!("clean-generated: dry run; pass --apply to remove these generated artifacts");
        return Ok(());
    }

    for artifact in artifacts {
        artifact.remove()?;
    }
    println!("clean-generated: ok");
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
enum GeneratedArtifact {
    NestedTarget(PathBuf),
    ProjectTarget(PathBuf),
    RawProfile(PathBuf),
    TemplateLockfile(PathBuf),
}

impl GeneratedArtifact {
    fn path(&self) -> &Path {
        match self {
            Self::NestedTarget(path)
            | Self::ProjectTarget(path)
            | Self::RawProfile(path)
            | Self::TemplateLockfile(path) => path,
        }
    }

    fn kind_label(&self) -> &'static str {
        match self {
            Self::NestedTarget(_) => "nested target dir",
            Self::ProjectTarget(_) => "repo-root target dir",
            Self::RawProfile(_) => "raw coverage profile",
            Self::TemplateLockfile(_) => "template lockfile",
        }
    }

    fn remove(&self) -> Result<()> {
        match self {
            Self::NestedTarget(path) | Self::ProjectTarget(path) => {
                fs::remove_dir_all(path).with_context(|| format!("remove {}", path.display()))
            }
            Self::RawProfile(path) | Self::TemplateLockfile(path) => {
                fs::remove_file(path).with_context(|| format!("remove {}", path.display()))
            }
        }
    }
}

fn generated_sprawl(project_root: &Path, workspace_root: &Path) -> Result<Vec<GeneratedArtifact>> {
    let mut artifacts = Vec::new();
    let project_target = project_root.join("target");
    if project_target.exists() {
        artifacts.push(GeneratedArtifact::ProjectTarget(project_target.clone()));
    }
    artifacts.extend(
        nested_targets(project_root, &workspace_root.join("target"))?
            .into_iter()
            .filter(|target| target != &project_target)
            .map(GeneratedArtifact::NestedTarget),
    );
    artifacts.extend(
        template_lockfiles(workspace_root)?
            .into_iter()
            .map(GeneratedArtifact::TemplateLockfile),
    );
    artifacts.extend(
        raw_profile_files(project_root)?
            .into_iter()
            .map(GeneratedArtifact::RawProfile),
    );
    artifacts.sort_by(|left, right| left.path().cmp(right.path()));
    Ok(artifacts)
}

fn nested_targets(scan_root: &Path, allowed_target: &Path) -> Result<Vec<PathBuf>> {
    let mut targets = Vec::new();
    collect_nested_targets(allowed_target, scan_root, &mut targets)?;
    targets.sort();
    Ok(targets)
}

fn collect_nested_targets(
    allowed_target: &Path,
    dir: &Path,
    targets: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if !file_type.is_dir() {
            continue;
        }

        if entry.file_name() == "target" {
            if path != allowed_target {
                targets.push(path);
            }
            continue;
        }

        if should_skip_generated_scan_dir(&entry.file_name()) {
            continue;
        }

        collect_nested_targets(allowed_target, &path, targets)?;
    }
    Ok(())
}

fn template_lockfiles(workspace_root: &Path) -> Result<Vec<PathBuf>> {
    let templates = workspace_root.join("templates");
    let mut lockfiles = Vec::new();
    if !templates.exists() {
        return Ok(lockfiles);
    }

    for entry in fs::read_dir(&templates).context("read templates")? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let lockfile = entry.path().join("Cargo.lock");
        if lockfile.exists() {
            lockfiles.push(lockfile);
        }
    }
    lockfiles.sort();
    Ok(lockfiles)
}

fn raw_profile_files(workspace_root: &Path) -> Result<Vec<PathBuf>> {
    let mut profiles = Vec::new();
    collect_raw_profile_files(workspace_root, &mut profiles)?;
    profiles.sort();
    Ok(profiles)
}

fn collect_raw_profile_files(dir: &Path, profiles: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if should_skip_generated_scan_dir(&entry.file_name()) {
                continue;
            }
            collect_raw_profile_files(&path, profiles)?;
        } else if path.extension().is_some_and(|ext| ext == "profraw") {
            profiles.push(path);
        }
    }
    Ok(())
}

fn should_skip_generated_scan_dir(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(
            ".git"
                | ".claude"
                | ".codex"
                | ".cursor"
                | ".agents"
                | "target"
                | "node_modules"
                | "dist"
        )
    )
}

fn dir_has_entries(path: &Path) -> Result<bool> {
    Ok(fs::read_dir(path)
        .with_context(|| format!("read {}", path.display()))?
        .next()
        .is_some())
}

fn dir_size(path: &Path) -> Result<u64> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }

    let mut total = 0;
    for entry in fs::read_dir(path).with_context(|| format!("read {}", path.display()))? {
        let entry = entry?;
        total += dir_size(&entry.path())?;
    }
    Ok(total)
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::{
        generated_sprawl, human_bytes, nested_targets, raw_profile_files, template_lockfiles,
        GeneratedArtifact,
    };
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn human_bytes_formats_binary_units() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MiB");
    }

    #[test]
    fn finds_nested_targets_without_descending_into_them() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();
        fs::create_dir_all(root.join("crates/core/target/debug")).expect("target");
        fs::create_dir_all(root.join("templates/demo/src")).expect("template");

        let targets = nested_targets(root, &root.join("target")).expect("scan targets");
        assert_eq!(targets, vec![root.join("crates/core/target")]);
    }

    #[test]
    fn finds_target_sprawl_from_project_root_but_allows_workspace_target() {
        let temp = tempdir().expect("tempdir");
        let project = temp.path();
        let workspace = project.join("bpk-lib");
        fs::create_dir_all(workspace.join("target/debug")).expect("workspace target");
        fs::create_dir_all(project.join("bpk-ts/target/debug")).expect("sibling target");
        fs::create_dir_all(project.join("node_modules/pkg/target")).expect("node target");

        let targets = nested_targets(project, &workspace.join("target")).expect("scan targets");
        assert_eq!(targets, vec![project.join("bpk-ts/target")]);
    }

    #[test]
    fn finds_template_lockfiles() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();
        fs::create_dir_all(root.join("templates/demo")).expect("template");
        fs::write(root.join("templates/demo/Cargo.lock"), "").expect("lock");

        let lockfiles = template_lockfiles(root).expect("scan locks");
        assert_eq!(lockfiles, vec![root.join("templates/demo/Cargo.lock")]);
    }

    #[test]
    fn generated_sprawl_combines_only_cleanup_owned_artifacts() {
        let temp = tempdir().expect("tempdir");
        let project = temp.path();
        let root = project.join("bpk-lib");
        fs::create_dir_all(root.join("crates/core/target/debug")).expect("target");
        fs::create_dir_all(root.join("templates/demo")).expect("template");
        fs::create_dir_all(project.join("bpk-ts/target/debug")).expect("sibling target");
        fs::write(project.join("root.profraw"), "").expect("root profile");
        fs::create_dir_all(project.join("target")).expect("project target");
        fs::write(root.join("templates/demo/Cargo.lock"), "").expect("lock");
        fs::write(root.join("crates/core/default.profraw"), "").expect("profile");

        let artifacts = generated_sprawl(project, &root).expect("scan generated artifacts");
        assert_eq!(
            artifacts,
            vec![
                GeneratedArtifact::RawProfile(
                    root.join("crates").join("core").join("default.profraw")
                ),
                GeneratedArtifact::NestedTarget(root.join("crates").join("core").join("target")),
                GeneratedArtifact::TemplateLockfile(
                    root.join("templates").join("demo").join("Cargo.lock")
                ),
                GeneratedArtifact::NestedTarget(project.join("bpk-ts").join("target")),
                GeneratedArtifact::RawProfile(project.join("root.profraw")),
                GeneratedArtifact::ProjectTarget(project.join("target")),
            ]
        );
    }

    #[test]
    fn finds_raw_profile_files_outside_target() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();
        fs::create_dir_all(root.join("target")).expect("target");
        fs::create_dir_all(root.join("crates/core")).expect("crate");
        fs::write(root.join("target/ignored.profraw"), "").expect("target profile");
        fs::write(root.join("crates/core/default.profraw"), "").expect("profile");

        let profiles = raw_profile_files(root).expect("scan profiles");
        assert_eq!(
            profiles,
            vec![root.join("crates").join("core").join("default.profraw")]
        );
    }
}
