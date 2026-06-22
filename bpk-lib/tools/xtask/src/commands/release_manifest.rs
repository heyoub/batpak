use crate::publish::PUBLISH_CRATES;
use crate::util::{cargo_target_dir, project_root, repo_root, run_output};
use crate::ReleaseManifestArgs;
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) fn release_manifest(args: ReleaseManifestArgs) -> Result<()> {
    if args.strict && args.allow_dirty {
        bail!("release-manifest: choose either --strict or --allow-dirty");
    }

    let project_root = project_root()?;
    let repo_root = repo_root()?;
    let target_dir = cargo_target_dir()?;
    let worktree_summary =
        git_output(&project_root, ["status", "--short"]).unwrap_or_else(|_| String::new());
    let dirty = !worktree_summary.trim().is_empty();
    if args.strict && dirty {
        bail!("release-manifest: dirty worktree refused by --strict");
    }

    let manifest = ReleaseManifest {
        head: git_output(&project_root, ["rev-parse", "--short", "HEAD"])?,
        branch: git_output(&project_root, ["branch", "--show-current"])?,
        staged_summary: git_output(&project_root, ["diff", "--cached", "--shortstat"])
            .unwrap_or_else(|_| String::new()),
        worktree_summary,
        dirty,
        rustc: command_output("rustc", ["--version"]).unwrap_or_else(|_| "unavailable".into()),
        cargo: command_output("cargo", ["--version"]).unwrap_or_else(|_| "unavailable".into()),
        uname: command_output("uname", ["-a"]).unwrap_or_else(|_| "unavailable".into()),
        version_pins: version_pin_table(&repo_root)?,
        baselines: public_api_baselines(&project_root)?,
        packages: package_archives(&target_dir.join("package"))?,
        gate_logs: gate_logs(&target_dir.join("release-manifest")),
    };

    let output = target_dir.join("release-manifest.md");
    fs::create_dir_all(&target_dir).with_context(|| format!("create {}", target_dir.display()))?;
    fs::write(&output, render_manifest(&manifest))
        .with_context(|| format!("write {}", output.display()))?;
    outln!("release-manifest: wrote {}", output.display());
    Ok(())
}

struct ReleaseManifest {
    head: String,
    branch: String,
    staged_summary: String,
    worktree_summary: String,
    dirty: bool,
    rustc: String,
    cargo: String,
    uname: String,
    version_pins: Vec<VersionPinRow>,
    baselines: Vec<FileEvidence>,
    packages: Vec<FileEvidence>,
    gate_logs: Vec<GateLog>,
}

struct VersionPinRow {
    package: String,
    version: String,
    manifest: String,
}

struct FileEvidence {
    name: String,
    path: PathBuf,
    sha256: String,
    bytes: u64,
    lines: Option<usize>,
}

struct GateLog {
    command: &'static str,
    path: PathBuf,
    status: &'static str,
}

fn render_manifest(manifest: &ReleaseManifest) -> String {
    let mut out = format!(
        "# batpak Release Manifest\n\n\
         ## Git\n\n\
         - branch: `{}`\n\
         - head: `{}`\n\
         - dirty: `{}`\n\
         - staged: `{}`\n\
         - worktree: `{}`\n\n\
         ## Build Environment\n\n\
         - rustc: `{}`\n\
         - cargo: `{}`\n\
         - host: `{}`\n\n\
         ## ADR Anchors\n\n\
         - ADR-0019 canonical encoding\n\
         - ADR-0026 pre-1.0 correction-cut public surface\n\
         - ADR-0028 syncbat runtime contract\n\
         - ADR-0029 netbat boundary contract\n\n",
        manifest.branch.trim(),
        manifest.head.trim(),
        manifest.dirty,
        one_line(&manifest.staged_summary),
        one_line(&manifest.worktree_summary),
        manifest.rustc.trim(),
        manifest.cargo.trim(),
        manifest.uname.trim(),
    );

    out.push_str("## Version Pins\n\n| Package | Version | Manifest |\n|---|---:|---|\n");
    for row in &manifest.version_pins {
        out.push_str(&format!(
            "| `{}` | `{}` | `{}` |\n",
            row.package, row.version, row.manifest
        ));
    }

    out.push_str("\n## Public API Baselines\n\n| Crate | SHA-256 | Bytes | Lines | Path |\n|---|---|---:|---:|---|\n");
    for file in &manifest.baselines {
        out.push_str(&format!(
            "| `{}` | `{}` | {} | {} | `{}` |\n",
            file.name,
            file.sha256,
            file.bytes,
            file.lines.unwrap_or(0),
            file.path.display()
        ));
    }

    out.push_str("\n## Crate Archives\n\n| Crate | SHA-256 | Bytes | Path |\n|---|---|---:|---|\n");
    for file in &manifest.packages {
        out.push_str(&format!(
            "| `{}` | `{}` | {} | `{}` |\n",
            file.name,
            file.sha256,
            file.bytes,
            file.path.display()
        ));
    }

    out.push_str("\n## Gate Logs\n\n| Command | Status | Log |\n|---|---|---|\n");
    for gate in &manifest.gate_logs {
        out.push_str(&format!(
            "| `{}` | `{}` | `{}` |\n",
            gate.command,
            gate.status,
            gate.path.display()
        ));
    }

    out.push_str("\n## Registry Actions\n\nNo publish or yank is implied by this manifest.\n");
    out
}

