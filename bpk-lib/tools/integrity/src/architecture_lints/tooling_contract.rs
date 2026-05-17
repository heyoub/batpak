use super::{ensure, relative};
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn check(repo_root: &Path) -> Result<()> {
    check_project_layout_contract(repo_root)?;
    check_no_mdbook_dependency(repo_root)?;
    check_testing_doc_renames_stay_current(repo_root)?;
    check_justfile_stays_thin(repo_root)?;
    check_packaging_surface(repo_root)?;
    check_single_target_dir_contract(repo_root)?;
    check_crate_layout_contract(repo_root)?;
    check_default_feature_surface(repo_root)?;
    check_xtask_surface_contract(repo_root)?;
    check_syncbat_is_explicitly_gated(repo_root)?;
    Ok(())
}

fn check_project_layout_contract(repo_root: &Path) -> Result<()> {
    let project_root = project_root(repo_root);
    let gitignore =
        fs::read_to_string(project_root.join(".gitignore")).context("read .gitignore")?;

    ensure(
        repo_root.file_name().and_then(|name| name.to_str()) == Some("bpk-lib"),
        "Cargo workspace root must stay at project-root/bpk-lib",
    )?;

    for path in [
        "000_REPO_MAP.md",
        "001_BATPAK_SUBSTRATE.md",
        "002_SYNCBAT_RUNTIME.md",
        "003_DownstreamKit_KIT.md",
        "004_NETBAT_NETWORK.md",
        "010_USER_GUIDE.md",
        "020_TECHNICAL_REFERENCE.md",
        "099_DECISION_INDEX.md",
        "cookbook",
        "bpk-lib/Cargo.toml",
        "bpk-lib/.cargo/config.toml",
        "bpk-lib/.config/nextest.toml",
    ] {
        ensure(
            project_root.join(path).exists(),
            format!("project layout contract requires `{path}`"),
        )?;
    }

    for path in [
        "Cargo.toml",
        "Cargo.lock",
        ".cargo",
        ".config",
        "crates",
        "tools",
        "templates",
        "traceability",
    ] {
        ensure(
            !project_root.join(path).exists(),
            format!("`{path}` belongs under bpk-lib/ in this repo layout"),
        )?;
    }

    ensure(
        gitignore.contains("bpk-lib/templates/*/Cargo.lock")
            && gitignore.contains("bpk-lib/crates/core/fixtures/*/Cargo.lock")
            && !gitignore.contains("\ntemplates/*/Cargo.lock")
            && !gitignore.contains("\nfixtures/*/Cargo.lock"),
        ".gitignore must ignore generated lockfiles at their bpk-lib paths, not stale root paths",
    )?;

    for entry in walkdir::WalkDir::new(repo_root.join("templates"))
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
    {
        ensure(
            entry.file_name() != "Cargo.lock",
            format!(
                "template lockfile `{}` is generated cache; do not track it",
                relative(repo_root, entry.path())
            ),
        )?;
    }

    Ok(())
}

fn check_testing_doc_renames_stay_current(repo_root: &Path) -> Result<()> {
    let harness_lints = fs::read_to_string(repo_root.join("tools/integrity/src/harness_lints.rs"))
        .context("read harness_lints.rs")?;
    ensure(
        !harness_lints.contains("HARNESS_LEDGER.md"),
        "harness lint diagnostics must name 041_TESTING_LEDGER.md, not retired HARNESS_LEDGER.md",
    )?;

    let docs_rs =
        fs::read_to_string(repo_root.join("tools/xtask/src/docs.rs")).context("read docs.rs")?;
    ensure(
        !docs_rs.contains("HARNESS_DIRECTIVE.html")
            && !docs_rs.contains("HARNESS_LEDGER.html")
            && docs_rs.contains("TESTING_DOCTRINE.html")
            && docs_rs.contains("TESTING_LEDGER.html"),
        "generated docs must use TESTING_DOCTRINE.html and TESTING_LEDGER.html names",
    )?;
    Ok(())
}

