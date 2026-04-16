mod architecture_lints;

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
    architecture_lints::check(&repo_root, &tracked_files)?;
    check_allow_justifications(&repo_root)?;
    check_pub_items_have_references(&repo_root)?;
    check_ci_parity(&repo_root)?;
    check_store_pub_fn_coverage(&repo_root)?;
    println!("structural-check: ok");
    Ok(())
}

/// Assert that the live GitHub Actions workflows do not drift from the local
/// `cargo xtask` command surface and the canonical devcontainer Dockerfile.
///
/// This is the safety harness that catches the kind of drift we hit
/// repeatedly during the v0.3 prep work: someone updates a workflow without
/// updating the xtask source tree (or vice versa), and CI passes locally but
/// fails on GitHub because the two pipelines run different commands. The check
/// is purely mechanical:
///
/// 1. Every `cargo xtask <subcommand>` referenced in `ci.yml`, `perf.yml`,
///    or `release.yml` must exist
///    as an `XtaskCommand` variant in `tools/xtask/src/main.rs`.
/// 2. Every tool installed via `taiki-e/install-action` in the workflow
///    must also be installed by `cargo xtask setup --install-tools` in the xtask source tree.
/// 3. The Dockerfile and the workflow must agree on tool versions for
///    `cargo-deny`, `cargo-llvm-cov`, `cargo-mutants`, `cargo-nextest`, and
///    `cargo-audit` (the tools we care about pinning).
/// 4. Workflow-owned matrix values for perf surfaces and scheduled mutation
///    shards must stay inside the exact xtask-owned truth set.
///
/// The implementation uses string-grep instead of YAML parsing to keep the
/// dependency surface minimal and the failure messages legible. If the
/// workflow YAML reorganizes substantially, this check will need updates,
/// which is the right behavior — drift detection requires the check to
/// itself be regularly maintained.
fn check_ci_parity(repo_root: &Path) -> Result<()> {
    let ci_yml = fs::read_to_string(repo_root.join(".github/workflows/ci.yml"))
        .context("read .github/workflows/ci.yml")?;
    let perf_yml = fs::read_to_string(repo_root.join(".github/workflows/perf.yml"))
        .context("read .github/workflows/perf.yml")?;
    let release_yml = fs::read_to_string(repo_root.join(".github/workflows/release.yml"))
        .context("read .github/workflows/release.yml")?;
    let xtask_main = fs::read_to_string(repo_root.join("tools/xtask/src/main.rs"))
        .context("read tools/xtask/src/main.rs")?;
    let xtask_sources = xtask_source_text(repo_root)?;
    let dockerfile = fs::read_to_string(repo_root.join(".devcontainer/Dockerfile"))
        .context("read .devcontainer/Dockerfile")?;
    let workflows = [
        (".github/workflows/ci.yml", ci_yml.as_str()),
        (".github/workflows/perf.yml", perf_yml.as_str()),
        (".github/workflows/release.yml", release_yml.as_str()),
    ];

    // 1. Every `cargo xtask <subcommand>` in the live workflows must exist in
    //    xtask main.rs.
    //    Match `cargo xtask <word>` patterns and look the word up in the
    //    XtaskCommand enum.
    let xtask_cmd_re = Regex::new(r"cargo\s+xtask\s+([a-z][a-z0-9-]*)").unwrap();
    let mut found_subcommands: BTreeSet<String> = BTreeSet::new();
    for (_, workflow) in workflows {
        for cap in xtask_cmd_re.captures_iter(workflow) {
            if let Some(sub) = cap.get(1) {
                found_subcommands.insert(sub.as_str().to_string());
            }
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

    assert_workflow_list_values(
        ".github/workflows/perf.yml",
        &perf_yml,
        "surface",
        &["neutral", "native"],
    )?;
    assert_workflow_list_values(
        ".github/workflows/ci.yml",
        &ci_yml,
        "surface",
        &["all-features", "no-default-features"],
    )?;
    assert_workflow_list_values(
        ".github/workflows/ci.yml",
        &ci_yml,
        "shard",
        &[
            "1/12", "2/12", "3/12", "4/12", "5/12", "6/12", "7/12", "8/12", "9/12", "10/12",
            "11/12", "12/12",
        ],
    )?;
    ensure(
        release_yml.contains("bash ./scripts/run-in-devcontainer.sh 'cargo xtask release --dry-run'"),
        "ci-parity: `.github/workflows/release.yml` must run `cargo xtask release --dry-run` through `scripts/run-in-devcontainer.sh`.",
    )?;

    // 2. Every tool installed via taiki-e/install-action in ci.yml must
    //    be pinned to the same version that `.devcontainer/Dockerfile`
    //    and `cargo xtask setup --install-tools` use.
    //
    //    This guards three drift vectors at once:
    //    (a) workflow installs an unpinned tool (Windows CI silently picks
    //        up a new release that breaks against pinned Linux);
    //    (b) workflow pins to a different version than the container;
    //    (c) tool added to workflow but missing from xtask setup.
    //
    //    The regex requires the canonical `name@x.y[.z]` form. A bare
    //    `tool: nextest` is intentionally rejected — see ci.yml for the
    //    drift comment that explains the lock-step requirement.
    let tool_install_re = Regex::new(r#"tool:\s*([a-z][a-z0-9-]*)@(\d+(?:\.\d+)+)"#).unwrap();
    let bare_tool_re = Regex::new(r#"tool:\s*([a-z][a-z0-9-]*)\s*$"#).unwrap();
    // Reject any unpinned `tool:` entry up front so we never have to wonder
    // why a Windows install drifted from the canonical Linux pin.
    for line in ci_yml.lines() {
        if let Some(cap) = bare_tool_re.captures(line.trim_end()) {
            let tool = cap.get(1).map(|m| m.as_str()).unwrap_or("?");
            bail!(
                "ci-parity: `.github/workflows/ci.yml` installs `{tool}` via \
                 taiki-e/install-action without a version pin. Use \
                 `tool: {tool}@<version>` so Linux and Windows CI install the \
                 same version. Match the pin in `.devcontainer/Dockerfile` \
                 and `cargo xtask setup --install-tools`."
            );
        }
    }
    let mut workflow_tools: BTreeSet<(String, String)> = BTreeSet::new();
    for cap in tool_install_re.captures_iter(&ci_yml) {
        if let (Some(tool), Some(ver)) = (cap.get(1), cap.get(2)) {
            workflow_tools.insert((tool.as_str().to_string(), ver.as_str().to_string()));
        }
    }
    for (tool, wf_ver) in &workflow_tools {
        // The xtask setup list must contain `"{tool}"` as a tuple key
        // followed (within ~80 bytes on the next install line) by
        // `{tool}@{version}`. Both checks together kill the
        // accidental-substring-match failure mode.
        let setup_key = format!("\"{tool}\"");
        if !xtask_sources.contains(&setup_key) {
            bail!(
                "ci-parity: workflow installs `{tool}@{wf_ver}` via \
                 taiki-e/install-action but `cargo xtask setup --install-tools` in \
                 tools/xtask/src/ does not list a `{setup_key}` entry. \
                 Either add it to setup or remove from the workflow."
            );
        }
        let xtask_pin_re = Regex::new(&format!(r"{tool}@(\d+(?:\.\d+)+)")).unwrap();
        let xtask_ver = xtask_pin_re
            .captures(&xtask_sources)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());
        match xtask_ver {
            Some(x) if &x != wf_ver => {
                bail!(
                    "ci-parity: tool `{tool}` is pinned to `{wf_ver}` in \
                     `.github/workflows/ci.yml` but `{x}` in `cargo xtask setup --install-tools` \
                     (tools/xtask/src/). Pick one version and update both."
                );
            }
            None => {
                bail!(
                    "ci-parity: tool `{tool}` is pinned in \
                     `.github/workflows/ci.yml` but unpinned (or missing) in \
                     `cargo xtask setup --install-tools`. Add the same pin so local installs \
                     match the workflow."
                );
            }
            _ => {}
        }
    }

    // 3. Tool version pin parity between Dockerfile and xtask setup.
    let pinned_tools = [
        "cargo-nextest",
        "cargo-deny",
        "cargo-audit",
        "cargo-llvm-cov",
        "cargo-mutants",
    ];
    for tool in pinned_tools {
        let dock_pin_re = Regex::new(&format!(r"{tool}@(\d+(?:\.\d+)+)")).unwrap();
        let xtask_pin_re = Regex::new(&format!(r"{tool}@(\d+(?:\.\d+)+)")).unwrap();
        let dock_v = dock_pin_re
            .captures(&dockerfile)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());
        let xtask_v = xtask_pin_re
            .captures(&xtask_sources)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());
        match (dock_v, xtask_v) {
            (Some(d), Some(x)) if d != x => {
                bail!(
                    "ci-parity: tool `{tool}` is pinned to `{d}` in \
                     `.devcontainer/Dockerfile` but `{x}` in `cargo xtask setup --install-tools` \
                     (tools/xtask/src/). Pick one version and update both."
                );
            }
            (Some(_), None) => {
                bail!(
                    "ci-parity: tool `{tool}` is pinned in \
                     `.devcontainer/Dockerfile` but unpinned (or missing) in \
                     `cargo xtask setup --install-tools`. Add the same pin to xtask setup so \
                     local installs match the container."
                );
            }
            _ => {}
        }
    }

    Ok(())
}

fn assert_workflow_list_values(
    workflow_name: &str,
    workflow: &str,
    key: &str,
    expected: &[&str],
) -> Result<()> {
    let expected_set: BTreeSet<String> = expected.iter().map(|value| (*value).to_owned()).collect();
    let actual_set: BTreeSet<String> = workflow_list_values(workflow, key)?.into_iter().collect();
    ensure(
        actual_set == expected_set,
        format!(
            "ci-parity: `{workflow_name}` must declare `{key}` values {:?}, found {:?}",
            expected, actual_set
        ),
    )?;
    Ok(())
}

fn workflow_list_values(workflow: &str, key: &str) -> Result<Vec<String>> {
    let inline_re = Regex::new(&format!(r"^\s*{}\s*:\s*\[(?P<values>[^\]]+)\]\s*$", key)).unwrap();
    let mut lines = workflow.lines().enumerate().peekable();
    while let Some((index, line)) = lines.next() {
        if let Some(caps) = inline_re.captures(line) {
            let values = caps["values"]
                .split(',')
                .map(|value| value.trim().trim_matches('"').trim_matches('\'').to_owned())
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>();
            return Ok(values);
        }

        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix(key) else {
            continue;
        };
        if !rest.trim_start().starts_with(':') {
            continue;
        }
        let base_indent = indentation(line);
        let mut values = Vec::new();
        while let Some((_, next_line)) = lines.peek() {
            let next_trimmed = next_line.trim();
            if next_trimmed.is_empty() {
                lines.next();
                continue;
            }
            let next_indent = indentation(next_line);
            if next_indent <= base_indent {
                break;
            }
            if let Some(value) = next_trimmed.strip_prefix("- ") {
                values.push(value.trim().trim_matches('"').trim_matches('\'').to_owned());
                lines.next();
                continue;
            }
            break;
        }
        if !values.is_empty() {
            return Ok(values);
        }
        bail!(
            "ci-parity: found `{key}:` in workflow but could not read any values near line {}",
            index + 1
        );
    }

    bail!("ci-parity: could not find `{key}` list in workflow")
}

fn indentation(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn xtask_source_text(repo_root: &Path) -> Result<String> {
    let mut combined = String::new();
    for path in files_with_extension(&repo_root.join("tools/xtask/src"), "rs") {
        combined.push_str(
            &fs::read_to_string(&path)
                .with_context(|| format!("read {}", relative(repo_root, &path)))?,
        );
        combined.push('\n');
    }
    Ok(combined)
}

/// Assert that every `pub fn` declared in `impl Store { ... }` blocks in
/// `src/store/mod.rs` has at least one reference in the test or source tree.
///
/// This is a structural guard: if a method is added to `Store` and no test
/// exercises it, a developer (or agent) has likely forgotten to write the test.
/// The check uses `syn` to parse the AST — no regex heuristics for pub fn
/// detection — so methods inside `#[cfg(...)]` blocks or across multiple `impl`
/// blocks are handled correctly.
///
/// Reference detection uses regex against the combined text of `tests/` and
/// `src/` (which covers both standalone test files and `#[cfg(test)] mod tests`
/// inline in source files).
fn check_store_pub_fn_coverage(repo_root: &Path) -> Result<()> {
    // Methods that are deliberately exercised only indirectly or are
    // intentionally infrastructure-only. Start empty and add only proven
    // false positives with a justification comment.
    let allowlist: &[&str] = &[
        // `subscription` is doc(hidden) glue for async integration, exercised
        // indirectly via `subscribe` in every subscription test.
        "subscription",
    ];

    // 1. Parse src/store/mod.rs with syn and walk all `impl Store` blocks.
    let store_mod_path = repo_root.join("src/store/mod.rs");
    let source = fs::read_to_string(&store_mod_path)
        .with_context(|| format!("read {}", store_mod_path.display()))?;
    let ast = syn::parse_file(&source)
        .with_context(|| format!("syn parse {}", store_mod_path.display()))?;

    let mut pub_fns: BTreeSet<String> = BTreeSet::new();
    for item in &ast.items {
        if let Item::Impl(impl_block) = item {
            // Match `impl Store` and `impl<T> Store<T>` (and `impl<T> ProjectionWatcher<T>`).
            // We only care about blocks whose self type is `Store` (not ProjectionWatcher).
            let is_store_impl = match impl_block.self_ty.as_ref() {
                syn::Type::Path(tp) => tp
                    .path
                    .segments
                    .last()
                    .map(|s| s.ident == "Store")
                    .unwrap_or(false),
                _ => false,
            };
            // Trait impls (e.g., `impl Drop for Store`) are excluded — we only
            // want inherent impls.
            if !is_store_impl || impl_block.trait_.is_some() {
                continue;
            }
            for impl_item in &impl_block.items {
                if let syn::ImplItem::Fn(method) = impl_item {
                    if matches!(method.vis, syn::Visibility::Public(_)) {
                        let name = method.sig.ident.to_string();
                        // Skip names starting with `_` (private convention).
                        if !name.starts_with('_') {
                            pub_fns.insert(name);
                        }
                    }
                }
            }
        }
    }

    if pub_fns.is_empty() {
        bail!(
            "structural-check: Store pub fn coverage — could not find any `impl Store` \
             block in src/store/mod.rs. The file may have been restructured; update this check."
        );
    }

    // 2. Build the reference corpus: all .rs files under tests/ and src/.
    let mut search_files: Vec<PathBuf> = rust_files(&repo_root.join("tests"));
    search_files.extend(rust_files(&repo_root.join("src")));

    // 3. For each pub fn, check that at least one file references it as a call.
    //    Patterns matched: `.name(`, `Store::name(`, `store.name(`
    let mut unreferenced: Vec<String> = Vec::new();
    for name in &pub_fns {
        if allowlist.contains(&name.as_str()) {
            continue;
        }
        // Build patterns that strongly indicate a method call or direct use.
        // We accept any of:
        //   `.name(`        — method call syntax
        //   `.name::<`      — method call with turbofish (e.g., `.watch_projection::<T>(...)`)
        //   `Store::name(`  — fully-qualified call
        //   `Store::name::<` — fully-qualified call with turbofish
        //   `store.name(`   — conventional variable name
        //   `store.name::<` — conventional variable name with turbofish
        // The turbofish variants are critical: we miss generic method calls
        // without them. Caught by the watch_projection false-positive when
        // this check first ran against the real codebase.
        let patterns = [
            format!(".{}(", name),
            format!(".{}::<", name),
            format!("Store::{}(", name),
            format!("Store::{}::<", name),
            format!("store.{}(", name),
            format!("store.{}::<", name),
        ];
        let mut found = false;
        'files: for path in &search_files {
            let content = match fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for pat in &patterns {
                if content.contains(pat.as_str()) {
                    found = true;
                    break 'files;
                }
            }
        }
        if !found {
            unreferenced.push(name.clone());
        }
    }

    if !unreferenced.is_empty() {
        let list = unreferenced
            .iter()
            .map(|n| format!("  - {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        bail!(
            "structural-check: Store pub fn coverage failure — the following methods on\n\
             `impl Store` have ZERO test or source references and are likely orphaned:\n\
             {list}\n\
             Investigate: src/store/mod.rs and add a test exercising each, or remove the\n\
             method if it's truly unused."
        );
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
    files.push(repo_root.join("README.md"));
    files.push(repo_root.join("GUIDE.md"));
    files.push(repo_root.join("REFERENCE.md"));
    files.push(repo_root.join("CONTRIBUTING.md"));
    files.push(repo_root.join("AGENTS.md"));
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
    yaml_serde::from_str(&content).with_context(|| format!("parse {}", path.display()))
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
