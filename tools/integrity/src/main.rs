use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use regex::Regex;
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use syn::Item;
use walkdir::WalkDir;

#[derive(Parser)]
#[command(author, version, about = "Executable integrity checks for batpak")]
struct Cli {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Subcommand)]
enum CommandKind {
    Doctor {
        #[arg(long)]
        strict: bool,
    },
    TraceabilityCheck,
    StructuralCheck,
}

#[derive(Debug, Deserialize)]
struct RequirementRecord {
    id: String,
    summary: String,
    artifacts: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct InvariantRecord {
    id: String,
    statement: String,
    artifacts: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct FlowRecord {
    id: String,
    summary: String,
    artifacts: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ArtifactRecord {
    id: String,
    kind: String,
    paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AllowlistEntry {
    name: String,
    justification: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        CommandKind::Doctor { strict } => doctor(strict),
        CommandKind::TraceabilityCheck => traceability_check(),
        CommandKind::StructuralCheck => structural_check(),
    }
}

fn doctor(strict: bool) -> Result<()> {
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

fn traceability_check() -> Result<()> {
    let repo_root = repo_root()?;
    let trace_dir = repo_root.join("traceability");
    let requirements: Vec<RequirementRecord> =
        load_yaml(&trace_dir.join("requirements.yaml")).context("requirements")?;
    let invariants: Vec<InvariantRecord> =
        load_yaml(&trace_dir.join("invariants.yaml")).context("invariants")?;
    let flows: Vec<FlowRecord> = load_yaml(&trace_dir.join("flows.yaml")).context("flows")?;
    let artifacts: Vec<ArtifactRecord> =
        load_yaml(&trace_dir.join("artifacts.yaml")).context("artifacts")?;

    ensure_unique_ids(
        requirements.iter().map(|r| r.id.as_str()),
        "duplicate requirement id",
    )?;
    ensure_unique_ids(
        invariants.iter().map(|i| i.id.as_str()),
        "duplicate invariant id",
    )?;
    ensure_unique_ids(flows.iter().map(|f| f.id.as_str()), "duplicate flow id")?;
    ensure_unique_ids(
        artifacts.iter().map(|a| a.id.as_str()),
        "duplicate artifact id",
    )?;

    let artifact_map: HashMap<&str, &ArtifactRecord> = artifacts
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect();
    let mut referenced_artifacts = BTreeSet::new();
    for requirement in &requirements {
        ensure(
            !requirement.summary.trim().is_empty(),
            format!("requirement {} missing summary", requirement.id),
        )?;
        ensure(
            !requirement.artifacts.is_empty(),
            format!("requirement {} must reference artifacts", requirement.id),
        )?;
        for artifact_id in &requirement.artifacts {
            ensure(
                artifact_map.contains_key(artifact_id.as_str()),
                format!(
                    "requirement {} references missing artifact {}",
                    requirement.id, artifact_id
                ),
            )?;
            referenced_artifacts.insert(artifact_id.as_str());
        }
    }

    for invariant in &invariants {
        ensure(
            !invariant.statement.trim().is_empty(),
            format!("invariant {} missing statement", invariant.id),
        )?;
        ensure(
            !invariant.artifacts.is_empty(),
            format!("invariant {} must reference artifacts", invariant.id),
        )?;
        for artifact_id in &invariant.artifacts {
            ensure(
                artifact_map.contains_key(artifact_id.as_str()),
                format!(
                    "invariant {} references missing artifact {}",
                    invariant.id, artifact_id
                ),
            )?;
            referenced_artifacts.insert(artifact_id.as_str());
        }
    }

    for flow in &flows {
        ensure(
            !flow.summary.trim().is_empty(),
            format!("flow {} missing summary", flow.id),
        )?;
        ensure(
            !flow.artifacts.is_empty(),
            format!("flow {} must reference artifacts", flow.id),
        )?;
        for artifact_id in &flow.artifacts {
            ensure(
                artifact_map.contains_key(artifact_id.as_str()),
                format!(
                    "flow {} references missing artifact {}",
                    flow.id, artifact_id
                ),
            )?;
            referenced_artifacts.insert(artifact_id.as_str());
        }
    }

    for artifact in &artifacts {
        ensure(
            !artifact.kind.trim().is_empty(),
            format!("artifact {} missing kind", artifact.id),
        )?;
        ensure(
            !artifact.paths.is_empty(),
            format!("artifact {} must declare paths", artifact.id),
        )?;
        for path in &artifact.paths {
            let full = repo_root.join(path);
            ensure(
                full.exists(),
                format!("artifact path missing: {}", full.display()),
            )?;
        }
        ensure(
            referenced_artifacts.contains(artifact.id.as_str()),
            format!(
                "artifact {} is orphaned from requirements/invariants/flows",
                artifact.id
            ),
        )?;
    }

    println!("traceability-check: ok");
    Ok(())
}

fn structural_check() -> Result<()> {
    let repo_root = repo_root()?;
    let tracked_files = tracked_repo_files(&repo_root)?;
    check_for_absolute_paths(&repo_root, &tracked_files)?;
    check_for_stale_references(&repo_root, &tracked_files)?;
    check_allow_justifications(&repo_root)?;
    check_pub_items_have_references(&repo_root)?;
    check_ci_parity(&repo_root)?;
    println!("structural-check: ok");
    Ok(())
}

/// Assert that the GitHub Actions workflow does not drift from the local
/// `cargo xtask` command surface and the canonical devcontainer Dockerfile.
///
/// This is the safety harness that catches the kind of drift we hit
/// repeatedly during the v0.3 prep work: someone updates `.github/workflows/ci.yml`
/// without updating `tools/xtask/src/main.rs` (or vice versa), and CI passes
/// locally but fails on GitHub because the two pipelines run different
/// commands. The check is purely mechanical:
///
/// 1. Every `cargo xtask <subcommand>` referenced in `ci.yml` must exist
///    as an `XtaskCommand` variant in `tools/xtask/src/main.rs`.
/// 2. Every tool installed via `taiki-e/install-action` in the workflow
///    must also be installed by `cargo xtask setup` in `tools/xtask/src/main.rs`.
/// 3. The Dockerfile and the workflow must agree on tool versions for
///    `cargo-deny`, `cargo-llvm-cov`, `cargo-mutants`, `cargo-nextest`, and
///    `mdbook` (the tools we care about pinning).
///
/// The implementation uses string-grep instead of YAML parsing to keep the
/// dependency surface minimal and the failure messages legible. If the
/// workflow YAML reorganizes substantially, this check will need updates,
/// which is the right behavior — drift detection requires the check to
/// itself be regularly maintained.
fn check_ci_parity(repo_root: &Path) -> Result<()> {
    let ci_yml = fs::read_to_string(repo_root.join(".github/workflows/ci.yml"))
        .context("read .github/workflows/ci.yml")?;
    let xtask_main = fs::read_to_string(repo_root.join("tools/xtask/src/main.rs"))
        .context("read tools/xtask/src/main.rs")?;
    let dockerfile = fs::read_to_string(repo_root.join(".devcontainer/Dockerfile"))
        .context("read .devcontainer/Dockerfile")?;

    // 1. Every `cargo xtask <subcommand>` in ci.yml must exist in xtask main.rs.
    //    Match `cargo xtask <word>` patterns and look the word up in the
    //    XtaskCommand enum.
    let xtask_cmd_re = Regex::new(r"cargo\s+xtask\s+([a-z][a-z0-9-]*)").unwrap();
    let mut found_subcommands: BTreeSet<String> = BTreeSet::new();
    for cap in xtask_cmd_re.captures_iter(&ci_yml) {
        if let Some(sub) = cap.get(1) {
            found_subcommands.insert(sub.as_str().to_string());
        }
    }
    for sub in &found_subcommands {
        // Map kebab-case to PascalCase: "perf-gates" → "PerfGates".
        let pascal: String = sub
            .split('-')
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    Some(c) => c.to_uppercase().chain(chars).collect::<String>(),
                    None => String::new(),
                }
            })
            .collect();
        // The variant must appear in the XtaskCommand enum (with optional args).
        let needle_a = format!("    {pascal},");
        let needle_b = format!("    {pascal}(");
        if !xtask_main.contains(&needle_a) && !xtask_main.contains(&needle_b) {
            bail!(
                "ci-parity: workflow references `cargo xtask {sub}` but no \
                 matching `XtaskCommand::{pascal}` variant in \
                 tools/xtask/src/main.rs. Either add the command to xtask \
                 or fix the workflow."
            );
        }
    }

    // 2. Every tool installed via taiki-e/install-action in ci.yml must also
    //    be installed by `cargo xtask setup` in tools/xtask/src/main.rs.
    let tool_install_re = Regex::new(r#"tool:\s*([a-z][a-z0-9-]*)"#).unwrap();
    let mut workflow_tools: BTreeSet<String> = BTreeSet::new();
    for cap in tool_install_re.captures_iter(&ci_yml) {
        if let Some(tool) = cap.get(1) {
            workflow_tools.insert(tool.as_str().to_string());
        }
    }
    for tool in &workflow_tools {
        if !xtask_main.contains(&format!("\"{tool}\"")) {
            bail!(
                "ci-parity: workflow installs `{tool}` via taiki-e/install-action \
                 but `cargo xtask setup` in tools/xtask/src/main.rs does not list \
                 it. Either add it to setup or remove from the workflow."
            );
        }
    }

    // 3. Tool version pin parity between Dockerfile and xtask setup.
    let pinned_tools = [
        "cargo-nextest",
        "cargo-deny",
        "cargo-llvm-cov",
        "cargo-mutants",
        "mdbook",
    ];
    for tool in pinned_tools {
        let dock_pin_re = Regex::new(&format!(r"{tool}@(\d+(?:\.\d+)+)")).unwrap();
        let xtask_pin_re = Regex::new(&format!(r"{tool}@(\d+(?:\.\d+)+)")).unwrap();
        let dock_v = dock_pin_re
            .captures(&dockerfile)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());
        let xtask_v = xtask_pin_re
            .captures(&xtask_main)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());
        match (dock_v, xtask_v) {
            (Some(d), Some(x)) if d != x => {
                bail!(
                    "ci-parity: tool `{tool}` is pinned to `{d}` in \
                     `.devcontainer/Dockerfile` but `{x}` in `cargo xtask setup` \
                     (tools/xtask/src/main.rs). Pick one version and update both."
                );
            }
            (Some(_), None) => {
                bail!(
                    "ci-parity: tool `{tool}` is pinned in \
                     `.devcontainer/Dockerfile` but unpinned (or missing) in \
                     `cargo xtask setup`. Add the same pin to xtask setup so \
                     local installs match the container."
                );
            }
            _ => {}
        }
    }

