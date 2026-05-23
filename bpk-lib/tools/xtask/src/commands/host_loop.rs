//! `cargo xtask host-loop` — prove the living TS loop: seed audit-loop, restart
//! hbat on the same store, replay-only rebuild from substrate truth.

use crate::util::{cargo, cargo_target_dir, project_root, run};
use anyhow::{bail, Context, Result};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const READY_PREFIX: &str = "HBAT_READY ";
const STREAM_BEGIN: &str = "audit-loop: stream begin";
const STREAM_END: &str = "audit-loop: stream end";

pub(crate) fn host_loop() -> Result<()> {
    let project = project_root()?;
    let bpk_ts = project.join("bpk-ts");
    let store_dir = cargo_target_dir()?.join("host-loop").join("store");
    if store_dir.exists() {
        fs::remove_dir_all(&store_dir).context("reset host-loop store dir")?;
    }
    fs::create_dir_all(&store_dir).context("create host-loop store dir")?;

    println!("host-loop: build hbat");
    cargo(["build", "-p", "hbat"])?;

    println!("host-loop: pnpm -w build");
    run_pnpm(&bpk_ts, &["-w", "build"])?;

    let audit_loop = bpk_ts.join("examples/audit-loop/dist/index.js");
    if !audit_loop.exists() {
        bail!(
            "audit-loop dist missing at {}; run pnpm -w build first",
            audit_loop.display()
        );
    }

    println!("host-loop: seed + replay on {}", store_dir.display());
    let seed_output = run_audit_loop(&bpk_ts, &store_dir, &[])?;
    let seed_stream = extract_stream_lines(&seed_output)?;

    println!("host-loop: restart hbat and replay-only");
    let replay_output = run_audit_loop(&bpk_ts, &store_dir, &["--replay-only"])?;
    let replay_stream = extract_stream_lines(&replay_output)?;

    if seed_stream != replay_stream {
        bail!(
            "replay-only stream differed from seed run\nseed:\n{}\nreplay:\n{}",
            seed_stream.join("\n"),
            replay_stream.join("\n")
        );
    }

    println!("host-loop: ok");
    Ok(())
}

fn run_audit_loop(bpk_ts: &Path, store_dir: &Path, extra_args: &[&str]) -> Result<String> {
    let process = HbatProcess::spawn(store_dir)?;
    let port = process.port;
    let port_string = port.to_string();

    let mut args = vec![
        "examples/audit-loop/dist/index.js",
        "--port",
        port_string.as_str(),
    ];
    args.extend(extra_args);

    let output = run_node_capture(bpk_ts, &args)
        .with_context(|| format!("audit-loop on port {port} args={}", extra_args.join(" ")))?;
    drop(process);

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "audit-loop failed with status {}:\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

struct HbatProcess {
    child: Child,
    port: u16,
}

impl HbatProcess {
    fn spawn(store_dir: &Path) -> Result<Self> {
        let hbat = hbat_binary()?;
        let mut child = Command::new(&hbat)
            .arg("serve")
            .arg("--store")
            .arg(store_dir)
            .arg("--tcp")
            .arg("127.0.0.1:0")
            .arg("--print-port")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawn {}", hbat.display()))?;

        let stdout = child.stdout.take().context("pipe hbat stdout")?;
        let mut reader = BufReader::new(stdout);
        let mut ready_line = String::new();
        let deadline = Instant::now() + Duration::from_secs(10);

        loop {
            if Instant::now() > deadline {
                let stderr = read_child_stderr(&mut child);
                let _ = child.kill();
                bail!(
                    "timed out waiting for HBAT_READY (read so far: {ready_line:?})\nstderr:\n{stderr}"
                );
            }
            ready_line.clear();
            match reader.read_line(&mut ready_line) {
                Ok(0) => {
                    let stderr = read_child_stderr(&mut child);
                    let _ = child.kill();
                    bail!("hbat closed stdout before HBAT_READY\nstderr:\n{stderr}");
                }
                Ok(_) => {
                    if ready_line.starts_with(READY_PREFIX) {
                        break;
                    }
                }
                Err(error) => {
                    let _ = child.kill();
                    return Err(error).context("read HBAT_READY");
                }
            }
        }

        let payload = ready_line.trim_start_matches(READY_PREFIX).trim();
        let parsed: serde_json::Value =
            serde_json::from_str(payload).context("HBAT_READY payload is JSON")?;
        let port_u64 = parsed
            .get("port")
            .and_then(serde_json::Value::as_u64)
            .context("HBAT_READY carries a numeric port")?;
        let port = u16::try_from(port_u64).context("HBAT_READY port fits in u16")?;

        Ok(Self { child, port })
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

fn extract_stream_lines(output: &str) -> Result<Vec<String>> {
    let mut in_stream = false;
    let mut lines = Vec::new();
    for line in output.lines() {
        if line == STREAM_BEGIN {
            in_stream = true;
            continue;
        }
        if line == STREAM_END {
            break;
        }
        if in_stream {
            lines.push(line.to_string());
        }
    }
    if lines.is_empty() {
        bail!("audit-loop output missing stream section");
    }
    Ok(lines)
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

fn run_pnpm(cwd: &Path, args: &[&str]) -> Result<()> {
    let program = resolve_pnpm_program()?;
    let mut command = Command::new(&program);
    command.current_dir(cwd).args(args);
    run(command).with_context(|| format!("pnpm {} (cwd {})", args.join(" "), cwd.display()))
}

fn run_node_capture(cwd: &Path, args: &[&str]) -> Result<Output> {
    let program = resolve_node_program()?;
    let output = Command::new(&program)
        .current_dir(cwd)
        .args(args)
        .output()
        .with_context(|| format!("node {} (cwd {})", args.join(" "), cwd.display()))?;
    Ok(output)
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
