use crate::publish::PUBLISH_CRATES;
use crate::util::{cargo_target_dir, repo_root, run, run_output};
use crate::PackageLeakScanArgs;
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) fn package_leak_scan(args: PackageLeakScanArgs) -> Result<()> {
    let root = repo_root()?;
    let mut total_entries = 0_usize;
    let mut scanned_archives = Vec::new();
    let mut findings = Vec::new();
    for package_name in PUBLISH_CRATES {
        package(&root, package_name, args.allow_dirty)?;
        let archive = latest_packaged_crate(&cargo_target_dir()?.join("package"), package_name)?;
        let entries = package_entries(&archive)?;
        total_entries += entries.len();
        findings.extend(scan_archive(&archive, &entries)?);
        scanned_archives.push(archive);
    }
    findings.sort();
    findings.dedup();

    let hard: Vec<_> = findings
        .iter()
        .filter(|finding| finding.severity == Severity::Hard)
        .collect();
    let language: Vec<_> = findings
        .iter()
        .filter(|finding| finding.severity == Severity::Language)
        .collect();

    for finding in &hard {
        eprintln!(
            "package-leak-scan: hard leak: {} in {}",
            finding.needle, finding.entry
        );
    }
    for finding in &language {
        eprintln!(
            "package-leak-scan: language warning: {} in {}",
            finding.needle, finding.entry
        );
    }

    if !hard.is_empty() || (args.strict_language && !language.is_empty()) {
        bail!(
            "package-leak-scan found {} hard leak(s), {} language warning(s)",
            hard.len(),
            language.len()
        );
    }

    println!(
        "package-leak-scan: ok; scanned {} file(s) across {} crate archive(s)",
        total_entries,
        scanned_archives.len()
    );
    Ok(())
}

fn package(root: &Path, package_name: &str, allow_dirty: bool) -> Result<()> {
    let mut command = Command::new("cargo");
    command
        .current_dir(root)
        .args(["package", "-p", package_name, "--locked", "--no-verify"]);
    if allow_dirty {
        command.arg("--allow-dirty");
    }
    // Internal path-deps (batpak-macros, batpak-macros-support,
    // syncbat-macros, batpak-bench-support) are at version 0.8.0 in
    // this workspace but only 0.7.0 is published on crates.io. Without
    // these `--config patch.crates-io.<name>.path=...` overrides,
    // `cargo package` would try to resolve the path-dep from
    // crates.io and fail with "failed to select a version for the
    // requirement". Mirrors release.rs::consumer_smoke which uses
    // the same pattern.
    for (name, relative_path) in [
        ("batpak-macros-support", "crates/macros-support"),
        ("batpak-macros", "crates/macros"),
        ("batpak-bench-support", "crates/bench-support"),
        ("syncbat-macros", "crates/syncbat-macros"),
        ("batpak", "crates/core"),
        ("syncbat", "crates/syncbat"),
    ] {
        command
            .arg("--config")
            .arg(format!("patch.crates-io.{name}.path=\"{relative_path}\""));
    }
    run(command)
}

fn package_entries(archive: &Path) -> Result<Vec<String>> {
    let mut command = Command::new("tar");
    command.arg("tf").arg(archive);
    let output = run_output(command)?;
    let stdout = String::from_utf8(output.stdout).context("tar file list utf8")?;
    let mut entries: Vec<String> = stdout
        .lines()
        .filter(|line| !line.ends_with('/'))
        .map(str::to_owned)
        .collect();
    entries.sort();
    Ok(entries)
}

fn scan_archive(archive: &Path, entries: &[String]) -> Result<Vec<Finding>> {
    let mut findings = Vec::new();
    for entry in entries {
        findings.extend(scan_bytes(entry, entry.as_bytes()));

        let mut command = Command::new("tar");
        command.arg("xOf").arg(archive).arg(entry);
        let output = run_output(command).with_context(|| format!("read package entry {entry}"))?;
        findings.extend(scan_bytes(entry, &output.stdout));
    }
    findings.sort();
    findings.dedup();
    Ok(findings)
}

