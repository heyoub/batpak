use anyhow::{anyhow, Context, Result};
use pulldown_cmark::{Event, Options, Parser, Tag};
use regex::Regex;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use syn::{Item, UseTree};

pub fn check(repo_root: &Path, tracked_files: &[PathBuf]) -> Result<()> {
    check_no_tracked_archive_or_audit_docs(repo_root, tracked_files)?;
    check_no_live_spec_markers(repo_root, tracked_files)?;
    check_no_legacy_topology_or_replay_names(repo_root, tracked_files)?;
    check_for_absolute_paths(repo_root, tracked_files)?;
    check_for_stale_references(repo_root, tracked_files)?;
    check_boundary_scripts_only(repo_root, tracked_files)?;
    check_removed_script_references(repo_root, tracked_files)?;
    check_no_mdbook_dependency(repo_root)?;
    check_justfile_stays_thin(repo_root)?;
    check_packaging_surface(repo_root)?;
    check_xtask_surface_contract(repo_root)?;
    check_release_hardening_patterns(repo_root, tracked_files)?;
    check_portable_context_links(repo_root)?;
    check_root_doc_site_contract(repo_root)?;
    check_live_docs_do_not_link_archives(repo_root)?;
    check_public_surface_truth(repo_root)?;
    Ok(())
}

fn check_no_tracked_archive_or_audit_docs(
    repo_root: &Path,
    tracked_files: &[PathBuf],
) -> Result<()> {
    for path in tracked_files {
        let rel = relative(repo_root, path);
        ensure(
            !rel.starts_with("docs/archive/") && !rel.starts_with("docs/audits/"),
            format!(
                "tracked archive/audit material is no longer allowed in-tree; move `{rel}` to external archive storage"
            ),
        )?;
    }
    Ok(())
}

fn check_no_live_spec_markers(repo_root: &Path, tracked_files: &[PathBuf]) -> Result<()> {
    let marker = Regex::new(r"\\?\[SPEC:").unwrap();
    let allow = [repo_root.join("tools/integrity/src/architecture_lints.rs")];
    for path in tracked_files {
        if allow.iter().any(|allowed| allowed == path) {
            continue;
        }
        let rel = relative(repo_root, path);
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default();
        let is_root_markdown = !rel.contains('/') && ext == "md";
        let is_live_surface = is_root_markdown
            || rel.starts_with("src/")
            || rel.starts_with("tools/")
            || rel.starts_with("tests/")
            || rel.starts_with("benches/")
            || rel.starts_with("examples/");
        if !is_live_surface || !matches!(ext, "rs" | "md") {
            continue;
        }
        let content =
            fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        ensure(
            !marker.is_match(&content),
            format!(
                "live surface still contains embedded [SPEC:...] markers: {}",
                relative(repo_root, path)
            ),
        )?;
    }
    Ok(())
}

