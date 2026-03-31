use anyhow::{anyhow, bail, Context, Result};
use cargo_metadata::MetadataCommand;
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
        repo_root.join("batpak/tools/integrity/Cargo.toml"),
        repo_root.join("traceability/requirements.yaml"),
    ];
    for path in canonical_files {
        ensure(path.exists(), format!("missing canonical file {}", path.display()))?;
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
        let has_container_runtime = command_exists("docker", &["--version"])
            || command_exists("podman", &["--version"]);
        let host_ok = if cfg!(windows) {
            command_exists("cl", &[])
                || command_exists(
                    "cmd",
                    &[
                        "/C",
                        "where cl >NUL 2>NUL || where link >NUL 2>NUL",
                    ],
                )
        } else {
            command_exists("clang", &["--version"]) || command_exists("cc", &["--version"])
        };
        ensure(
            has_container_runtime || host_ok,
            "strict doctor requires either a container runtime or a validated native toolchain",
        )?;
    }

    let git_attrs = fs::read_to_string(repo_root.join(".gitattributes"))
        .context("read .gitattributes")?;
    ensure(
        git_attrs.contains("eol=lf"),
        ".gitattributes must normalize line endings",
    )?;

    println!("doctor: ok");
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

    let artifact_map: HashMap<&str, &ArtifactRecord> =
        artifacts.iter().map(|record| (record.id.as_str(), record)).collect();
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
                format!("flow {} references missing artifact {}", flow.id, artifact_id),
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
            ensure(full.exists(), format!("artifact path missing: {}", full.display()))?;
        }
        ensure(
            referenced_artifacts.contains(artifact.id.as_str()),
            format!("artifact {} is orphaned from requirements/invariants/flows", artifact.id),
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
    println!("structural-check: ok");
    Ok(())
}

fn check_for_absolute_paths(repo_root: &Path, tracked_files: &[PathBuf]) -> Result<()> {
    let absolute_windows = Regex::new(r"(^|[^A-Za-z])([A-Za-z]:\\)").unwrap();
    let absolute_unix = Regex::new(r"(?m)(file://|/Users/|/home/|/opt/|/tmp/)").unwrap();
    let allow = [
        repo_root.join(".devcontainer/Dockerfile"),
        repo_root.join("HICP_AUDIT_REPORT.md"),
        repo_root.join("batpak/tools/integrity/src/main.rs"),
    ];
    for path in tracked_files {
        if allow.iter().any(|allowed| allowed == path) {
            continue;
        }
        let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or_default();
        let is_text = matches!(
            ext,
            "rs" | "toml" | "md" | "yml" | "yaml" | "json" | "sh" | "stderr"
        ) || path.file_name().and_then(|s| s.to_str()) == Some("justfile");
        if !is_text {
            continue;
        }
        let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        ensure(
            !absolute_windows.is_match(&content),
            format!("absolute Windows path leak in {}", relative(repo_root, path)),
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
        "self_benchmark.rs",
        "quiet_stragglers.rs",
        "bigbang_compliance.rs",
        "coverage_gaps.rs",
        "with_expected_sequence",
    ];
    let allow = [
        repo_root.join("HICP_AUDIT_REPORT.md"),
        repo_root.join("batpak/tools/integrity/src/main.rs"),
    ];
    for path in tracked_files {
        if allow.iter().any(|allowed| allowed == path) {
            continue;
        }
        let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or_default();
        if !matches!(ext, "md" | "rs" | "toml" | "yml" | "yaml" | "json" | "sh") {
            continue;
        }
        let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        for term in stale_terms {
            ensure(
                !content.contains(term),
                format!("stale reference `{term}` found in {}", relative(repo_root, path)),
            )?;
        }
    }
    Ok(())
}

fn check_allow_justifications(repo_root: &Path) -> Result<()> {
    for path in rust_files(&repo_root.join("batpak/src")) {
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
    for path in rust_files(&repo_root.join("batpak/src")) {
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
    let mut files = rust_files(&repo_root.join("batpak/tests"));
    files.extend(rust_files(&repo_root.join("batpak/benches")));
    files.extend(rust_files(&repo_root.join("batpak/examples")));
    files.push(repo_root.join("batpak/README.md"));
    files.push(repo_root.join("batpak/ARCHITECTURE.md"));
    files.push(repo_root.join("README.md"));
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
    let metadata = MetadataCommand::new()
        .current_dir(repo_root.join("batpak"))
        .exec()
        .context("cargo metadata")?;
    let workspace_root = PathBuf::from(metadata.workspace_root.as_std_path());
    let mut files = Vec::new();
    let walker = WalkDir::new(&workspace_root).into_iter().filter_entry(|entry| {
        let name = entry.file_name().to_string_lossy();
        name != ".git" && name != "target" && !name.starts_with("mutants")
    });
    for entry in walker.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if entry.file_type().is_file() {
            files.push(path.to_path_buf());
        }
    }
    let report = repo_root.join("HICP_AUDIT_REPORT.md");
    if report.exists() {
        files.push(report);
    }
    Ok(files)
}

fn rust_files(root: &Path) -> Vec<PathBuf> {
    if !root.exists() {
        return Vec::new();
    }
    WalkDir::new(root)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("rs"))
        .map(|entry| entry.into_path())
        .collect()
}

fn repo_root() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .nth(3)
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
        format!("required command missing or failing: {program} {}", args.join(" ")),
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
