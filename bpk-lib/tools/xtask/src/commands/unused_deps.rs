//! `xtask unused-deps` — detect dependencies declared in any
//! workspace crate's `Cargo.toml` that are never referenced from source.
//!
//! Backed by `cargo-machete`. Each unused dep widens the supply-chain
//! blast radius and slows builds; this gate keeps the dep set tight.
//!
//! `cargo-machete` is a separate install. If it's not on PATH, this
//! command emits a clear install hint and exits non-zero (advisory).
//! Consulting clients run the gate in clean containers and want
//! deterministic tool versioning — no auto-install.

use anyhow::{anyhow, Context, Result};
use std::process::Command;

use crate::util::{repo_root, run};

/// Run `cargo machete` over the workspace.
pub(crate) fn unused_deps() -> Result<()> {
    if !is_machete_installed() {
        eprintln!(
            "xtask unused-deps: cargo-machete is not installed.\n\
             Install with: cargo install cargo-machete --locked"
        );
        return Err(anyhow!("cargo-machete not on PATH"));
    }

    let root = repo_root()?;
    // bpk-lib is the Rust workspace; machete walks Cargo.toml from
    // that root.
    let mut cmd = Command::new("cargo");
    cmd.current_dir(root.join("bpk-lib"))
        .args(["machete", "--skip-target-dir"]);
    run(cmd).context("cargo machete")
}

fn is_machete_installed() -> bool {
    Command::new("cargo")
        .args(["machete", "--version"])
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
        // cargo or cargo-machete is missing, it returns false.
        let _ = is_machete_installed();
    }
}
