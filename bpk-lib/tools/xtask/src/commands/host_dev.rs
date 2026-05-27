//! `cargo xtask host-dev` — prove the host profile in one motion:
//! manifest → TS tool build → codegen → TS build/test → hbat boot → heartbeat-spike → determinism.

use super::export_ts_manifest;
use crate::util::{cargo, cargo_target_dir, project_root, run, run_output};
use crate::{ExportTsManifestArgs, HostDevArgs};
use anyhow::{bail, Context, Result};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

const READY_PREFIX: &str = "HBAT_READY ";
const HBAT_READY_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) fn host_dev(args: &HostDevArgs) -> Result<()> {
    let project = project_root()?;
    let bpk_ts = project.join("bpk-ts");
    let manifest_out = bpk_ts.join("batpak.manifest.json");

    println!("host-dev: build hbat + xtask");
    cargo(["build", "-p", "hbat", "-p", "xtask"])?;

    println!("host-dev: pnpm install --frozen-lockfile");
    run_pnpm(&bpk_ts, &["install", "--frozen-lockfile"])?;

    println!("host-dev: export-ts-manifest");
    export_ts_manifest::export_ts_manifest(&ExportTsManifestArgs {
        out: manifest_out.clone(),
        check: false,
    })?;

    println!("host-dev: pnpm -w build (tool bootstrap)");
    run_pnpm(&bpk_ts, &["-w", "build"])?;

    println!("host-dev: codegen generate");
    run_pnpm(&bpk_ts, &["--filter", "@batpak/codegen", "run", "generate"])?;

    println!("host-dev: pnpm -w build");
    run_pnpm(&bpk_ts, &["-w", "build"])?;

    if !args.skip_tests {
        println!("host-dev: pnpm -w test");
        run_pnpm(&bpk_ts, &["-w", "test"])?;
    }

    println!("host-dev: live heartbeat-spike");
    run_live_spike(&bpk_ts)?;

    if !args.skip_determinism {
        println!("host-dev: determinism check");
        check_determinism(&project, &bpk_ts, &manifest_out)?;
    }

    println!("host-dev: ok");
    Ok(())
}

fn run_pnpm(cwd: &Path, args: &[&str]) -> Result<()> {
    let program = resolve_pnpm_program()?;
    let mut command = Command::new(&program);
    command.current_dir(cwd).args(args);
    run(command).with_context(|| format!("pnpm {} (cwd {})", args.join(" "), cwd.display()))
}

