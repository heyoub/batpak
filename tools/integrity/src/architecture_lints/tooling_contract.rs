use super::{ensure, relative};
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn check(repo_root: &Path) -> Result<()> {
    check_no_mdbook_dependency(repo_root)?;
    check_justfile_stays_thin(repo_root)?;
    check_packaging_surface(repo_root)?;
    check_default_feature_surface(repo_root)?;
    check_xtask_surface_contract(repo_root)?;
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
        let rel = relative(repo_root, &path);
        if rel.starts_with("tools/integrity/src/architecture_lints/") {
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
        "\"scripts/**\"",
        "\"tools/integrity/**\"",
        "\"tools/xtask/**\"",
    ] {
        ensure(
            content.contains(required),
            format!("Cargo.toml exclude list must keep packaging boundary {required}"),
        )?;
    }
    Ok(())
}

fn check_default_feature_surface(repo_root: &Path) -> Result<()> {
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(repo_root.join("Cargo.toml"))
        .no_deps()
        .exec()
        .context("read Cargo metadata for default feature surface")?;
    let package = metadata
        .packages
        .iter()
        .find(|package| package.name == "batpak")
        .context("Cargo metadata must contain root batpak package")?;
    let default_features = package
        .features
        .get("default")
        .context("root batpak package must declare a default feature set")?;
    ensure(
        !default_features
            .iter()
            .any(|feature| feature == "dangerous-test-hooks"),
        "Cargo.toml default features must not include dangerous-test-hooks; test/fault APIs must stay opt-in",
    )
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
        justfile_content.contains("stress:\n    cargo xtask stress"),
        "justfile stress recipe must remain a thin alias over `cargo xtask stress`",
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
        coverage_content.contains("\"LLVM_PROFILE_FILE\"")
            && coverage_content.contains("coverage_profraw_dir"),
        "xtask coverage must route raw LLVM profiles into a dedicated staging directory instead of spraying .profraw files into the repo root",
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
            && devcontainer_content.contains("OsString::from(\"-c\")")
            && devcontainer_content.contains("Avoid a login shell here"),
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
