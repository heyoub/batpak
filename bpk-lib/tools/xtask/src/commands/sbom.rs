//! `cargo xtask sbom` — emit a CycloneDX 1.5 SBOM JSON file per publishable
//! crate.
//!
//! High-trust consulting clients increasingly require a Software Bill of
//! Materials next to every deliverable. This subcommand drives
//! [`cargo-cyclonedx`](https://crates.io/crates/cargo-cyclonedx) over every
//! crate in [`crate::publish::PUBLISH_CRATES`] and writes
//! `target/sbom/<crate>.cdx.json` under the Cargo workspace target dir for each.
//!
//! `cargo-cyclonedx` is **a separate install** and is intentionally not
//! auto-installed by this subcommand: consulting clients run the release
//! gates inside clean containers and want deterministic tool versioning.
//! Install with:
//!
//! ```text
//! cargo install cargo-cyclonedx --locked
//! ```
//!
//! When the binary is missing the subcommand fails with a clear install
//! hint instead of silently no-opping.
use crate::publish::PUBLISH_CRATES;
use crate::util::{cargo_target_dir, cargo_target_dir_arg, repo_root};
use anyhow::{bail, Context, Result};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

/// Install hint emitted when `cargo-cyclonedx` is not on PATH.
pub(crate) const INSTALL_HINT: &str =
    "cargo-cyclonedx is not installed; install with `cargo install cargo-cyclonedx --locked`";

/// Drive `cargo cyclonedx` across every publishable crate and copy each
/// generated CycloneDX 1.5 SBOM into the Cargo workspace target dir.
///
/// `cargo-cyclonedx` is a separate install (`cargo install cargo-cyclonedx
/// --locked`); this command never auto-installs it.
pub(crate) fn sbom() -> Result<()> {
    let repo_root = repo_root()?;
    let target_dir = cargo_target_dir()?;
    let sbom_dir = target_dir.join("sbom");
    fs::create_dir_all(&sbom_dir).with_context(|| format!("create {}", sbom_dir.display()))?;

    let target_dir_arg = cargo_target_dir_arg()?;

    for crate_name in PUBLISH_CRATES {
        let crate_dir = locate_crate_dir(&repo_root, crate_name)?;
        run_cyclonedx_for_crate(&repo_root, &crate_dir, crate_name, &target_dir_arg)?;
        let staged = collect_cyclonedx_output(&crate_dir, crate_name)?;
        let dest = sbom_dir.join(format!("{crate_name}.cdx.json"));
        fs::copy(&staged, &dest)
            .with_context(|| format!("copy {} to {}", staged.display(), dest.display()))?;
        // `cargo cyclonedx` writes a sibling SBOM for every other
        // workspace member into that member's directory, with the same
        // filename as the override we passed. Sweep them so the working
        // tree stays clean — consulting clients expect a single canonical
        // artifact under the Cargo workspace target dir, not litter scattered across the
        // workspace.
        sweep_workspace_residue(&repo_root, crate_name)?;
        let bytes = fs::metadata(&dest)
            .with_context(|| format!("stat {}", dest.display()))?
            .len();
        outln!(
            "xtask sbom: wrote {} ({} bytes)",
            display_relative(&dest),
            bytes
        );
    }
    Ok(())
}

/// Remove every `batpak-sbom-<crate>.json` (or `.cdx.json`) file
/// `cargo cyclonedx` may have dropped into a sibling workspace
/// member's directory. Workspace members live under both `crates/` and
/// `tools/`, so sweep both.
fn sweep_workspace_residue(repo_root: &Path, crate_name: &str) -> Result<()> {
    for top in ["crates", "tools"] {
        let dir = repo_root.join(top);
        if !dir.exists() {
            continue;
        }
        for entry in fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            for suffix in [".json", ".cdx.json"] {
                let candidate = entry
                    .path()
                    .join(format!("batpak-sbom-{crate_name}{suffix}"));
                if candidate.exists() {
                    let _ = fs::remove_file(&candidate);
                }
            }
        }
    }
    Ok(())
}

/// Invoke `cargo cyclonedx` for one crate.
///
/// The cyclonedx CLI writes its output next to the crate's `Cargo.toml` by
/// default, so we let it do that and pick the file up afterwards. We pass
/// `--target-dir` via `CARGO_TARGET_DIR` (set inside [`cyclonedx_command`])
/// to honour the same target directory the rest of xtask uses.
///
/// `cargo cyclonedx` selects the crate via `--manifest-path` rather than
/// `-p`; consulting clients want SBOMs at CycloneDX spec 1.5, so we pin
/// that here.
fn run_cyclonedx_for_crate(
    repo_root: &Path,
    crate_dir: &Path,
    crate_name: &str,
    target_dir_arg: &str,
) -> Result<()> {
    let manifest_path = crate_dir.join("Cargo.toml");
    let override_filename = format!("batpak-sbom-{crate_name}");
    let mut command = cyclonedx_command(repo_root, target_dir_arg);
    command.arg("cyclonedx");
    command.arg("--manifest-path").arg(&manifest_path);
    command.args(["--format", "json"]);
    command.args(["--spec-version", "1.5"]);
    command.args(["--override-filename", override_filename.as_str()]);
    let status = command
        .status()
        .with_context(|| format!("run {command:?}"))?;
    interpret_cyclonedx_status(&command, status)
}

/// Build a `cargo cyclonedx ...` invocation. Separated out so tests and
/// callers can inspect the program/args without spawning the binary.
fn cyclonedx_command(repo_root: &Path, target_dir_arg: &str) -> Command {
    let mut command = Command::new("cargo");
    command
        .current_dir(repo_root)
        .env("CARGO_TARGET_DIR", target_dir_arg);
    command
}