fn version_pin_table(root: &Path) -> Result<Vec<VersionPinRow>> {
    let mut rows = Vec::new();
    for package in crate::publish::RELEASE_CHAIN {
        let manifest = manifest_for_package(root, package)?;
        let text = fs::read_to_string(&manifest)
            .with_context(|| format!("read {}", manifest.display()))?;
        let parsed: toml::Value =
            toml::from_str(&text).with_context(|| format!("parse {}", manifest.display()))?;
        let version = parsed
            .get("package")
            .and_then(|package| package.get("version"))
            .and_then(toml::Value::as_str)
            .with_context(|| format!("{} missing package.version", manifest.display()))?;
        rows.push(VersionPinRow {
            package: (*package).to_owned(),
            version: version.to_owned(),
            manifest: manifest
                .strip_prefix(root)
                .unwrap_or(&manifest)
                .display()
                .to_string(),
        });
    }
    Ok(rows)
}

fn manifest_for_package(root: &Path, package: &str) -> Result<PathBuf> {
    let dir = match package {
        "batpak" => "core",
        "batpak-macros-support" => "macros-support",
        "batpak-macros" => "macros",
        "batpak-bench-support" => "bench-support",
        other => other,
    };
    let manifest = root.join("crates").join(dir).join("Cargo.toml");
    if manifest.exists() {
        Ok(manifest)
    } else {
        bail!(
            "no manifest for package `{package}` at {}",
            manifest.display()
        )
    }
}

fn public_api_baselines(project_root: &Path) -> Result<Vec<FileEvidence>> {
    let mut files = Vec::new();
    for package in PUBLISH_CRATES {
        let path = project_root
            .join("bpk-lib")
            .join("traceability")
            .join("public_api")
            .join(format!("{package}.txt"));
        files.push(file_evidence((*package).to_owned(), path, true)?);
    }
    Ok(files)
}

fn package_archives(package_dir: &Path) -> Result<Vec<FileEvidence>> {
    let mut files = Vec::new();
    if !package_dir.exists() {
        return Ok(files);
    }
    for package in PUBLISH_CRATES {
        if let Some(path) = latest_package_archive(package_dir, package)? {
            files.push(file_evidence((*package).to_owned(), path, false)?);
        }
    }
    Ok(files)
}

fn latest_package_archive(package_dir: &Path, package: &str) -> Result<Option<PathBuf>> {
    let mut latest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(package_dir)
        .with_context(|| format!("read package directory {}", package_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !path.is_file()
            || !file_name.starts_with(&format!("{package}-"))
            || !file_name.ends_with(".crate")
        {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .with_context(|| format!("read modified time for {}", path.display()))?;
        match &latest {
            Some((current, _)) if modified <= *current => {}
            _ => latest = Some((modified, path)),
        }
    }
    Ok(latest.map(|(_, path)| path))
}

fn file_evidence(name: String, path: PathBuf, count_lines: bool) -> Result<FileEvidence> {
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let digest = Sha256::digest(&bytes);
    let sha256 = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let lines = if count_lines {
        Some(String::from_utf8_lossy(&bytes).lines().count())
    } else {
        None
    };
    Ok(FileEvidence {
        name,
        path,
        sha256,
        bytes: u64::try_from(bytes.len()).expect("usize fits in u64"),
        lines,
    })
}

fn gate_logs(root: &Path) -> Vec<GateLog> {
    [
        ("cargo xtask preflight", "preflight.log"),
        ("cargo xtask evidence-audit", "evidence-audit.log"),
        ("cargo xtask structural", "structural.log"),
        (
            "cargo xtask public-api --strict --check-baseline",
            "public-api.log",
        ),
        (
            "cargo xtask package-leak-scan --strict-language",
            "package-leak-scan.log",
        ),
    ]
    .into_iter()
    .map(|(command, file)| {
        let path = root.join(file);
        let status = if path.exists() {
            "present"
        } else {
            "not captured"
        };
        GateLog {
            command,
            path,
            status,
        }
    })
    .collect()
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

fn command_output<const N: usize>(program: &str, args: [&str; N]) -> Result<String> {
    let mut command = Command::new(program);
    command.args(args);
    let output = run_output(command)?;
    Ok(String::from_utf8(output.stdout)
        .context("command output utf8")?
        .trim()
        .to_owned())
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
    use super::{one_line, render_manifest, FileEvidence, GateLog, ReleaseManifest, VersionPinRow};
    use std::path::PathBuf;

    #[test]
    fn one_line_collapses_empty_and_multiline_text() {
        assert_eq!(one_line(""), "clean");
        assert_eq!(one_line(" one\n two\tthree "), "one two three");
    }

    #[test]
    fn manifest_renders_proof_sections() {
        let manifest = ReleaseManifest {
            head: "abc123".to_owned(),
            branch: "main".to_owned(),
            staged_summary: "1 file changed".to_owned(),
            worktree_summary: "".to_owned(),
            dirty: false,
            rustc: "rustc 1.92.0".to_owned(),
            cargo: "cargo 1.92.0".to_owned(),
            uname: "Linux test".to_owned(),
            version_pins: vec![VersionPinRow {
                package: "batpak".to_owned(),
                version: "0.8.0".to_owned(),
                manifest: "crates/core/Cargo.toml".to_owned(),
            }],
            baselines: vec![FileEvidence {
                name: "batpak".to_owned(),
                path: PathBuf::from("bpk-lib/traceability/public_api/batpak.txt"),
                sha256: "00".to_owned(),
                bytes: 10,
                lines: Some(1),
            }],
            packages: Vec::new(),
            gate_logs: vec![GateLog {
                command: "cargo xtask public-api --strict --check-baseline",
                path: PathBuf::from("target/release-manifest/public-api.log"),
                status: "present",
            }],
        };
        let rendered = render_manifest(&manifest);
        assert!(rendered.contains("ADR-0028 syncbat runtime contract"));
        assert!(rendered.contains("## Version Pins"));
        assert!(rendered.contains("## Public API Baselines"));
        assert!(rendered.contains("cargo xtask public-api --strict --check-baseline"));
    }
}
