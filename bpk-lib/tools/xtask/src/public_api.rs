use crate::util::{cargo_target_dir, repo_root, run_output};
use crate::{PublicApiArgs, SemverCheckArgs};
use anyhow::{bail, Context, Result};
use std::borrow::Cow;
use std::fs;
use std::process::Command;

struct PublicApiPackage {
    package: &'static str,
    baseline: &'static str,
    features: &'static [&'static str],
}

const PUBLIC_API_PACKAGES: &[PublicApiPackage] = &[
    PublicApiPackage {
        package: "batpak",
        baseline: "batpak.txt",
        features: &[],
    },
    PublicApiPackage {
        package: "syncbat",
        baseline: "syncbat.txt",
        features: &[],
    },
    PublicApiPackage {
        package: "netbat",
        baseline: "netbat.txt",
        features: &[],
    },
];

pub(crate) fn public_api(args: PublicApiArgs) -> Result<()> {
    let root = repo_root()?;
    let target_dir = cargo_target_dir()?.join("public-api");
    fs::create_dir_all(&target_dir).with_context(|| format!("create {}", target_dir.display()))?;
    if args.check_baseline && args.bless_baseline {
        bail!("public-api: choose either --check-baseline or --bless-baseline, not both");
    }

    if !cargo_public_api_is_available() {
        let message = "public-api: cargo-public-api is not installed; advisory run skipped";
        if args.strict {
            bail!("{message}");
        }
        eprintln!("{message}");
        return Ok(());
    }

    for package in PUBLIC_API_PACKAGES {
        let mut command = Command::new("cargo");
        command
            .current_dir(&root)
            .env("CARGO_TARGET_DIR", cargo_target_dir()?)
            .args([
                "public-api",
                "-sss",
                "--color",
                "never",
                "--package",
                package.package,
                "--manifest-path",
                "Cargo.toml",
            ]);
        if !package.features.is_empty() {
            command.arg("--features").arg(package.features.join(","));
        }

        let output = match run_output(command) {
            Ok(output) => output,
            Err(error) if !args.strict => {
                eprintln!(
                    "public-api: advisory run failed for {}: {error:#}",
                    package.package
                );
                return Ok(());
            }
            Err(error) => return Err(error),
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let snapshot = normalize_public_api_snapshot(&stdout);
        let current = target_dir.join(package.baseline);
        let stderr_path = target_dir.join(format!("{}.stderr.txt", package.package));
        fs::write(&current, snapshot.as_ref())
            .with_context(|| format!("write {}", current.display()))?;
        fs::write(&stderr_path, stderr.as_bytes())
            .with_context(|| format!("write {}", stderr_path.display()))?;
        println!("public-api: wrote {}", current.display());

        let baseline = root.join("traceability/public_api").join(package.baseline);
        if args.bless_baseline {
            if let Some(parent) = baseline.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
            fs::write(&baseline, snapshot.as_ref())
                .with_context(|| format!("write {}", baseline.display()))?;
            println!("public-api: blessed {}", baseline.display());
        }
        if args.check_baseline {
            let expected_raw = fs::read_to_string(&baseline)
                .with_context(|| format!("read {}", baseline.display()))?;
            let expected = normalize_public_api_snapshot(&expected_raw);
            if expected != snapshot {
                bail!(
                    "public-api baseline for {} drifted; inspect {} and refresh intentionally with `cargo xtask public-api --strict --bless-baseline`",
                    package.package,
                    current.display()
                );
            }
            println!("public-api: baseline matches {}", baseline.display());
        }
    }
    Ok(())
}

pub(crate) fn semver_check(args: SemverCheckArgs) -> Result<()> {
    let root = repo_root()?;
    let target_dir = cargo_target_dir()?.join("semver-public-api");
    fs::create_dir_all(&target_dir).with_context(|| format!("create {}", target_dir.display()))?;

    if !cargo_semver_checks_is_available() {
        let message = "semver-check: cargo-semver-checks is not installed; advisory run skipped";
        if args.strict {
            bail!("{message}");
        }
        eprintln!("{message}");
        return Ok(());
    }

    let mut combined_stdout = String::new();
    let mut combined_stderr = String::new();
    for package in PUBLIC_API_PACKAGES {
        let manifest_path = format!("crates/{}/Cargo.toml", crate_dir(package.package));
        let mut command = Command::new("cargo");
        command
            .current_dir(&root)
            .env("CARGO_TARGET_DIR", cargo_target_dir()?)
            .args([
                "semver-checks",
                "--manifest-path",
                manifest_path.as_str(),
                "--package",
                package.package,
            ]);
        if !package.features.is_empty() {
            command.arg("--features").arg(package.features.join(","));
        }

        match run_output(command) {
            Ok(output) => {
                combined_stdout.push_str(&format!("## {}\n", package.package));
                combined_stdout.push_str(&String::from_utf8_lossy(&output.stdout));
                combined_stderr.push_str(&format!("## {}\n", package.package));
                combined_stderr.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            Err(error) if !args.strict => {
                combined_stdout.push_str(&format!("## {}\n{error:#}\n", package.package));
                eprintln!(
                    "semver-check: advisory run reported incompatibility or failed for {}",
                    package.package
                );
            }
            Err(error) => return Err(error),
        }
    }

    fs::write(
        target_dir.join("semver-checks.txt"),
        combined_stdout.as_bytes(),
    )
    .context("write target/semver-public-api/semver-checks.txt")?;
    fs::write(
        target_dir.join("semver-checks.stderr.txt"),
        combined_stderr.as_bytes(),
    )
    .context("write target/semver-public-api/semver-checks.stderr.txt")?;
    println!(
        "semver-check: wrote {}",
        target_dir.join("semver-checks.txt").display()
    );
    Ok(())
}

/// Collapse platform-specific spellings so Linux devcontainer and Windows hosts
/// compare the same public-api snapshot text.
fn normalize_public_api_snapshot(text: &str) -> Cow<'_, str> {
    let mut normalized = Cow::Borrowed(text);
    for (from, to) in [
        ("std::net::tcp::TcpListener", "std::net::TcpListener"),
        ("std::net::tcp::TcpStream", "std::net::TcpStream"),
    ] {
        if normalized.contains(from) {
            normalized = Cow::Owned(normalized.replace(from, to));
        }
    }
    normalized
}

fn crate_dir(package: &str) -> &str {
    match package {
        "batpak" => "core",
        other => other,
    }
}

fn cargo_public_api_is_available() -> bool {
    Command::new("cargo-public-api")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn cargo_semver_checks_is_available() -> bool {
    Command::new("cargo-semver-checks")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::PUBLIC_API_PACKAGES;
    use crate::publish::PUBLISH_CRATES;

    use super::normalize_public_api_snapshot;

    #[test]
    fn normalize_public_api_snapshot_collapses_tcp_module_paths() {
        let input = "pub fn netbat::serve_tcp_listener(listener: std::net::tcp::TcpListener)";
        assert_eq!(
            normalize_public_api_snapshot(input),
            "pub fn netbat::serve_tcp_listener(listener: std::net::TcpListener)"
        );
    }

    #[test]
    fn public_api_packages_match_publish_crates() {
        let public = PUBLIC_API_PACKAGES
            .iter()
            .map(|package| package.package)
            .collect::<Vec<_>>();
        assert_eq!(public, PUBLISH_CRATES);
    }
}
