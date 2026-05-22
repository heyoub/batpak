use crate::util::{project_root, run};
use anyhow::{Context, Result};
use std::process::Command;

pub(crate) fn ast_grep() -> Result<()> {
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
        "run ast-grep calipers; install `sg` via `npm install -g @ast-grep/cli` or `cargo install ast-grep --locked`"
    })
}