/// Translate a `cargo cyclonedx` exit status into an `anyhow::Result`.
///
/// On Unix the kernel reports a missing binary up the stack as a spawn
/// error (handled at the [`Command::status`] call site), but when the
/// binary is invoked through `cargo <subcommand>` cargo itself exits
/// non-zero with a "no such subcommand" message. Either way, this helper
/// surfaces the [`INSTALL_HINT`] so the operator knows the fix.
fn interpret_cyclonedx_status(command: &Command, status: ExitStatus) -> Result<()> {
    if status.success() {
        return Ok(());
    }
    if looks_like_missing_cyclonedx(command) {
        bail!("{INSTALL_HINT}");
    }
    bail!(
        "cargo cyclonedx failed with status {status}; rerun with `--verbose` for details. {INSTALL_HINT}"
    )
}

/// Heuristic: if the program we attempted to spawn was literally
/// `cargo-cyclonedx-...` (used only by the unit test) treat a non-zero
/// status as the missing-binary case. Real invocations go through
/// `cargo cyclonedx` and rely on cargo's own "no such subcommand" exit
/// path, which we still surface via the install hint in the generic
/// branch above.
fn looks_like_missing_cyclonedx(command: &Command) -> bool {
    let program = command.get_program();
    program_name_starts_with(program, "cargo-cyclonedx")
}

fn program_name_starts_with(program: &OsStr, prefix: &str) -> bool {
    Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.starts_with(prefix))
        .unwrap_or(false)
}

/// Locate the file `cargo cyclonedx` produced for `crate_name`.
///
/// The cyclonedx CLI writes into the crate's manifest directory using the
/// `--override-filename` we passed, with a `.cdx.json` suffix.
fn collect_cyclonedx_output(crate_dir: &Path, crate_name: &str) -> Result<PathBuf> {
    // `cargo cyclonedx` writes `<override-filename>.json` when `--format
    // json` is passed; older versions emit `.cdx.json`. Try both.
    for suffix in [".json", ".cdx.json"] {
        let candidate = crate_dir.join(format!("batpak-sbom-{crate_name}{suffix}"));
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!(
        "expected cyclonedx output under {} matching `batpak-sbom-{crate_name}*.json` but no file was produced; {INSTALL_HINT}",
        crate_dir.display()
    )
}

/// Resolve the manifest directory for a crate name within the workspace.
fn locate_crate_dir(repo_root: &Path, crate_name: &str) -> Result<PathBuf> {
    let direct = repo_root.join("crates").join(crate_name);
    if direct.join("Cargo.toml").exists() {
        return Ok(direct);
    }
    // Walk the `crates/` tree once as a fallback in case a crate ever
    // gets renested. Keep the search shallow (depth 2) so we don't pay
    // for an entire workspace traversal.
    let crates_dir = repo_root.join("crates");
    if crates_dir.exists() {
        for entry in
            fs::read_dir(&crates_dir).with_context(|| format!("read {}", crates_dir.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let manifest = entry.path().join("Cargo.toml");
            if manifest.exists() {
                let contents = fs::read_to_string(&manifest).unwrap_or_default();
                if contents.contains(&format!("name = \"{crate_name}\"")) {
                    return Ok(entry.path());
                }
            }
        }
    }
    bail!(
        "could not locate crate directory for `{crate_name}` under {}",
        crates_dir.display()
    )
}

/// Format a path relative to the project root when possible, otherwise
/// fall back to the absolute display.
fn display_relative(path: &Path) -> String {
    if let Ok(repo_root) = repo_root() {
        if let Some(parent) = repo_root.parent() {
            if let Ok(rel) = path.strip_prefix(parent) {
                return rel.display().to_string();
            }
        }
        if let Ok(rel) = path.strip_prefix(&repo_root) {
            return rel.display().to_string();
        }
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::{interpret_cyclonedx_status, INSTALL_HINT};
    use std::process::Command;

    fn nonzero_status() -> std::process::ExitStatus {
        #[cfg(windows)]
        let mut command = {
            let mut command = Command::new("cmd");
            command.args(["/C", "exit", "/B", "1"]);
            command
        };

        #[cfg(not(windows))]
        let mut command = Command::new("false");

        command
            .status()
            .expect("platform command must synthesize a failed ExitStatus")
    }

    #[test]
    fn missing_cyclonedx_binary_reports_install_hint() {
        // Spawn a binary that is guaranteed not to exist. We then ask the
        // helper to interpret the resulting non-zero exit status; the
        // helper must surface the install hint instead of swallowing the
        // failure.
        let mut command = Command::new("cargo-cyclonedx-definitely-not-installed");
        // Best-effort: run the program to obtain a real ExitStatus when
        // possible. On platforms where spawn fails outright (the common
        // case) we synthesize a non-zero ExitStatus through the platform
        // shell.
        let status = match command.status() {
            Ok(status) => {
                assert!(!status.success(), "missing binary should not exit 0");
                status
            }
            Err(_) => nonzero_status(),
        };

        let err = interpret_cyclonedx_status(&command, status)
            .expect_err("missing cyclonedx must surface as an error");
        let message = err.to_string();
        assert!(
            message.contains(INSTALL_HINT),
            "expected install hint in error, got: {message}"
        );
        assert!(
            message.contains("cargo install cargo-cyclonedx --locked"),
            "expected install command in error, got: {message}"
        );
    }
}
