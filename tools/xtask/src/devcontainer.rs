use crate::util::{repo_root, run};
use crate::DevcontainerExecArgs;
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const IMAGE_HASH_LABEL: &str = "io.batpak.devcontainer-hash";
const WORKSPACE_DIR: &str = "/workspace/batpak";
const FORWARDED_ENV_VARS: &[&str] = &[
    "CARGO_TERM_COLOR",
    "PROPTEST_CASES",
    "CHAOS_ITERATIONS",
    "CARGO_INCREMENTAL",
];

pub(crate) fn devcontainer_exec(args: DevcontainerExecArgs) -> Result<()> {
    if args.command.is_empty() {
        bail!("`cargo xtask devcontainer-exec -- <command...>` requires an explicit command");
    }
    exec_in_devcontainer(&args.command)
}

pub(crate) fn run_in_devcontainer(args: &[&str]) -> Result<()> {
    let command = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    exec_in_devcontainer(&command)
}

fn exec_in_devcontainer(args: &[String]) -> Result<()> {
    let root = repo_root()?;
    ensure_devcontainer_image(&root)?;

    let runtime = oci_runtime();
    let image = devcontainer_image();
    let mut command = Command::new(&runtime);
    command
        .arg("run")
        .arg("--rm")
        .arg("-e")
        .arg("DEVCONTAINER=1")
        .arg("-e")
        .arg(format!(
            "CARGO_TERM_COLOR={}",
            std::env::var("CARGO_TERM_COLOR").unwrap_or_else(|_| "always".to_owned())
        ));

    for var in FORWARDED_ENV_VARS
        .iter()
        .filter(|var| **var != "CARGO_TERM_COLOR")
    {
        if let Ok(value) = std::env::var(var) {
            command.arg("-e").arg(format!("{var}={value}"));
        }
    }

    command
        .arg("-v")
        .arg(format!("{}:{WORKSPACE_DIR}", root.display()))
        .arg("-w")
        .arg(WORKSPACE_DIR)
        .arg(&image);
    command.args(inner_command(args));
    run(command)
}

fn ensure_devcontainer_image(repo_root: &Path) -> Result<()> {
    let runtime = oci_runtime();
    let image = devcontainer_image();

    if skip_build() {
        if image_exists(&runtime, &image)? {
            return Ok(());
        }
        bail!(
            "BATPAK_DEVCONTAINER_SKIP_BUILD=1 was set but image `{image}` is not available locally"
        );
    }

    let dockerfile = dockerfile(repo_root);
    let current_hash = dockerfile_hash(&dockerfile)?;
    let existing_hash = inspect_image_hash(&runtime, &image)?;
    if existing_hash.as_deref() == Some(current_hash.as_str()) {
        println!("Reusing local devcontainer image `{image}` (Dockerfile unchanged).");
        return Ok(());
    }

    let mut command = Command::new(&runtime);
    command
        .arg("build")
        .arg("--label")
        .arg(format!("{IMAGE_HASH_LABEL}={current_hash}"))
        .arg("-f")
        .arg(&dockerfile)
        .arg("-t")
        .arg(&image)
        .arg(repo_root);
    run(command)
}

fn inspect_image_hash(runtime: &str, image: &str) -> Result<Option<String>> {
    let output = Command::new(runtime)
        .args([
            "image",
            "inspect",
            image,
            "--format",
            "{{ index .Config.Labels \"io.batpak.devcontainer-hash\" }}",
        ])
        .output()
        .with_context(|| format!("inspect devcontainer image `{image}`"))?;

    if !output.status.success() {
        return Ok(None);
    }

    let hash = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if hash.is_empty() || hash == "<no value>" {
        Ok(None)
    } else {
        Ok(Some(hash))
    }
}

fn image_exists(runtime: &str, image: &str) -> Result<bool> {
    let status = Command::new(runtime)
        .args(["image", "inspect", image])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("inspect devcontainer image `{image}`"))?;
    Ok(status.success())
}

fn dockerfile_hash(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn dockerfile(repo_root: &Path) -> PathBuf {
    repo_root.join(".devcontainer").join("Dockerfile")
}

fn oci_runtime() -> String {
    std::env::var("OCI_RUNTIME").unwrap_or_else(|_| "docker".to_owned())
}

fn devcontainer_image() -> String {
    std::env::var("BATPAK_DEVCONTAINER_IMAGE").unwrap_or_else(|_| "batpak-devcontainer".to_owned())
}

fn skip_build() -> bool {
    matches!(
        std::env::var("BATPAK_DEVCONTAINER_SKIP_BUILD")
            .ok()
            .as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

fn inner_command(args: &[String]) -> Vec<OsString> {
    if args.len() == 1 {
        return vec![
            OsString::from("bash"),
            OsString::from("-lc"),
            OsString::from(&args[0]),
        ];
    }

    args.iter().map(OsString::from).collect()
}

#[cfg(test)]
mod tests {
    use super::inner_command;
    use std::ffi::OsString;

    #[test]
    fn single_string_command_uses_shell_entry_path() {
        let args = vec!["cargo xtask ci".to_owned()];
        assert_eq!(
            inner_command(&args),
            vec![
                OsString::from("bash"),
                OsString::from("-lc"),
                OsString::from("cargo xtask ci"),
            ]
        );
    }

    #[test]
    fn argv_command_stays_argv_command() {
        let args = vec!["cargo".to_owned(), "xtask".to_owned(), "docs".to_owned()];
        assert_eq!(
            inner_command(&args),
            vec![
                OsString::from("cargo"),
                OsString::from("xtask"),
                OsString::from("docs"),
            ]
        );
    }
}