fn resolve_pnpm_program() -> Result<PathBuf> {
    if command_succeeds("pnpm", &["--version"]) {
        return Ok(PathBuf::from("pnpm"));
    }

    if let Ok(pnpm_home) = std::env::var("PNPM_HOME") {
        let candidate =
            PathBuf::from(&pnpm_home).join(if cfg!(windows) { "pnpm.cmd" } else { "pnpm" });
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    if cfg!(windows) {
        let output = Command::new("where.exe")
            .arg("pnpm")
            .output()
            .context("locate pnpm via where.exe")?;
        if output.status.success() {
            if let Some(line) = String::from_utf8_lossy(&output.stdout).lines().next() {
                let path = line.trim();
                if !path.is_empty() {
                    return Ok(PathBuf::from(path));
                }
            }
        }
    }

    bail!("pnpm not found; install pnpm or set PNPM_HOME on PATH")
}

fn run_node(cwd: &Path, args: &[&str]) -> Result<()> {
    let program = resolve_node_program()?;
    let mut command = Command::new(&program);
    command.current_dir(cwd).args(args);
    run(command)
}

fn resolve_node_program() -> Result<PathBuf> {
    if command_succeeds("node", &["--version"]) {
        return Ok(PathBuf::from("node"));
    }

    if cfg!(windows) {
        let output = Command::new("where.exe")
            .arg("node")
            .output()
            .context("locate node via where.exe")?;
        if output.status.success() {
            if let Some(line) = String::from_utf8_lossy(&output.stdout).lines().next() {
                let path = line.trim();
                if !path.is_empty() {
                    return Ok(PathBuf::from(path));
                }
            }
        }
    }

    bail!("node not found on PATH")
}

fn command_succeeds(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn hbat_binary() -> Result<PathBuf> {
    let name = if cfg!(windows) { "hbat.exe" } else { "hbat" };
    let path = cargo_target_dir()?.join("debug").join(name);
    if !path.exists() {
        bail!(
            "hbat binary missing at {}; run `cargo build -p hbat` first",
            path.display()
        );
    }
    Ok(path)
}

struct HbatProcess {
    child: Child,
    port: u16,
    _store: TempDir,
}

impl HbatProcess {
    fn spawn() -> Result<Self> {
        let host_dev_root = cargo_target_dir()?.join("host-dev");
        fs::create_dir_all(&host_dev_root).context("create host-dev target dir")?;
        let store = TempDir::new_in(&host_dev_root).context("create ephemeral store dir")?;
        let hbat = hbat_binary()?;

        let mut child = Command::new(&hbat)
            .arg("serve")
            .arg("--store")
            .arg(store.path())
            .arg("--tcp")
            .arg("127.0.0.1:0")
            .arg("--print-port")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawn {}", hbat.display()))?;

        let ready_line = read_hbat_ready_line(&mut child)?;

        let payload = ready_line.trim_start_matches(READY_PREFIX).trim();
        let parsed: serde_json::Value =
            serde_json::from_str(payload).context("HBAT_READY payload is JSON")?;
        let port_u64 = parsed
            .get("port")
            .and_then(serde_json::Value::as_u64)
            .context("HBAT_READY carries a numeric port")?;
        let port = u16::try_from(port_u64).context("HBAT_READY port fits in u16")?;

        Ok(Self {
            child,
            port,
            _store: store,
        })
    }
}

enum ReadyReadResult {
    Ready(String),
    Eof,
    Error(std::io::Error),
}

fn read_hbat_ready_line(child: &mut Child) -> Result<String> {
    let stdout = child.stdout.take().context("pipe hbat stdout")?;
    let last_line = Arc::new(Mutex::new(String::new()));
    let thread_last_line = Arc::clone(&last_line);
    let (tx, rx) = mpsc::channel();

    thread::Builder::new()
        .name("host-dev-hbat-ready-reader".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        let _ = tx.send(ReadyReadResult::Eof);
                        return;
                    }
                    Ok(_) => {
                        if let Ok(mut last) = thread_last_line.lock() {
                            last.clear();
                            last.push_str(&line);
                        }
                        if line.starts_with(READY_PREFIX) {
                            let _ = tx.send(ReadyReadResult::Ready(line.clone()));
                            return;
                        }
                    }
                    Err(error) => {
                        let _ = tx.send(ReadyReadResult::Error(error));
                        return;
                    }
                }
            }
        })
        .context("spawn hbat ready reader thread")?;

    match rx.recv_timeout(HBAT_READY_TIMEOUT) {
        Ok(ReadyReadResult::Ready(line)) => Ok(line),
        Ok(ReadyReadResult::Eof) => {
            let _ = child.kill();
            let stderr = read_child_stderr(child);
            bail!("hbat closed stdout before HBAT_READY\nstderr:\n{stderr}");
        }
        Ok(ReadyReadResult::Error(error)) => {
            let _ = child.kill();
            Err(error).context("read HBAT_READY")
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let _ = child.kill();
            let ready_line = last_line
                .lock()
                .map(|line| line.clone())
                .unwrap_or_default();
            let stderr = read_child_stderr(child);
            bail!(
                "timed out waiting for HBAT_READY (read so far: {ready_line:?})\nstderr:\n{stderr}"
            );
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = child.kill();
            let stderr = read_child_stderr(child);
            bail!("hbat readiness reader exited before HBAT_READY\nstderr:\n{stderr}");
        }
    }
}

impl Drop for HbatProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_child_stderr(child: &mut Child) -> String {
    child
        .stderr
        .take()
        .map(|stderr| {
            let mut buf = String::new();
            let _ = BufReader::new(stderr).read_to_string(&mut buf);
            buf
        })
        .unwrap_or_default()
}

fn run_live_spike(bpk_ts: &Path) -> Result<()> {
    let process = HbatProcess::spawn()?;
    let port = process.port;
    let spike = bpk_ts.join("examples/heartbeat-spike/dist/index.js");
    if !spike.exists() {
        bail!(
            "heartbeat-spike dist missing at {}; run pnpm -w build first",
            spike.display()
        );
    }
    run_node(
        bpk_ts,
        &[
            "examples/heartbeat-spike/dist/index.js",
            "--port",
            &port.to_string(),
        ],
    )
    .with_context(|| format!("heartbeat-spike on port {port}"))?;
    Ok(())
}

fn check_determinism(project: &Path, bpk_ts: &Path, manifest_out: &Path) -> Result<()> {
    let generated_src = bpk_ts.join("packages/generated/src");
    if generated_src.exists() {
        fs::remove_dir_all(&generated_src)
            .with_context(|| format!("remove {}", generated_src.display()))?;
    }

    export_ts_manifest::export_ts_manifest(&ExportTsManifestArgs {
        out: manifest_out.to_path_buf(),
        check: false,
    })?;

    run_pnpm(bpk_ts, &["--filter", "@batpak/codegen", "run", "generate"])?;

    let mut command = Command::new("git");
    command
        .current_dir(project)
        .arg("diff")
        .arg("--exit-code")
        .arg("--")
        .arg("bpk-ts/batpak.manifest.json")
        .arg("bpk-ts/packages/generated/src");
    run_output(command)
        .map(|_| ())
        .context("codegen non-deterministic; see git diff above")?;

    let mut status = Command::new("git");
    status
        .current_dir(project)
        .arg("status")
        .arg("--porcelain")
        .arg("--")
        .arg("bpk-ts/batpak.manifest.json")
        .arg("bpk-ts/packages/generated/src");
    let output = run_output(status).context("inspect generated file status")?;
    if !output.stdout.is_empty() {
        let status = String::from_utf8_lossy(&output.stdout);
        bail!("codegen produced untracked or staged generated files:\n{status}");
    }

    Ok(())
}
