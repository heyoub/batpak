use crate::util::{cargo_target_dir, project_root, run_output};
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) fn release_manifest() -> Result<()> {
    let project_root = project_root()?;
    let target_dir = cargo_target_dir()?;
    let manifest = ReleaseManifest {
        head: git_output(&project_root, ["rev-parse", "--short", "HEAD"])?,
        branch: git_output(&project_root, ["branch", "--show-current"])?,
        staged_summary: git_output(&project_root, ["diff", "--cached", "--shortstat"])
            .unwrap_or_else(|_| "no staged summary available".to_owned()),
        worktree_summary: git_output(&project_root, ["status", "--short"])
            .unwrap_or_else(|_| "no status available".to_owned()),
        package_archive: latest_package_archive(&target_dir.join("package")),
    };

    let output = target_dir.join("release-manifest.md");
    fs::create_dir_all(&target_dir).with_context(|| format!("create {}", target_dir.display()))?;
    fs::write(&output, render_manifest(&manifest))
        .with_context(|| format!("write {}", output.display()))?;
    println!("release-manifest: wrote {}", output.display());
    Ok(())
}

struct ReleaseManifest {
    head: String,
    branch: String,
    staged_summary: String,
    worktree_summary: String,
    package_archive: Option<PathBuf>,
}

fn render_manifest(manifest: &ReleaseManifest) -> String {
    let package = manifest
        .package_archive
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| {
            "not found; run `cargo xtask package-leak-scan --allow-dirty`".to_owned()
        });

    format!(
        "# batpak Release Manifest\n\n\
         ## Git\n\n\
         - branch: `{}`\n\
         - head: `{}`\n\
         - staged: `{}`\n\
         - worktree: `{}`\n\n\
         ## Package\n\n\
         - local crate archive: `{}`\n\n\
         ## Proof Commands\n\n\
         - `cargo xtask layout`\n\
         - `cargo xtask boundary`\n\
         - `cargo xtask stale-paths`\n\
         - `cargo xtask disk-audit`\n\
         - `cargo xtask clean-generated`\n\
         - `cargo xtask template-freshness`\n\
         - `cargo xtask semver-check`\n\
         - `cargo xtask public-api`\n\
         - `cargo xtask package-leak-scan --allow-dirty`\n\
         - `cargo xtask staged-diff`\n\
         - `cargo xtask ci`\n\n\
         ## Registry Actions\n\n\
         No publish, yank, or version bump is implied by this manifest.\n",
        manifest.branch.trim(),
        manifest.head.trim(),
        one_line(&manifest.staged_summary),
        one_line(&manifest.worktree_summary),
        package
    )
}

fn git_output<const N: usize>(root: &Path, args: [&str; N]) -> Result<String> {
    let mut command = Command::new("git");
    command.current_dir(root).args(args);
    let output = run_output(command)?;
    Ok(String::from_utf8(output.stdout)
        .context("git output utf8")?
        .trim()
        .to_owned())
}

fn latest_package_archive(package_dir: &Path) -> Option<PathBuf> {
    let mut latest: Option<(std::time::SystemTime, PathBuf)> = None;
    let entries = fs::read_dir(package_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = path.file_name().and_then(|name| name.to_str())?;
        if !path.is_file() || !file_name.starts_with("batpak-") || !file_name.ends_with(".crate") {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()?;
        match &latest {
            Some((current, _)) if modified <= *current => {}
            _ => latest = Some((modified, path)),
        }
    }
    latest.map(|(_, path)| path)
}

fn one_line(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        "clean".to_owned()
    } else {
        compact
    }
}

#[cfg(test)]
mod tests {
    use super::{one_line, render_manifest, ReleaseManifest};

    #[test]
    fn one_line_collapses_empty_and_multiline_text() {
        assert_eq!(one_line(""), "clean");
        assert_eq!(one_line(" one\n two\tthree "), "one two three");
    }

    #[test]
    fn manifest_states_registry_actions_are_not_implied() {
        let manifest = ReleaseManifest {
            head: "abc123".to_owned(),
            branch: "main".to_owned(),
            staged_summary: "1 file changed".to_owned(),
            worktree_summary: "".to_owned(),
            package_archive: None,
        };
        let rendered = render_manifest(&manifest);
        assert!(rendered.contains("No publish, yank, or version bump is implied"));
        assert!(rendered.contains("cargo xtask package-leak-scan --allow-dirty"));
    }
}
