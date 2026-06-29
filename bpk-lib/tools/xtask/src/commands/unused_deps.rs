//! `xtask unused-deps` — detect dependencies declared in any
//! workspace crate's `Cargo.toml` that are never referenced from source.
//!
//! Backed by `cargo-machete`. Each unused dep widens the supply-chain
//! blast radius and slows builds; this gate keeps the dep set tight.
//!
//! `cargo xtask setup --install-tools` owns the pinned tool install. If
//! `cargo-machete` is not on PATH, this command emits a clear setup hint and
//! exits non-zero (advisory). Consulting clients run the gate in clean
//! containers and want deterministic tool versioning — no auto-install.

use anyhow::{anyhow, Context, Result};
use std::process::Command;

use crate::util::{repo_root, run};

/// Run `cargo machete` over the workspace.
pub(crate) fn unused_deps() -> Result<()> {
    if !is_machete_installed() {
        errln!(
            "xtask unused-deps: cargo-machete is not installed.\n\
             Install with: cargo xtask setup --install-tools"
        );
        return Err(anyhow!("cargo-machete not on PATH"));
    }

    let root = repo_root()?;
    // `repo_root()` is the bpk-lib Cargo workspace root; machete walks
    // Cargo.toml from that root.
    let mut cmd = Command::new("cargo-machete");
    cmd.current_dir(root).arg("--skip-target-dir");
    run(cmd).context("cargo-machete")
}

fn is_machete_installed() -> bool {
    Command::new("cargo-machete")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_hint_path_is_advisory_not_panic() {
        // The is_machete_installed predicate must never panic — if
        // cargo-machete is missing, it returns false.
        let _ = is_machete_installed();
    }
}