fn check_no_legacy_topology_or_replay_names(
    repo_root: &Path,
    tracked_files: &[PathBuf],
) -> Result<()> {
    let banned_terms = [
        ("IndexLayout", Regex::new(r"\bIndexLayout\b").unwrap()),
        ("ViewConfig", Regex::new(r"\bViewConfig\b").unwrap()),
        ("ProjectionMode", Regex::new(r"\bProjectionMode\b").unwrap()),
        ("ValueInput", Regex::new(r"\bValueInput\b").unwrap()),
    ];
    let allow = [
        repo_root.join("CHANGELOG.md"),
        repo_root.join("tools/integrity/src/architecture_lints.rs"),
    ];
    for path in tracked_files {
        if allow.iter().any(|allowed| allowed == path) {
            continue;
        }
        let rel = relative(repo_root, path);
        let is_live_surface = rel == "README.md"
            || rel == "GUIDE.md"
            || rel == "REFERENCE.md"
            || rel.starts_with("src/")
            || rel.starts_with("examples/")
            || rel.starts_with("tests/");
        if !is_live_surface {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default();
        if !matches!(ext, "rs" | "md") {
            continue;
        }
        let content =
            fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        for (term, pattern) in &banned_terms {
            ensure(
                !pattern.is_match(&content),
                format!(
                    "live surface {} still references removed legacy term `{term}`",
                    relative(repo_root, path)
                ),
            )?;
        }
    }
    Ok(())
}

fn check_for_absolute_paths(repo_root: &Path, tracked_files: &[PathBuf]) -> Result<()> {
    let absolute_windows = Regex::new(r"(^|[^A-Za-z])([A-Za-z]:\\)").unwrap();
    let absolute_unix = Regex::new(r"(?m)(file://|/Users/|/home/|/opt/|/tmp/)").unwrap();
    let allow = [
        repo_root.join(".devcontainer/Dockerfile"),
        repo_root.join("tools/integrity/src/main.rs"),
        repo_root.join("tools/integrity/src/architecture_lints.rs"),
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
        "self_benchmark.rs",
        "quiet_stragglers.rs",
        "bigbang_compliance.rs",
        "coverage_gaps.rs",
        "with_expected_sequence",
        "plans-test-bench-reorganization",
        "MSRV is 1.75",
        "MSRV: 1.75",
        "MSRV 1.75",
        "RedbCache",
        "LmdbCache",
        "entity_locks",
        "cache_map_size_bytes",
        "with_cache_map_size_bytes",
        "open_with_redb_cache",
        "open_with_lmdb_cache",
        "Freshness::BestEffort",
        "subscribe(region)",
        "cursor(region)",
        "`test-support`",
    ];
    let allow = [
        repo_root.join("docs/adr/ADR-0003-cache-safety-assumptions.md"),
        repo_root.join("CHANGELOG.md"),
        repo_root.join("AGENTS.md"),
        repo_root.join("build.rs"),
        repo_root.join("tools/integrity/src/main.rs"),
        repo_root.join("tools/integrity/src/architecture_lints.rs"),
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

fn check_release_hardening_patterns(repo_root: &Path, tracked_files: &[PathBuf]) -> Result<()> {
    let historical_allow = [
        repo_root.join("CHANGELOG.md"),
        repo_root.join("build.rs"),
        repo_root.join("tools/integrity/src/main.rs"),
        repo_root.join("tools/integrity/src/architecture_lints.rs"),
    ];
    for path in tracked_files {
        let rel = relative(repo_root, path);
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default();
        let is_text = matches!(ext, "md" | "rs" | "toml" | "yml" | "yaml" | "json" | "sh");
        if !is_text {
            continue;
        }
        let content =
            fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;

        if rel == "src/store/mod.rs" {
            ensure(
                !content.contains("pub fn subscribe("),
                "structural-check: src/store/mod.rs still exports ambiguous `subscribe`",
            )?;
            ensure(
                !content.contains("pub fn cursor("),
                "structural-check: src/store/mod.rs still exports ambiguous `cursor`",
            )?;
        }

        if rel.starts_with("src/store/") {
            ensure(
                !content.contains("index.ckpt.tmp"),
                format!("structural-check: fixed checkpoint temp-file pattern found in {rel}"),
            )?;
            ensure(
                !content.contains(".tmp_{pid}_{n}"),
                format!("structural-check: fixed native-cache temp-file pattern found in {rel}"),
            )?;
        }

        if historical_allow.iter().any(|allowed| allowed == path) {
            continue;
        }

        ensure(
            !content.contains("test-support"),
            format!("structural-check: stale `test-support` reference found in {rel}"),
        )?;
    }
    Ok(())
}

fn check_boundary_scripts_only(repo_root: &Path, tracked_files: &[PathBuf]) -> Result<()> {
    let allowed = [
        "scripts/bootstrap.ps1",
        "scripts/bootstrap.sh",
        "scripts/run-in-devcontainer.sh",
    ];
    for path in tracked_files {
        let rel = relative(repo_root, path);
        if rel.starts_with("scripts/") {
            ensure(
                allowed.contains(&rel.as_str()),
                format!(
                    "scripts/ may only contain environment-bound wrappers; found non-boundary script {rel}"
                ),
            )?;
        }
    }
    Ok(())
}

fn check_removed_script_references(repo_root: &Path, tracked_files: &[PathBuf]) -> Result<()> {
    let removed = [
        "scripts/bench-report",
        "scripts/coverage-feedback",
        "scripts/verify-all.sh",
    ];
    let allow = [repo_root.join("tools/integrity/src/architecture_lints.rs")];
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
        for script in removed {
            ensure(
                !content.contains(script),
                format!(
                    "live surface {} still references removed script `{script}`",
                    relative(repo_root, path)
                ),
            )?;
        }
    }
    Ok(())
}

fn check_no_mdbook_dependency(repo_root: &Path) -> Result<()> {
    let mut files = vec![
        repo_root.join(".devcontainer/Dockerfile"),
        repo_root.join(".github/workflows/ci.yml"),
        repo_root.join(".github/workflows/perf.yml"),
        repo_root.join("README.md"),
        repo_root.join("GUIDE.md"),
        repo_root.join("REFERENCE.md"),
        repo_root.join("CONTRIBUTING.md"),
        repo_root.join("AGENTS.md"),
        repo_root.join("justfile"),
    ];
    files.extend(files_with_extension(
        &repo_root.join("tools/xtask/src"),
        "rs",
    ));
    files.extend(files_with_extension(
        &repo_root.join("tools/integrity/src"),
        "rs",
    ));
    for path in files {
        if !path.exists() {
            continue;
        }
        if path == repo_root.join("tools/integrity/src/architecture_lints.rs") {
            continue;
        }
        let content =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        ensure(
            !content.contains("mdbook"),
            format!(
                "live repo tooling still depends on mdbook in {}",
                relative(repo_root, &path)
            ),
        )?;
    }
    Ok(())
}

fn check_justfile_stays_thin(repo_root: &Path) -> Result<()> {
    let path = repo_root.join("justfile");
    let content = fs::read_to_string(&path).context("read justfile")?;
    for (index, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("set ") {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            ensure(
                trimmed.starts_with("cargo xtask ") || trimmed.starts_with("just "),
                format!(
                    "justfile command at line {} must stay a thin alias over cargo xtask or just",
                    index + 1
                ),
            )?;
        }
    }
    Ok(())
}

fn check_packaging_surface(repo_root: &Path) -> Result<()> {
    let cargo_toml = repo_root.join("Cargo.toml");
    let content = fs::read_to_string(&cargo_toml).context("read Cargo.toml")?;
    ensure(
        content.contains("readme = \"README.md\""),
        "Cargo.toml must keep README.md as the package readme",
    )?;
    ensure(
        !content.contains("\"guide/**\""),
        "Cargo.toml must not exclude removed guide/** paths",
    )?;
    for required in [
        "\"docs/**\"",
        "\"scripts/**\"",
        "\"tools/**\"",
        "\"traceability/**\"",
    ] {
        ensure(
            content.contains(required),
            format!("Cargo.toml exclude list must keep packaging boundary {required}"),
        )?;
    }
    Ok(())
}

fn check_xtask_surface_contract(repo_root: &Path) -> Result<()> {
    let xtask_main = repo_root.join("tools/xtask/src/main.rs");
    let coverage_rs = repo_root.join("tools/xtask/src/coverage.rs");
    let commands_rs = repo_root.join("tools/xtask/src/commands.rs");
    let devcontainer_rs = repo_root.join("tools/xtask/src/devcontainer.rs");
    let preflight_rs = repo_root.join("tools/xtask/src/preflight.rs");
    let util_rs = repo_root.join("tools/xtask/src/util.rs");
    let justfile = repo_root.join("justfile");
    let dockerfile = repo_root.join(".devcontainer/Dockerfile");
    let setup_devcontainer_action = repo_root.join(".github/actions/setup-devcontainer/action.yml");
    let run_in_devcontainer = repo_root.join("scripts/run-in-devcontainer.sh");

    let xtask_main_content = fs::read_to_string(&xtask_main).context("read xtask main")?;
    let coverage_content = fs::read_to_string(&coverage_rs).context("read xtask coverage")?;
    let commands_content = fs::read_to_string(&commands_rs).context("read xtask commands")?;
    let devcontainer_content =
        fs::read_to_string(&devcontainer_rs).context("read xtask devcontainer")?;
    let preflight_content = fs::read_to_string(&preflight_rs).context("read xtask preflight")?;
    let util_content = fs::read_to_string(&util_rs).context("read xtask util")?;
    let justfile_content = fs::read_to_string(&justfile).context("read justfile")?;
    let dockerfile_content =
        fs::read_to_string(&dockerfile).context("read devcontainer dockerfile")?;
    let setup_devcontainer_action_content = fs::read_to_string(&setup_devcontainer_action)
        .context("read setup-devcontainer composite action")?;
    let run_in_devcontainer_content =
        fs::read_to_string(&run_in_devcontainer).context("read run-in-devcontainer wrapper")?;

    ensure(
        !xtask_main_content.contains("BenchCompile"),
        "xtask must not reintroduce a separate BenchCompile CLI variant",
    )?;
    ensure(
        xtask_main_content.contains("InstallHooks")
            && xtask_main_content.contains("DevcontainerExec("),
        "xtask main must expose install-hooks and devcontainer-exec as first-class command surfaces",
    )?;
    ensure(
        justfile_content.contains("bench-compile:\n    cargo xtask bench --compile"),
        "justfile bench-compile recipe must remain a thin alias over `cargo xtask bench --compile`",
    )?;
    ensure(
        justfile_content.contains("install-hooks:\n    cargo xtask install-hooks"),
        "justfile install-hooks recipe must remain a thin alias over `cargo xtask install-hooks`",
    )?;
    ensure(
        coverage_content.contains("target/xtask-cover/last-run"),
        "xtask coverage artifacts must live under target/xtask-cover/last-run",
    )?;
    ensure(
        coverage_content.contains("batpak-xtask-cover-staging"),
        "xtask coverage must stage exports outside target so cargo-llvm-cov cleanup cannot delete them mid-run",
    )?;
    ensure(
        coverage_content.contains("\"nextest\",\n        \"--profile\",\n        \"ci\",")
            || coverage_content.contains("\"nextest\", \"--profile\", \"ci\","),
        "xtask coverage must use the ci nextest profile so slow compile-fail tests remain truthful under coverage",
    )?;
    ensure(
        coverage_content.contains("if !args.json {")
            && coverage_content
                .contains("println!(\"Running tests with coverage instrumentation...\");")
            && coverage_content
                .contains("println!(\"Coverage export written to {}\", coverage_json.display());"),
        "xtask coverage banners must stay out of JSON mode",
    )?;
    ensure(
        coverage_content.contains("print!(\"{json_text}\");"),
        "xtask coverage JSON mode must print only the exported JSON payload",
    )?;
    ensure(
        coverage_content.contains("command.stdout(Stdio::null())")
            && coverage_content.contains("command.stderr(Stdio::inherit())"),
        "xtask coverage JSON mode must suppress stdout and keep stderr visible",
    )?;
    ensure(
        util_content.contains("stdout(Stdio::null())")
            && util_content.contains("stderr(Stdio::null())"),
        "xtask command probes must stay silent so `cargo xtask cover --json` remains stdout-clean",
    )?;
    ensure(
        !coverage_content.contains("cleanup_export_dir"),
        "xtask coverage must retain artifacts instead of deleting them eagerly",
    )?;
    ensure(
        preflight_content.contains("std::env::var_os(\"DEVCONTAINER\")")
            && preflight_content.contains("crate::commands::ci()?")
            && preflight_content.contains("coverage::cover(CoverArgs")
            && preflight_content.contains("crate::docs::docs(DocsArgs { open: false })"),
        "xtask preflight must collapse the proof chain into one in-container execution path",
    )?;
    ensure(
        preflight_content.contains("run_in_devcontainer(&[\"cargo\", \"xtask\", \"preflight\"])"),
        "xtask preflight must re-enter the canonical devcontainer only once",
    )?;
    ensure(
        commands_content.contains("cargo xtask install-hooks")
            && commands_content.contains(".githooks/pre-commit"),
        "xtask commands must own the tracked git-hook surface and surface install guidance",
    )?;
    ensure(
        devcontainer_content.contains("io.batpak.devcontainer-hash")
            && devcontainer_content.contains("Reusing local devcontainer image")
            && devcontainer_content.contains("\"PROPTEST_CASES\"")
            && devcontainer_content.contains("\"CHAOS_ITERATIONS\"")
            && devcontainer_content.contains("bash")
            && devcontainer_content.contains("-lc"),
        "xtask devcontainer logic must own image reuse, env forwarding, and single-string shell compatibility",
    )?;
    ensure(
        setup_devcontainer_action_content.contains("dtolnay/rust-toolchain@stable"),
        "setup-devcontainer action must install a host Rust toolchain so the thin wrapper can delegate to cargo xtask",
    )?;
    ensure(
        dockerfile_content.contains("ENV PATH=\"/usr/local/cargo/bin:${PATH}\"")
            && dockerfile_content.contains("install-from-binstall-release.sh")
            && dockerfile_content.contains("cargo binstall --no-confirm cargo-deny@0.19.0")
            && dockerfile_content.contains("cargo binstall --no-confirm cargo-llvm-cov@0.8.5"),
        "devcontainer bootstrap must expose cargo on PATH and prefer binstall for pinned cargo helper tools before falling back to source builds",
    )?;
    ensure(
        dockerfile_content.contains("cargo install --locked cargo-mutants@27.0.0"),
        "devcontainer bootstrap must source-install cargo-mutants on bookworm because the published GNU binary is not glibc-compatible there",
    )?;
    ensure(
        run_in_devcontainer_content.contains("cargo xtask devcontainer-exec -- \"$@\"")
            && !run_in_devcontainer_content.contains("docker build")
            && !run_in_devcontainer_content.contains("image_hash_label"),
        "run-in-devcontainer.sh must stay a thin compatibility wrapper over xtask-owned devcontainer logic",
    )?;
    Ok(())
}

fn check_portable_context_links(repo_root: &Path) -> Result<()> {
    let readme = repo_root.join("README.md");
    let guide = repo_root.join("GUIDE.md");
    let reference = repo_root.join("REFERENCE.md");

    let readme_links = markdown_links(repo_root, &readme)?;
    ensure(
        readme_links.contains("GUIDE.md"),
        "README.md must link to GUIDE.md",
    )?;
    ensure(
        readme_links.contains("REFERENCE.md"),
        "README.md must link to REFERENCE.md",
    )?;

    let guide_links = markdown_links(repo_root, &guide)?;
    ensure(
        guide_links.contains("README.md"),
        "GUIDE.md must link back to README.md",
    )?;
    ensure(
        guide_links.contains("REFERENCE.md"),
        "GUIDE.md must link to REFERENCE.md",
    )?;

    let reference_links = markdown_links(repo_root, &reference)?;
    ensure(
        reference_links.contains("README.md"),
        "REFERENCE.md must link back to README.md",
    )?;
    ensure(
        reference_links.contains("GUIDE.md"),
        "REFERENCE.md must link to GUIDE.md",
    )?;

    Ok(())
}

fn check_live_docs_do_not_link_archives(repo_root: &Path) -> Result<()> {
    let files = vec![
        repo_root.join("README.md"),
        repo_root.join("GUIDE.md"),
        repo_root.join("REFERENCE.md"),
        repo_root.join("CONTRIBUTING.md"),
    ];
    for path in files {
        let rel = relative(repo_root, &path);
        for link in markdown_links(repo_root, &path)? {
            ensure(
                !link.starts_with("docs/archive/"),
                format!("live doc {rel} links archive material as if it were live: {link}"),
            )?;
        }
    }
    Ok(())
}

fn check_root_doc_site_contract(repo_root: &Path) -> Result<()> {
    let docs_rs = repo_root.join("tools/xtask/src/docs.rs");
    let content = fs::read_to_string(&docs_rs).context("read tools/xtask/src/docs.rs")?;
    for (source, rendered) in [
        ("README.md", "README.html"),
        ("GUIDE.md", "GUIDE.html"),
        ("REFERENCE.md", "REFERENCE.html"),
    ] {
        ensure(
            content.contains(source),
            format!("xtask docs surface must render canonical root doc {source}"),
        )?;
        ensure(
            content.contains(rendered),
            format!("xtask docs surface must emit canonical page {rendered}"),
        )?;
    }
    ensure(
        content.contains("api/batpak/"),
        "xtask docs surface must expose rustdoc API under api/batpak/",
    )?;
    ensure(
        !content.contains("mdbook"),
        "xtask docs surface must not depend on mdbook",
    )?;
    Ok(())
}

fn check_public_surface_truth(repo_root: &Path) -> Result<()> {
    let store_mod = parse_rust(repo_root.join("src/store/mod.rs"))?;
    let prelude = parse_rust(repo_root.join("src/prelude.rs"))?;
    let event_mod = parse_rust(repo_root.join("src/event/mod.rs"))?;
    let event_sourcing = parse_rust(repo_root.join("src/event/sourcing.rs"))?;
    let config = parse_rust(repo_root.join("src/store/config.rs"))?;

    let store_exports = public_use_names(&store_mod);
    ensure(
        store_exports.contains("IndexTopology"),
        "src/store/mod.rs must re-export IndexTopology",
    )?;
    ensure(
        !store_exports.contains("IndexLayout"),
        "src/store/mod.rs still re-exports removed IndexLayout",
    )?;
    ensure(
        !store_exports.contains("ViewConfig"),
        "src/store/mod.rs still re-exports removed ViewConfig",
    )?;

    let prelude_exports = public_use_names(&prelude);
    for required in ["IndexTopology", "ReplayLane", "JsonValueInput"] {
        ensure(
            prelude_exports.contains(required),
            format!("src/prelude.rs must re-export {required}"),
        )?;
    }
    for banned in ["IndexLayout", "ViewConfig", "ProjectionMode", "ValueInput"] {
        ensure(
            !prelude_exports.contains(banned),
            format!("src/prelude.rs still exposes removed public name {banned}"),
        )?;
    }

    let event_exports = public_use_names(&event_mod);
    for required in ["ReplayLane", "JsonValueInput", "RawMsgpackInput"] {
        ensure(
            event_exports.contains(required),
            format!("src/event/mod.rs must re-export {required}"),
        )?;
    }
    for banned in ["ProjectionMode", "ValueInput"] {
        ensure(
            !event_exports.contains(banned),
            format!("src/event/mod.rs still exposes removed replay name {banned}"),
        )?;
    }

    let config_types = public_item_names(&config);
    ensure(
        config_types.contains("IndexTopology"),
        "src/store/config.rs must define IndexTopology as a live public type",
    )?;
    for banned in ["IndexLayout", "ViewConfig"] {
        ensure(
            !config_types.contains(banned),
            format!("src/store/config.rs still defines removed topology name {banned}"),
        )?;
    }

    let config_methods = public_impl_method_names(&config, "StoreConfig");
    ensure(
        config_methods.contains("with_index_topology"),
        "StoreConfig must expose with_index_topology",
    )?;
    for banned in ["with_index_layout", "with_views"] {
        ensure(
            !config_methods.contains(banned),
            format!("StoreConfig still exposes removed builder {banned}"),
        )?;
    }

    let replay_items = public_item_names(&event_sourcing);
    for required in ["ReplayLane", "JsonValueInput", "RawMsgpackInput"] {
        ensure(
            replay_items.contains(required),
            format!("src/event/sourcing.rs must define {required}"),
        )?;
    }
    for banned in ["ProjectionMode", "ValueInput"] {
        ensure(
            !replay_items.contains(banned),
            format!("src/event/sourcing.rs still defines removed replay type {banned}"),
        )?;
    }

    let topology_constructors = public_impl_method_names(&config, "IndexTopology");
    for required in ["aos", "scan", "entity_local", "tiled", "all"] {
        ensure(
            topology_constructors.contains(required),
            format!("IndexTopology must expose constructor `{required}`"),
        )?;
    }
    let topology_variants = public_struct_field_names(&config, "IndexTopology");
    for required in ["soa", "entity_groups", "tiles64"] {
        ensure(
            topology_variants.contains(required),
            format!("IndexTopology must expose field `{required}`"),
        )?;
    }
    let topology_default = default_impl_return_target(&config, "IndexTopology");
    ensure(
        matches!(
            topology_default.as_deref(),
            Some("Self::aos") | Some("IndexTopology::aos")
        ),
        "IndexTopology::default() must delegate to aos() so overlays stay opt-in",
    )?;

    let replay_variants = public_enum_variant_names(&event_sourcing, "ReplayLane");
    ensure(
        replay_variants == BTreeSet::from(["RawMsgpack".to_string(), "Value".to_string()]),
        format!(
            "ReplayLane must contain exactly {{Value, RawMsgpack}}, found {:?}",
            replay_variants
        ),
    )?;
    ensure(
        impl_const_expr_target(&event_sourcing, "JsonValueInput", "MODE").as_deref()
            == Some("ReplayLane::Value"),
        "JsonValueInput::MODE must map to ReplayLane::Value",
    )?;
    ensure(
        impl_const_expr_target(&event_sourcing, "RawMsgpackInput", "MODE").as_deref()
            == Some("ReplayLane::RawMsgpack"),
        "RawMsgpackInput::MODE must map to ReplayLane::RawMsgpack",
    )?;

    Ok(())
}

fn parse_rust(path: PathBuf) -> Result<syn::File> {
    let content = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    syn::parse_file(&content).with_context(|| format!("parse {}", path.display()))
}

fn public_item_names(file: &syn::File) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for item in &file.items {
        match item {
            Item::Struct(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.insert(item.ident.to_string());
            }
            Item::Enum(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.insert(item.ident.to_string());
            }
            Item::Trait(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.insert(item.ident.to_string());
            }
            Item::Type(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.insert(item.ident.to_string());
            }
            Item::Const(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.insert(item.ident.to_string());
            }
            Item::Fn(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.insert(item.sig.ident.to_string());
            }
            _ => {}
        }
    }
    names
}

fn public_impl_method_names(file: &syn::File, type_name: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for item in &file.items {
        let Item::Impl(impl_block) = item else {
            continue;
        };
        if impl_block.trait_.is_some() {
            continue;
        }
        let is_target_impl = match impl_block.self_ty.as_ref() {
            syn::Type::Path(tp) => tp
                .path
                .segments
                .last()
                .map(|segment| segment.ident == type_name)
                .unwrap_or(false),
            _ => false,
        };
        if !is_target_impl {
            continue;
        }
        for impl_item in &impl_block.items {
            if let syn::ImplItem::Fn(method) = impl_item {
                if matches!(method.vis, syn::Visibility::Public(_)) {
                    names.insert(method.sig.ident.to_string());
                }
            }
        }
    }
    names
}

fn public_struct_field_names(file: &syn::File, struct_name: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for item in &file.items {
        let Item::Struct(item_struct) = item else {
            continue;
        };
        if item_struct.ident != struct_name {
            continue;
        }
        if !matches!(item_struct.vis, syn::Visibility::Public(_)) {
            continue;
        }
        if let syn::Fields::Named(fields) = &item_struct.fields {
            for field in &fields.named {
                if matches!(field.vis, syn::Visibility::Public(_)) {
                    if let Some(ident) = &field.ident {
                        names.insert(ident.to_string());
                    }
                }
            }
        }
    }
    names
}

fn public_enum_variant_names(file: &syn::File, enum_name: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for item in &file.items {
        let Item::Enum(item_enum) = item else {
            continue;
        };
        if item_enum.ident != enum_name || !matches!(item_enum.vis, syn::Visibility::Public(_)) {
            continue;
        }
        for variant in &item_enum.variants {
            names.insert(variant.ident.to_string());
        }
    }
    names
}

fn default_impl_return_target(file: &syn::File, type_name: &str) -> Option<String> {
    for item in &file.items {
        let Item::Impl(impl_block) = item else {
            continue;
        };
        let Some((_, trait_path, _)) = &impl_block.trait_ else {
            continue;
        };
        let is_default_impl = trait_path
            .segments
            .last()
            .map(|segment| segment.ident == "Default")
            .unwrap_or(false);
        if !is_default_impl || !self_ty_is(impl_block.self_ty.as_ref(), type_name) {
            continue;
        }
        for impl_item in &impl_block.items {
            let syn::ImplItem::Fn(method) = impl_item else {
                continue;
            };
            if method.sig.ident != "default" {
                continue;
            }
            if let Some(target) = trailing_expr_target(&method.block) {
                return Some(target);
            }
        }
    }
    None
}

fn impl_const_expr_target(file: &syn::File, type_name: &str, const_name: &str) -> Option<String> {
    for item in &file.items {
        let Item::Impl(impl_block) = item else {
            continue;
        };
        if impl_block.trait_.is_none() || !self_ty_is(impl_block.self_ty.as_ref(), type_name) {
            continue;
        }
        for impl_item in &impl_block.items {
            let syn::ImplItem::Const(item_const) = impl_item else {
                continue;
            };
            if item_const.ident == const_name {
                return expr_target(&item_const.expr);
            }
        }
    }
    None
}

fn self_ty_is(ty: &syn::Type, type_name: &str) -> bool {
    match ty {
        syn::Type::Path(tp) => tp
            .path
            .segments
            .last()
            .map(|segment| segment.ident == type_name)
            .unwrap_or(false),
        _ => false,
    }
}

fn trailing_expr_target(block: &syn::Block) -> Option<String> {
    let stmt = block.stmts.last()?;
    match stmt {
        syn::Stmt::Expr(expr, _) => expr_target(expr),
        _ => None,
    }
}

fn expr_target(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Call(call) => expr_target(&call.func),
        syn::Expr::Path(path) => Some(path_to_string(&path.path)),
        _ => None,
    }
}