fn latest_packaged_crate(package_dir: &Path, package: &str) -> Result<PathBuf> {
    let mut latest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(package_dir)
        .with_context(|| format!("read packaged crate directory {}", package_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !path.is_file() || !is_package_archive(file_name, package) {
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

    latest
        .map(|(_, path)| path)
        .with_context(|| format!("could not locate packaged {package} .crate archive"))
}

fn is_package_archive(file_name: &str, package: &str) -> bool {
    let Some(rest) = file_name
        .strip_prefix(package)
        .and_then(|rest| rest.strip_prefix('-'))
        .and_then(|rest| rest.strip_suffix(".crate"))
    else {
        return false;
    };
    rest.chars().next().is_some_and(|ch| ch.is_ascii_digit())
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Severity {
    Hard,
    Language,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Finding {
    severity: Severity,
    needle: &'static str,
    entry: String,
}

#[derive(Clone, Copy)]
struct Needle {
    text: &'static str,
    severity: Severity,
}

fn scan_bytes(entry: &str, bytes: &[u8]) -> Vec<Finding> {
    needles()
        .iter()
        .filter(|needle| contains_ascii_case_insensitive(bytes, needle.text.as_bytes()))
        .map(|needle| Finding {
            severity: needle.severity,
            needle: needle.text,
            entry: entry.to_owned(),
        })
        .collect()
}

fn needles() -> &'static [Needle] {
    &[
        Needle {
            text: concat!("/", "home", "/"),
            severity: Severity::Hard,
        },
        Needle {
            text: concat!("/", "Users", "/"),
            severity: Severity::Hard,
        },
        Needle {
            text: "Documents/code/",
            severity: Severity::Hard,
        },
        Needle {
            text: concat!("/", "tmp", "/", "claude-"),
            severity: Severity::Hard,
        },
        Needle {
            text: concat!("/", "tmp", "/", "codex-"),
            severity: Severity::Hard,
        },
        Needle {
            text: "BEGIN PRIVATE KEY",
            severity: Severity::Hard,
        },
        Needle {
            text: "Authorization: Bearer",
            severity: Severity::Hard,
        },
        Needle {
            text: "ghp_",
            severity: Severity::Hard,
        },
        Needle {
            text: "xoxb-",
            severity: Severity::Hard,
        },
        Needle {
            text: "do not publish",
            severity: Severity::Hard,
        },
        Needle {
            text: "proprietary doctrine",
            severity: Severity::Hard,
        },
        Needle {
            text: "private register",
            severity: Severity::Hard,
        },
        Needle {
            text: "internal codename",
            severity: Severity::Hard,
        },
        Needle {
            text: "oath lane",
            severity: Severity::Hard,
        },
        Needle {
            text: "agent runtime",
            severity: Severity::Hard,
        },
        Needle {
            text: "delegated-work",
            severity: Severity::Hard,
        },
        Needle {
            text: "SaaS",
            severity: Severity::Hard,
        },
        Needle {
            text: "sovereign",
            severity: Severity::Hard,
        },
        Needle {
            text: "lawful",
            severity: Severity::Hard,
        },
        Needle {
            text: "control plane",
            severity: Severity::Language,
        },
        Needle {
            text: "policy gates",
            severity: Severity::Language,
        },
        Needle {
            text: "commit authority",
            severity: Severity::Language,
        },
        Needle {
            text: "tenant",
            severity: Severity::Language,
        },
        Needle {
            text: "toll",
            severity: Severity::Language,
        },
    ]
}

fn contains_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| eq_ascii_case_insensitive(window, needle))
}

fn eq_ascii_case_insensitive(left: &[u8], right: &[u8]) -> bool {
    left.iter()
        .zip(right.iter())
        .all(|(left, right)| left.eq_ignore_ascii_case(right))
}

#[cfg(test)]
mod tests {
    use super::{contains_ascii_case_insensitive, scan_bytes, Severity};

    #[test]
    fn ascii_scan_is_case_insensitive() {
        assert!(contains_ascii_case_insensitive(
            b"this mentions SaaS language",
            b"saas"
        ));
        assert!(!contains_ascii_case_insensitive(b"short", b"longer needle"));
    }

    #[test]
    fn scanner_classifies_secret_like_strings_as_hard() {
        let findings = scan_bytes("README.md", b"Authorization: Bearer token");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Hard);
    }

    #[test]
    fn scanner_classifies_broad_language_as_warning() {
        let findings = scan_bytes("README.md", b"control plane");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Language);
    }
}
