//! The bpk-ts (TypeScript) half of the polyglot monorepo gate surface.
//!
//! `cargo xtask verify-ts` mirrors the Rust `preflight` for the bpk-ts package
//! set so whole-repo validation is one command (`just verify-all`) instead of a
//! hand-run Rust + pnpm combo. The justfile-stays-thin contract
//! (`tools/integrity/.../tooling_contract.rs`) requires real tooling logic to
//! live here in xtask, not as raw recipe lines — so the polyglot driver is an
//! xtask subcommand the justfile only forwards to.

use crate::util::{project_root, run};
use anyhow::{bail, Context, Result};
use std::process::Command;

/// Run the bpk-ts package gates in dependency order: frozen-lockfile install,
/// the workspace build (the tsc compile), lint, format check, and tests.
pub(crate) fn verify_ts() -> Result<()> {
    let bpk_ts = project_root()?.join("bpk-ts");
    if !bpk_ts.join("package.json").exists() {
        bail!(
            "verify-ts: {} has no package.json; expected the bpk-ts workspace at the repo root",
            bpk_ts.display()
        );
    }
    let steps: [&[&str]; 5] = [
        &["install", "--frozen-lockfile"],
        &["-r", "run", "build"],
        &["run", "lint"],
        &["run", "format:check"],
        &["-r", "run", "test"],
    ];
    for args in steps {
        let mut command = Command::new("pnpm");
        command.current_dir(&bpk_ts).args(args);
        run(command).with_context(|| format!("bpk-ts gate failed: pnpm {}", args.join(" ")))?;
    }
    Ok(())
}