fn check_no_mdbook_dependency(repo_root: &Path) -> Result<()> {
    let project_root = project_root(repo_root);
    let mut files = vec![
        project_root.join(".devcontainer/Dockerfile"),
        project_root.join(".github/workflows/ci.yml"),
        project_root.join(".github/workflows/perf.yml"),
        project_root.join("000_REPO_MAP.md"),
        project_root.join("README.md"),
        project_root.join("010_USER_GUIDE.md"),
        project_root.join("020_TECHNICAL_REFERENCE.md"),
        project_root.join("060_CONTRIBUTING.md"),
        project_root.join("AGENTS.md"),
        project_root.join("justfile"),
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
    let path = project_root(repo_root).join("justfile");
    let content = fs::read_to_string(&path).context("read justfile")?;
    for (index, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("set ") {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            ensure(
                trimmed.starts_with("cd bpk-lib && cargo xtask ")
                    || trimmed.starts_with("cargo xtask ")
                    || trimmed.starts_with("just "),
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
    let workspace_toml = repo_root.join("Cargo.toml");
    let workspace = fs::read_to_string(&workspace_toml).context("read Cargo.toml")?;
    ensure(
        workspace.contains("\"crates/core\""),
        "workspace Cargo.toml must keep crates/core as the primary batpak package member",
    )?;
    ensure(
        workspace.contains("default-members = [\"crates/core\"]"),
        "workspace Cargo.toml must default to the primary batpak package",
    )?;

    let package_toml = repo_root.join("crates/core/Cargo.toml");
    let package = fs::read_to_string(&package_toml).context("read crates/core/Cargo.toml")?;
    ensure(
        package.contains("readme = \"../../../README.md\""),
        "crates/core/Cargo.toml must keep the project-root README.md as the package readme",
    )?;
    ensure(
        !package.contains("\"guide/**\""),
        "crates/core/Cargo.toml must not exclude removed guide/** paths",
    )?;
    ensure(
        !package.contains("\"tools/integrity/**\"") && !package.contains("\"tools/xtask/**\""),
        "crates/core/Cargo.toml package boundary is physical; root tool paths must not be encoded as package excludes",
    )?;
    Ok(())
}

fn check_single_target_dir_contract(repo_root: &Path) -> Result<()> {
    let project_root = project_root(repo_root);
    let cargo_config = fs::read_to_string(repo_root.join(".cargo/config.toml"))
        .context("read .cargo/config.toml")?;
    ensure(
        cargo_config.contains("target-dir = \"../target\""),
        "bpk-lib/.cargo/config.toml must route all default Cargo output to repo-root target/",
    )?;

    let nextest_config = fs::read_to_string(repo_root.join(".config/nextest.toml"))
        .context("read .config/nextest.toml")?;
    ensure(
        nextest_config.contains("dir = \"../target/nextest\"")
            && nextest_config.contains("path = \"junit.xml\"")
            && !nextest_config.contains("path = \"target/")
            && !nextest_config.contains("path = \"../target/"),
        "nextest reports and run metadata must stay under repo-root target/nextest",
    )?;

    let devcontainer = fs::read_to_string(project_root.join(".devcontainer/devcontainer.json"))
        .context("read devcontainer.json")?;
    ensure(
        devcontainer.contains("\"CARGO_TARGET_DIR\": \"/workspace/batpak/target\""),
        "devcontainer must keep Cargo output under repo-root target/",
    )?;

    let ci = fs::read_to_string(project_root.join(".github/workflows/ci.yml"))
        .context("read ci workflow")?;
    ensure(
        ci.contains("path: target/site") && !ci.contains("bpk-lib/target"),
        "CI artifact paths must use repo-root target/, not bpk-lib/target/",
    )?;

    let perf = fs::read_to_string(project_root.join(".github/workflows/perf.yml"))
        .context("read perf workflow")?;
    ensure(
        perf.contains("path: target/criterion") && !perf.contains("bpk-lib/target"),
        "perf artifact paths must use repo-root target/, not bpk-lib/target/",
    )?;

    for entry in walkdir::WalkDir::new(repo_root)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_dir())
    {
        if entry.file_name() == "target" {
            let rel = relative(repo_root, entry.path());
            let mut children = fs::read_dir(entry.path())
                .with_context(|| format!("read nested target dir `{rel}`"))?;
            ensure(
                children.next().is_none(),
                format!(
                    "nested Cargo target dir `{rel}` contains artifacts; use repo-root target/ only"
                ),
            )?;
        }
    }

    Ok(())
}

fn check_crate_layout_contract(repo_root: &Path) -> Result<()> {
    for dir in [
        "crates/core/src",
        "crates/core/tests",
        "crates/core/examples",
        "crates/core/benches",
        "crates/core/fixtures",
        "crates/syncbat/src",
        "crates/syncbat/tests",
        "crates/downstream-kit/src",
        "crates/downstream-kit/tests",
        "crates/netbat/src",
        "crates/netbat/tests",
        "crates/macros/src",
        "crates/macros-support/src",
        "crates/syncbat-macros/src",
        "crates/bench-support/src",
    ] {
        ensure(
            repo_root.join(dir).is_dir(),
            format!("crate layout contract requires `{dir}`"),
        )?;
    }

    for dir in ["tests", "examples", "benches", "fixtures"] {
        ensure(
            !repo_root.join(dir).exists(),
            format!(
                "workspace-root `{dir}` is ambiguous; put package-owned surfaces under the owning crate"
            ),
        )?;
    }

    for crate_name in ["syncbat", "downstream-kit", "netbat"] {
        let crate_root = repo_root.join("crates").join(crate_name);
        for dir in ["examples", "benches", "fixtures"] {
            ensure(
                !crate_root.join(dir).exists(),
                format!(
                    "`crates/{crate_name}/{dir}` would blur ownership; companion crate demos belong in root docs/cookbook or explicit tests"
                ),
            )?;
        }
    }

    for (macro_crate, owner) in [
        ("crates/macros", "crates/core/tests/ui"),
        ("crates/syncbat-macros", "crates/syncbat/tests/ui"),
    ] {
        ensure(
            repo_root.join(owner).is_dir(),
            format!(
                "`{macro_crate}` does not need its own integration-test folder, but `{owner}` must exist as its compile-fail owner"
            ),
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
    let project_root = project_root(repo_root);
    let xtask_main = repo_root.join("tools/xtask/src/main.rs");
    let coverage_rs = repo_root.join("tools/xtask/src/coverage.rs");
    let commands_rs = repo_root.join("tools/xtask/src/commands.rs");
    let devcontainer_rs = repo_root.join("tools/xtask/src/devcontainer.rs");
    let preflight_rs = repo_root.join("tools/xtask/src/preflight.rs");
    let util_rs = repo_root.join("tools/xtask/src/util.rs");
    let justfile = project_root.join("justfile");
    let dockerfile = project_root.join(".devcontainer/Dockerfile");
    let setup_devcontainer_action =
        project_root.join(".github/actions/setup-devcontainer/action.yml");
    let run_in_devcontainer = project_root.join("scripts/run-in-devcontainer.sh");

    let xtask_main_content = fs::read_to_string(&xtask_main).context("read xtask main")?;
    let coverage_content = fs::read_to_string(&coverage_rs).context("read xtask coverage")?;
    let commands_content = fs::read_to_string(&commands_rs).context("read xtask commands")?;
    let devcontainer_content =
        fs::read_to_string(&devcontainer_rs).context("read xtask devcontainer")?;
    let preflight_content = fs::read_to_string(&preflight_rs).context("read xtask preflight")?;
    let util_content = fs::read_to_string(&util_rs).context("read xtask util")?;
    let justfile_content = fs::read_to_string(&justfile).context("read justfile")?;
    let agents_content =
        fs::read_to_string(project_root.join("AGENTS.md")).context("read AGENTS.md")?;
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
        justfile_content.contains("bench-compile:\n    cd bpk-lib && cargo xtask bench --compile"),
        "justfile bench-compile recipe must remain a thin alias over `cd bpk-lib && cargo xtask bench --compile`",
    )?;
    ensure(
        justfile_content.contains("install-hooks:\n    cd bpk-lib && cargo xtask install-hooks"),
        "justfile install-hooks recipe must remain a thin alias over `cd bpk-lib && cargo xtask install-hooks`",
    )?;
    ensure(
        justfile_content.contains("stress:\n    cd bpk-lib && cargo xtask stress"),
        "justfile stress recipe must remain a thin alias over `cd bpk-lib && cargo xtask stress`",
    )?;
    for command in [
        XtaskDocCommand {
            command: "layout",
            variant: "Layout",
        },
        XtaskDocCommand {
            command: "boundary",
            variant: "Boundary",
        },
        XtaskDocCommand {
            command: "stale-paths",
            variant: "StalePaths",
        },
        XtaskDocCommand {
            command: "disk-audit",
            variant: "DiskAudit",
        },
        XtaskDocCommand {
            command: "clean-generated",
            variant: "CleanGenerated",
        },
        XtaskDocCommand {
            command: "package-leak-scan",
            variant: "PackageLeakScan",
        },
        XtaskDocCommand {
            command: "template-freshness",
            variant: "TemplateFreshness",
        },
    ] {
        ensure(
            xtask_main_content.contains(command.variant),
            format!(
                "xtask main must expose `{}` as variant `{}`",
                command.command, command.variant
            ),
        )?;
        ensure(
            justfile_content.contains(&format!("cargo xtask {}", command.command)),
            format!(
                "justfile must expose a thin alias for `cargo xtask {}`",
                command.command
            ),
        )?;
        ensure(
            agents_content.contains(&format!("cargo xtask {}", command.command)),
            format!(
                "AGENTS.md must list canonical command `cargo xtask {}`",
                command.command
            ),
        )?;
    }
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
            && run_in_devcontainer_content.contains("cd \"${repo_root}/bpk-lib\"")
            && !run_in_devcontainer_content.contains("docker build")
            && !run_in_devcontainer_content.contains("image_hash_label"),
        "run-in-devcontainer.sh must stay a thin compatibility wrapper over xtask-owned devcontainer logic",
    )?;
    Ok(())
}

struct XtaskDocCommand {
    command: &'static str,
    variant: &'static str,
}

fn project_root(repo_root: &Path) -> &Path {
    repo_root.parent().unwrap_or(repo_root)
}

fn check_syncbat_is_explicitly_gated(repo_root: &Path) -> Result<()> {
    let workspace_toml =
        fs::read_to_string(repo_root.join("Cargo.toml")).context("read workspace Cargo.toml")?;
    let family_crates = [
        ("syncbat", "\"crates/syncbat\""),
        ("downstream-kit", "\"crates/downstream-kit\""),
        ("netbat", "\"crates/netbat\""),
    ];

    let xtask_main =
        fs::read_to_string(repo_root.join("tools/xtask/src/main.rs")).context("read xtask main")?;
    let ci_rs =
        fs::read_to_string(repo_root.join("tools/xtask/src/commands/ci.rs")).context("read ci")?;
    let coverage_rs = fs::read_to_string(repo_root.join("tools/xtask/src/coverage.rs"))
        .context("read coverage")?;
    let docs_rs =
        fs::read_to_string(repo_root.join("tools/xtask/src/docs.rs")).context("read docs")?;

    let active_family_crates = family_crates
        .iter()
        .filter_map(|(package, manifest_entry)| {
            workspace_toml.contains(manifest_entry).then_some(*package)
        })
        .collect::<Vec<_>>();

    if active_family_crates.is_empty() {
        return Ok(());
    }

    for (label, content) in [
        ("tools/xtask/src/main.rs", xtask_main),
        ("tools/xtask/src/commands/ci.rs", ci_rs),
    ] {
        for package in &active_family_crates {
            ensure(
                content.contains(&format!("\"{package}\""))
                    && content.contains("\"check\"")
                    && content.contains("\"test\"")
                    && content.contains("\"clippy\""),
                format!(
                    "{label} must explicitly gate {package} with check, test, and clippy while default-members stays core-only"
                ),
            )?;
        }
    }

    for package in &active_family_crates {
        ensure(
            coverage_rs.contains(&format!("\"{package}\"")),
            format!("tools/xtask/src/coverage.rs must include {package} while default-members stays core-only"),
        )?;
        ensure(
            docs_rs.contains(&format!("\"{package}\"")),
            format!("tools/xtask/src/docs.rs must include {package} while default-members stays core-only"),
        )?;
    }

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