    Ok(())
}

fn check_for_absolute_paths(repo_root: &Path, tracked_files: &[PathBuf]) -> Result<()> {
    let absolute_windows = Regex::new(r"(^|[^A-Za-z])([A-Za-z]:\\)").unwrap();
    let absolute_unix = Regex::new(r"(?m)(file://|/Users/|/home/|/opt/|/tmp/)").unwrap();
    let allow = [
        repo_root.join(".devcontainer/Dockerfile"),
        repo_root.join("docs/audits/HICP_AUDIT_REPORT.md"),
        repo_root.join("tools/integrity/src/main.rs"),
    ];
    for path in tracked_files {
        if allow.iter().any(|allowed| allowed == path) {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default();
        let is_text = matches!(
            ext,
            "rs" | "toml" | "md" | "yml" | "yaml" | "json" | "sh" | "stderr"
        ) || path.file_name().and_then(|s| s.to_str()) == Some("justfile");
        if !is_text {
            continue;
        }
        let content =
            fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        ensure(
            !absolute_windows.is_match(&content),
            format!(
                "absolute Windows path leak in {}",
                relative(repo_root, path)
            ),
        )?;
        ensure(
            !absolute_unix.is_match(&content),
            format!("absolute Unix path leak in {}", relative(repo_root, path)),
        )?;
    }
    Ok(())
}

fn check_for_stale_references(repo_root: &Path, tracked_files: &[PathBuf]) -> Result<()> {
    let stale_terms = [
        // Old test file names (renamed in repo flatten)
        "self_benchmark.rs",
        "quiet_stragglers.rs",
        "bigbang_compliance.rs",
        "coverage_gaps.rs",
        // Old API name (replaced by AppendOptions::with_cas)
        "with_expected_sequence",
        // Deleted plan file
        "plans-test-bench-reorganization",
        // Old MSRV (updated to 1.92)
        "MSRV is 1.75",
        "MSRV: 1.75",
        "MSRV 1.75",
    ];
    let allow = [
        // Audit report legitimately documents historical state
        repo_root.join("docs/audits/HICP_AUDIT_REPORT.md"),
        // This file contains the terms as string literals
        repo_root.join("tools/integrity/src/main.rs"),
    ];
    for path in tracked_files {
        if allow.iter().any(|allowed| allowed == path) {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default();
        if !matches!(ext, "md" | "rs" | "toml" | "yml" | "yaml" | "json" | "sh") {
            continue;
        }
        let content =
            fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        for term in stale_terms {
            ensure(
                !content.contains(term),
                format!(
                    "stale reference `{term}` found in {}",
                    relative(repo_root, path)
                ),
            )?;
        }
    }
    Ok(())
}

fn check_allow_justifications(repo_root: &Path) -> Result<()> {
    for path in rust_files(&repo_root.join("src")) {
        let content = fs::read_to_string(&path)?;
        let lines: Vec<&str> = content.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("#![allow") {
                continue;
            }
            if trimmed.starts_with("#[allow(") {
                let justified = trimmed.contains("//")
                    || index
                        .checked_sub(1)
                        .and_then(|prev| lines.get(prev))
                        .map(|prev| prev.trim_start().starts_with("//"))
                        .unwrap_or(false);
                ensure(
                    justified,
                    format!(
                        "unjustified allow in {}:{}",
                        relative(repo_root, &path),
                        index + 1
                    ),
                )?;
            }
        }
    }
    Ok(())
}

fn check_pub_items_have_references(repo_root: &Path) -> Result<()> {
    let allowlist: Vec<AllowlistEntry> =
        load_yaml(&repo_root.join("traceability/pub_item_allowlist.yaml"))?;
    let allowed: HashMap<&str, &str> = allowlist
        .iter()
        .map(|entry| (entry.name.as_str(), entry.justification.as_str()))
        .collect();
    let reference_space = collect_reference_text(repo_root)?;
    for path in rust_files(&repo_root.join("src")) {
        let content = fs::read_to_string(&path)?;
        let file = syn::parse_file(&content)
            .with_context(|| format!("parse {}", relative(repo_root, &path)))?;
        for name in public_item_names(&file) {
            if allowed.contains_key(name.as_str()) {
                continue;
            }
            ensure(
                reference_space.contains(&name),
                format!(
                    "public item `{}` from {} has no reference in tests/benches/examples/docs",
                    name,
                    relative(repo_root, &path)
                ),
            )?;
        }
    }
    Ok(())
}

fn collect_reference_text(repo_root: &Path) -> Result<BTreeSet<String>> {
    let mut refs = BTreeSet::new();
    let mut files = rust_files(&repo_root.join("tests"));
    files.extend(rust_files(&repo_root.join("benches")));
    files.extend(rust_files(&repo_root.join("examples")));
    files.extend(files_with_extension(&repo_root.join("guide/src"), "md"));
    files.push(repo_root.join("README.md"));
    files.push(repo_root.join("CONTRIBUTING.md"));
    files.push(repo_root.join("AGENTS.md"));
    files.push(repo_root.join("docs/reference/ARCHITECTURE.md"));
    for path in files {
        let content = fs::read_to_string(&path)?;
        for token in content.split(|ch: char| !ch.is_alphanumeric() && ch != '_') {
            if !token.is_empty() {
                refs.insert(token.to_string());
            }
        }
    }
    Ok(refs)
}

fn public_item_names(file: &syn::File) -> Vec<String> {
    let mut names = Vec::new();
    for item in &file.items {
        match item {
            Item::Fn(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.push(item.sig.ident.to_string());
            }
            Item::Struct(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.push(item.ident.to_string());
            }
            Item::Enum(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.push(item.ident.to_string());
            }
            Item::Trait(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.push(item.ident.to_string());
            }
            Item::Type(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.push(item.ident.to_string());
            }
            Item::Const(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.push(item.ident.to_string());
            }
            Item::Mod(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.push(item.ident.to_string());
            }
            _ => {}
        }
    }
    names
}

fn tracked_repo_files(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .args(["ls-files"])
        .current_dir(repo_root)
        .output()
        .context("git ls-files")?;
    ensure(output.status.success(), "git ls-files failed")?;

    let stdout = String::from_utf8(output.stdout).context("git ls-files utf8")?;
    let mut files = Vec::new();
    for line in stdout.lines().filter(|line| !line.is_empty()) {
        let path = repo_root.join(line);
        if path.exists() {
            files.push(path);
        }
    }
    Ok(files)
}

fn rust_files(root: &Path) -> Vec<PathBuf> {
    files_with_extension(root, "rs")
}

fn files_with_extension(root: &Path, extension: &str) -> Vec<PathBuf> {
    if !root.exists() {
        return Vec::new();
    }
    WalkDir::new(root)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some(extension))
        .map(|entry| entry.into_path())
        .collect()
}

fn repo_root() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("failed to determine repo root"))
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn load_yaml<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_yaml::from_str(&content).with_context(|| format!("parse {}", path.display()))
}

fn ensure_unique_ids<'a>(ids: impl IntoIterator<Item = &'a str>, context: &str) -> Result<()> {
    let mut seen = BTreeSet::new();
    for id in ids {
        ensure(seen.insert(id.to_string()), format!("{context}: {id}"))?;
    }
    Ok(())
}

fn check_command(program: &str, args: &[&str]) -> Result<()> {
    ensure(
        command_exists(program, args),
        format!(
            "required command missing or failing: {program} {}",
            args.join(" ")
        ),
    )
}

fn command_exists(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn ensure(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        bail!(message.into())
    }
}
