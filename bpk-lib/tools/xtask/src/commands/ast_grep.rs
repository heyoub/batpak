use crate::util::{project_root, run};
use anyhow::{Context, Result};
use std::process::Command;

/// Store production calipers (audit-only) plus devops/repo sanity calipers on CI surfaces.
pub(crate) fn ast_grep() -> Result<()> {
    ast_grep_store()?;
    ast_grep_devops()?;
    super::ast_grep_family_version::ast_grep_family_version()?;
    Ok(())
}

fn ast_grep_store() -> Result<()> {
    let root = project_root()?;
    let mut command = Command::new("sg");
    command.current_dir(&root).args([
        "scan",
        "--config",
        "sgconfig.yml",
        "--report-style",
        "short",
        "--globs",
        "bpk-lib/crates/core/src/store/**/*.rs",
        "--globs",
        "!bpk-lib/crates/core/src/store/platform/**",
        "--globs",
        "!**/tests.rs",
        "--globs",
        "!**/test_support.rs",
        "--globs",
        "!**/fixtures/**",
    ]);
    run(command).with_context(|| {
        "run store ast-grep calipers; install `sg` via `npm install -g @ast-grep/cli` or `cargo install ast-grep --locked`"
    })
}

fn ast_grep_devops() -> Result<()> {
    let root = project_root()?;
    let mut command = Command::new("sg");
    command.current_dir(&root).args([
        "scan",
        "--config",
        "sgconfig.yml",
        "--report-style",
        "short",
        "--globs",
        ".github/workflows/ci.yml",
        "--globs",
        "justfile",
    ]);
    run(command)
        .with_context(|| "run devops ast-grep calipers on .github/workflows/ci.yml and justfile")
}
