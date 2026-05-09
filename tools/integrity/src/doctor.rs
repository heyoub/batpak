use crate::repo_surface::{check_command, command_exists, ensure, repo_root};
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

pub(crate) fn run(strict: bool) -> Result<()> {
    let repo_root = repo_root()?;
    let canonical_files = [
        repo_root.join(".gitattributes"),
        repo_root.join(".devcontainer/devcontainer.json"),
        repo_root.join("tools/integrity/Cargo.toml"),
        repo_root.join("traceability/requirements.yaml"),
    ];
    for path in canonical_files {
        ensure(
            path.exists(),
            format!("missing canonical file {}", path.display()),
        )?;
    }

    check_command("git", &["--version"])?;
    check_command("rustc", &["--version"])?;
    check_command("cargo", &["--version"])?;
    check_command("cargo", &["fmt", "--version"])?;
    check_command("cargo", &["clippy", "--version"])?;
    check_command("cargo", &["deny", "--version"])?;
    check_command("cargo", &["audit", "--version"])?;
    check_command("cargo", &["nextest", "--version"])?;
    check_command("cargo", &["llvm-cov", "--version"])?;
    check_command("cargo", &["mutants", "--version"])?;

    let in_container = Path::new("/.dockerenv").exists() || std::env::var("DEVCONTAINER").is_ok();
    if strict && !in_container {
        let has_container_runtime =
            command_exists("docker", &["--version"]) || command_exists("podman", &["--version"]);
        let host_ok = if cfg!(windows) {
            command_exists("cl", &[])
                || command_exists(
                    "cmd",
                    &["/C", "where cl >NUL 2>NUL || where link >NUL 2>NUL"],
                )
        } else {
            command_exists("clang", &["--version"]) || command_exists("cc", &["--version"])
        };
        ensure(
            has_container_runtime || host_ok,
            "strict doctor requires either a container runtime or a validated native toolchain",
        )?;
    }

    let git_attrs =
        fs::read_to_string(repo_root.join(".gitattributes")).context("read .gitattributes")?;
    ensure(
        git_attrs.contains("eol=lf"),
        ".gitattributes must normalize line endings",
    )?;

    // Filesystem fsync probe — gives users an honest expectation of durable
    // throughput before they wonder why their numbers vary across machines.
    // Skipped in non-strict mode to keep CI fast; only the strict path runs it.
    if strict {
        fsync_probe(&repo_root)?;
    }

    println!("doctor: ok");
    Ok(())
}

/// Measure the local filesystem's effective fsync rate by writing N small
/// files and timing the per-file `sync_all` cost. Prints the median fsync
/// latency and the implied per-event durable throughput. This is informational
/// only — it never fails the doctor command.
///
/// Why this exists: `durable_write_throughput` benchmarks vary by 20-200x
/// depending on whether you're on bare-metal NVMe (5K-50K fsyncs/sec) or a
/// virtualized devcontainer (~250 fsyncs/sec). Without this probe, users
/// see weird numbers and assume the writer is slow. With it, they see the
/// physical limit of their disk and can interpret bench results correctly.
fn fsync_probe(repo_root: &Path) -> Result<()> {
    use std::fs::File;
    use std::io::Write;
    use std::time::Instant;

    let probe_dir = repo_root.join("target").join(".fsync-probe");
    fs::create_dir_all(&probe_dir).context("create fsync probe dir")?;

    const PROBE_COUNT: usize = 16;
    let mut samples_us: Vec<u128> = Vec::with_capacity(PROBE_COUNT);

    for i in 0..PROBE_COUNT {
        let path = probe_dir.join(format!("probe_{i}.bin"));
        let start = Instant::now();
        {
            let mut f = File::create(&path).context("create probe file")?;
            f.write_all(&[0xab; 64]).context("write probe file")?;
            f.sync_all().context("sync probe file")?;
        }
        samples_us.push(start.elapsed().as_micros());
    }

    // Best-effort cleanup; not fatal.
    let _ = fs::remove_dir_all(&probe_dir);

    samples_us.sort_unstable();
    let median_us = samples_us[PROBE_COUNT / 2];
    let median_ms = median_us as f64 / 1000.0;
    let fsyncs_per_sec = if median_us == 0 {
        f64::INFINITY
    } else {
        1_000_000.0 / median_us as f64
    };

    println!("fsync probe: median {median_ms:.2} ms/fsync ({fsyncs_per_sec:.0} fsyncs/sec)");
    println!(
        "  → expected single-event durable throughput: ~{fsyncs_per_sec:.0} events/sec\n  \
           (configure batch.group_commit_max_batch > 1 or use append_batch for higher throughput)"
    );

    let environment_hint = if fsyncs_per_sec < 1_000.0 {
        Some("slow fsync — likely virtualized FS, devcontainer, or remote mount")
    } else if fsyncs_per_sec < 5_000.0 {
        Some("moderate fsync — likely consumer SSD or aging NVMe")
    } else {
        None
    };
    if let Some(hint) = environment_hint {
        println!("  hint: {hint}");
    }

    Ok(())
}