fn path_to_string(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

fn public_use_names(file: &syn::File) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for item in &file.items {
        if let Item::Use(item_use) = item {
            if matches!(item_use.vis, syn::Visibility::Public(_)) {
                collect_use_tree_names(&item_use.tree, &mut names);
            }
        }
    }
    names
}

fn collect_use_tree_names(tree: &UseTree, names: &mut BTreeSet<String>) {
    match tree {
        UseTree::Path(path) => collect_use_tree_names(&path.tree, names),
        UseTree::Name(name) => {
            names.insert(name.ident.to_string());
        }
        UseTree::Rename(rename) => {
            names.insert(rename.rename.to_string());
        }
        UseTree::Group(group) => {
            for item in &group.items {
                collect_use_tree_names(item, names);
            }
        }
        UseTree::Glob(_) => {}
    }
}

fn markdown_links(repo_root: &Path, path: &Path) -> Result<BTreeSet<String>> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let parser = Parser::new_ext(&content, Options::all());
    let mut links = BTreeSet::new();
    for event in parser {
        if let Event::Start(Tag::Link { dest_url, .. }) = event {
            let Some(link) = resolve_link(repo_root, path, dest_url.as_ref()) else {
                continue;
            };
            links.insert(link);
        }
    }
    Ok(links)
}

fn resolve_link(repo_root: &Path, source: &Path, raw_link: &str) -> Option<String> {
    if raw_link.starts_with("http://")
        || raw_link.starts_with("https://")
        || raw_link.starts_with("mailto:")
    {
        return None;
    }
    let path_part = raw_link.split('#').next()?.trim();
    if path_part.is_empty() {
        return None;
    }
    let source_rel = source.strip_prefix(repo_root).ok()?;
    let base = source_rel.parent().unwrap_or(Path::new(""));
    Some(normalize_repo_path(&base.join(path_part)))
}

fn files_with_extension(root: &Path, extension: &str) -> Vec<PathBuf> {
    if !root.exists() {
        return Vec::new();
    }
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some(extension))
        .map(|entry| entry.into_path())
        .collect()
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn normalize_repo_path(path: &Path) -> String {
    let mut normalized = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part.to_string_lossy().into_owned()),
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    normalized.join("/")
}

fn ensure(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(anyhow!(message.into()))
    }
}
