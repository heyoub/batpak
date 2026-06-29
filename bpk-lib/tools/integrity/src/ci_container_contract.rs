//! INV-CANONICAL-CONTAINER-CI: Linux CI proof lanes run through the checked-in
//! devcontainer image and repo-owned wrapper, not a hand-installed host shell.

use crate::repo_surface::{ensure, project_root};
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn check(repo_root: &Path) -> Result<BTreeSet<PathBuf>> {
    let project_root = project_root(repo_root);
    let ci_yml = project_root.join(".github/workflows/ci.yml");
    let setup_action = project_root.join(".github/actions/setup-devcontainer/action.yml");
    let devcontainer_rs = repo_root.join("tools/xtask/src/devcontainer.rs");

    let mut inputs = BTreeSet::new();
    inputs.insert(ci_yml.clone());
    inputs.insert(setup_action.clone());
    inputs.insert(devcontainer_rs.clone());

    let ci = fs::read_to_string(&ci_yml).context("read .github/workflows/ci.yml")?;
    let setup = fs::read_to_string(&setup_action)
        .context("read .github/actions/setup-devcontainer/action.yml")?;
    let devcontainer =
        fs::read_to_string(&devcontainer_rs).context("read tools/xtask/src/devcontainer.rs")?;

    check_ci_workflow(&ci)?;
    check_setup_action(&setup)?;
    check_xtask_devcontainer(&devcontainer)?;
    Ok(inputs)
}

fn check_ci_workflow(ci: &str) -> Result<()> {
    ensure(
        ci.contains("ci-fast-linux:")
            && ci.contains("name: CI fast (ubuntu-devcontainer)")
            && ci.contains("uses: ./.github/actions/setup-devcontainer")
            && ci.contains("run: bash ./scripts/run-in-devcontainer.sh 'cargo xtask ci-fast'"),
        "canonical-container-ci (INV-CANONICAL-CONTAINER-CI): ci-fast-linux must build the \
         checked-in devcontainer action and run `cargo xtask ci-fast` through scripts/run-in-devcontainer.sh",
    )?;
    ensure(
        ci.contains("verify-linux:")
            && ci.contains("name: Verify (ubuntu-devcontainer)")
            && ci.contains("run: bash ./scripts/run-in-devcontainer.sh 'cargo xtask preflight'"),
        "canonical-container-ci (INV-CANONICAL-CONTAINER-CI): verify-linux must run preflight \
         through scripts/run-in-devcontainer.sh",
    )?;
    ensure(
        ci.contains("BATPAK_DEVCONTAINER_SKIP_BUILD: \"1\"")
            && ci.contains("BATPAK_DEVCONTAINER_IMAGE: batpak-devcontainer:ci"),
        "canonical-container-ci (INV-CANONICAL-CONTAINER-CI): Linux proof jobs must consume \
         the setup action's batpak-devcontainer:ci image instead of rebuilding or using host cargo",
    )
}

fn check_setup_action(action: &str) -> Result<()> {
    ensure(
        action.contains("docker/build-push-action@")
            && action.contains("file: ${{ github.workspace }}/.devcontainer/Dockerfile")
            && action.contains("tags: batpak-devcontainer:ci")
            && action.contains("load: true"),
        "canonical-container-ci (INV-CANONICAL-CONTAINER-CI): setup-devcontainer action must \
         build and load the checked-in .devcontainer/Dockerfile as batpak-devcontainer:ci",
    )
}

fn check_xtask_devcontainer(devcontainer: &str) -> Result<()> {
    ensure(
        devcontainer.contains(".arg(\"DEVCONTAINER=1\")")
            && devcontainer.contains("WORKSPACE_LIB_DIR")
            && devcontainer.contains("repo_root.join(\".devcontainer\").join(\"Dockerfile\")"),
        "canonical-container-ci (INV-CANONICAL-CONTAINER-CI): xtask devcontainer runner must \
         mark in-container execution, mount bpk-lib as cwd, and hash the checked-in Dockerfile",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const GREEN_CI: &str = r#"
jobs:
  ci-fast-linux:
    name: CI fast (ubuntu-devcontainer)
    steps:
      - uses: ./.github/actions/setup-devcontainer
      - name: Fast PR signal
        env:
          BATPAK_DEVCONTAINER_IMAGE: batpak-devcontainer:ci
          BATPAK_DEVCONTAINER_SKIP_BUILD: "1"
        run: bash ./scripts/run-in-devcontainer.sh 'cargo xtask ci-fast'
  verify-linux:
    name: Verify (ubuntu-devcontainer)
    steps:
      - uses: ./.github/actions/setup-devcontainer
      - name: Canonical preflight proof bundle
        env:
          BATPAK_DEVCONTAINER_IMAGE: batpak-devcontainer:ci
          BATPAK_DEVCONTAINER_SKIP_BUILD: "1"
        run: bash ./scripts/run-in-devcontainer.sh 'cargo xtask preflight'
"#;

    const GREEN_ACTION: &str = r#"
runs:
  using: composite
  steps:
    - uses: docker/build-push-action@pin
      with:
        file: ${{ github.workspace }}/.devcontainer/Dockerfile
        tags: batpak-devcontainer:ci
        load: true
"#;

    const GREEN_XTASK: &str = r#"
fn exec() {
    command.arg("-e").arg("DEVCONTAINER=1");
    command.arg(WORKSPACE_LIB_DIR);
}
fn dockerfile(repo_root: &Path) -> PathBuf {
    repo_root.join(".devcontainer").join("Dockerfile")
}
"#;

    #[test]
    fn canonical_container_contract_rejects_host_cargo_fast_lane() {
        let red = GREEN_CI.replace(
            "run: bash ./scripts/run-in-devcontainer.sh 'cargo xtask ci-fast'",
            "working-directory: bpk-lib\n        run: cargo xtask ci-fast",
        );
        assert!(
            check_ci_workflow(&red).is_err(),
            "host cargo fast lane must be rejected"
        );
        assert!(
            check_setup_action("runs:\n  using: composite\n").is_err(),
            "setup action without Dockerfile build must be rejected"
        );
        assert!(
            check_xtask_devcontainer("fn exec() {}\n").is_err(),
            "xtask devcontainer runner without checked-in Dockerfile path must be rejected"
        );
    }

    #[test]
    fn canonical_container_contract_accepts_devcontainer_lane() {
        check_ci_workflow(GREEN_CI).expect("ci workflow runs through devcontainer");
        check_setup_action(GREEN_ACTION).expect("setup action builds checked-in Dockerfile");
        check_xtask_devcontainer(GREEN_XTASK).expect("xtask uses checked-in Dockerfile");
    }
}
