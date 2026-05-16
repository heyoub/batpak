use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

pub(crate) fn cargo<I, S>(args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut command = Command::new("cargo");
    for arg in args {
        command.arg(arg.as_ref());
    }
    run(command)
}

pub(crate) fn run(mut command: Command) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("run {:?}", command))?;
    if status.success() {
        Ok(())
    } else {
        bail!("command failed with status {status}")
    }
}

pub(crate) fn run_output(mut command: Command) -> Result<Output> {
    let output = command
        .output()
        .with_context(|| format!("run {:?}", command))?;
    if output.status.success() {
        Ok(output)
    } else {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "command failed with status {}:\nstdout:\n{}\nstderr:\n{}",
            output.status,
            stdout,
            stderr
        )
    }
}

pub(crate) fn command_succeeds<const N: usize>(program: &str, args: [&str; N]) -> bool {
    Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub(crate) fn repo_root() -> Result<PathBuf> {
    let mut current = std::env::current_dir().context("get cwd")?;
    loop {
        let manifest = current.join("Cargo.toml");
        if manifest.exists() {
            let contents = fs::read_to_string(&manifest).context("read manifest")?;
            let has_repo_markers =
                current.join(".git").exists() || current.join("tools").join("xtask").exists();
            if contents.contains("[workspace]") && has_repo_markers {
                return Ok(current);
            }
        }
        if !current.pop() {
            bail!("could not locate workspace root from cwd");
        }
    }
}

pub(crate) fn project_root() -> Result<PathBuf> {
    let repo_root = repo_root()?;
    Ok(repo_root
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or(repo_root))
}

pub(crate) fn cargo_target_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CARGO_TARGET_DIR") {
        return Ok(PathBuf::from(path));
    }
    Ok(project_root()?.join("target"))
}

pub(crate) fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let entry_path = entry.path();
        let dest_path = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry_path, &dest_path)?;
        } else {
            fs::copy(&entry_path, &dest_path)?;
        }
    }
    Ok(())
}

pub(crate) fn open_in_browser(path: PathBuf) -> Result<()> {
    if cfg!(target_os = "windows") {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", "", path.to_string_lossy().as_ref()]);
        run(command)
    } else if cfg!(target_os = "macos") {
        let mut command = Command::new("open");
        command.arg(path);
        run(command)
    } else {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        run(command)
    }
}
