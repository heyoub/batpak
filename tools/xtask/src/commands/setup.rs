use super::{HookStatus, InstallStrategy, PRE_COMMIT_HOOK, REPO_HOOKS_PATH};
use crate::util::{cargo, command_succeeds, repo_root, run};
use crate::SetupArgs;
use anyhow::{bail, Context, Result};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

pub(crate) fn setup(args: SetupArgs) -> Result<()> {
    let required = [
        (
            "cargo-nextest",
            "cargo-nextest@0.9.132",
            InstallStrategy::PreferBinstall,
        ),
        (
            "cargo-deny",
            "cargo-deny@0.19.0",
            InstallStrategy::PreferBinstall,
        ),
        (
            "cargo-audit",
            "cargo-audit@0.22.1",
            InstallStrategy::PreferBinstall,
        ),
        (
            "cargo-llvm-cov",
            "cargo-llvm-cov@0.8.5",
            InstallStrategy::PreferBinstall,
        ),
        (
            "cargo-mutants",
            "cargo-mutants@27.0.0",
            InstallStrategy::SourceOnly,
        ),
    ];

    let mut missing = Vec::new();
    for (tool, _, _) in required {
        if !command_succeeds(tool, ["--version"]) {
            missing.push(tool);
        }
    }

    if missing.is_empty() {
        println!("All developer tools are installed.");
    } else if args.install_tools {
        if required.iter().any(|(tool, _, strategy)| {
            missing.contains(tool) && *strategy == InstallStrategy::PreferBinstall
        }) {
            ensure_binstall_helper()?;
        }
        for (tool, spec, strategy) in required {
            if missing.contains(&tool) {
                install_tool(spec, strategy)?;
            }
        }
    } else {
        println!("Missing tools: {}", missing.join(", "));
        println!("Run `cargo xtask setup --install-tools` to install the standard toolchain.");
    }

    if cfg!(windows) {
        println!("Native Windows detected. `cargo xtask doctor` will validate the host toolchain.");
    } else {
        println!("Use the checked-in devcontainer for the canonical environment.");
    }
    let hook_status = if args.install_tools {
        maybe_install_repo_hooks().map(|status| (status, true))
    } else {
        repo_hook_status().map(|status| (status, false))
    };
    match hook_status {
        Ok((status, attempted_install)) => report_hook_install_result(status, attempted_install),
        Err(err) => eprintln!("setup: warning: could not inspect/install repo hooks: {err:#}"),
    }
    Ok(())
}

pub(crate) fn install_hooks() -> Result<()> {
    report_hook_install_result(maybe_install_repo_hooks()?, true);
    Ok(())
}

pub(crate) fn doctor() -> Result<()> {
    super::integrity("doctor", ["--strict"])?;
    match repo_hook_status() {
        Ok(HookStatus::Installed) => {}
        Ok(HookStatus::Default) => {
            eprintln!(
                "doctor: warning: repo-managed hooks are not installed. Run `cargo xtask install-hooks` to wire `.githooks/pre-commit`."
            );
        }
        Ok(HookStatus::Custom(path)) => {
            eprintln!(
                "doctor: warning: custom git hooks path `{path}` is active, so `.githooks/pre-commit` is not managing pre-commit checks. Clear or change `core.hooksPath`, then run `cargo xtask install-hooks` if you want the repo hook surface."
            );
        }
        Err(err) => {
            eprintln!("doctor: warning: could not inspect git hooks path: {err:#}");
        }
    }
    Ok(())
}

fn maybe_install_repo_hooks() -> Result<HookStatus> {
    let root = repo_root()?;
    let hook = root.join(PRE_COMMIT_HOOK);
    if !hook.exists() {
        bail!(
            "repo hook surface is missing `{}`; restore the tracked hook before installing",
            hook.display()
        );
    }

    match repo_hook_status()? {
        HookStatus::Installed => Ok(HookStatus::Installed),
        HookStatus::Custom(path) => Ok(HookStatus::Custom(path)),
        HookStatus::Default => {
            let mut command = Command::new("git");
            command
                .current_dir(&root)
                .args(["config", "core.hooksPath", REPO_HOOKS_PATH]);
            run(command)?;
            Ok(HookStatus::Installed)
        }
    }
}

fn repo_hook_status() -> Result<HookStatus> {
    let root = repo_root()?;
    let output = Command::new("git")
        .current_dir(&root)
        .args(["config", "--get", "core.hooksPath"])
        .output()
        .context("inspect git core.hooksPath")?;

    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if path.is_empty() || is_default_hooks_path(&root, &path) {
            return Ok(HookStatus::Default);
        }
        if matches_repo_hooks_path(&root, &path) {
            let hook = root.join(PRE_COMMIT_HOOK);
            if hook.exists() {
                return Ok(HookStatus::Installed);
            }
            return Ok(HookStatus::Custom(path));
        }
        return Ok(HookStatus::Custom(path));
    }

    if output.status.code() == Some(1) {
        return Ok(HookStatus::Default);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!(
        "git config --get core.hooksPath failed with status {}: {}",
        output.status,
        stderr.trim()
    )
}

pub(super) fn is_default_hooks_path(root: &Path, configured_path: &str) -> bool {
    configured_path == ".git/hooks"
        || resolved_git_path(root, configured_path)
            == normalize_path(&root.join(".git").join("hooks"))
}

pub(super) fn matches_repo_hooks_path(root: &Path, configured_path: &str) -> bool {
    resolved_git_path(root, configured_path) == normalize_path(&root.join(REPO_HOOKS_PATH))
}

fn resolved_git_path(root: &Path, configured_path: &str) -> PathBuf {
    let path = Path::new(configured_path);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    normalize_path(&resolved)
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

fn report_hook_install_result(status: HookStatus, attempted_install: bool) {
    match status {
        HookStatus::Installed if attempted_install => {
            println!("Repo hooks are installed at `{REPO_HOOKS_PATH}`.");
        }
        HookStatus::Installed => {
            println!("Repo hooks are already installed at `{REPO_HOOKS_PATH}`.");
        }
        HookStatus::Default => {
            println!(
                "Repo hooks are not installed. Run `cargo xtask install-hooks` to wire `.githooks/pre-commit`."
            );
        }
        HookStatus::Custom(path) => {
            println!(
                "Custom git hooks path `{path}` is active; leaving it unchanged. To opt into the repo-managed hook surface, set `git config core.hooksPath {REPO_HOOKS_PATH}` or clear the custom path first, then run `cargo xtask install-hooks`."
            );
        }
    }
}

fn ensure_binstall_helper() -> Result<()> {
    if command_succeeds("cargo", ["binstall", "--version"]) {
        return Ok(());
    }

    let mut install = Command::new("cargo");
    install.args(["install", "cargo-binstall"]);
    run(install)
}

fn install_tool(spec: &str, strategy: InstallStrategy) -> Result<()> {
    if strategy == InstallStrategy::PreferBinstall
        && command_succeeds("cargo", ["binstall", "--version"])
    {
        let mut binstall = Command::new("cargo");
        binstall.args(["binstall", "--no-confirm", spec]);
        if run(binstall).is_ok() {
            return Ok(());
        }
        eprintln!("binstall fallback: `{spec}` binary install failed; retrying with cargo install");
    }

    cargo(["install", "--locked", spec])
}
