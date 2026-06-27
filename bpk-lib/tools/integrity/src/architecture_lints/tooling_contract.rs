use super::{ensure, relative};
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn check(repo_root: &Path) -> Result<()> {
    check_project_layout_contract(repo_root)?;
    check_supply_chain_boundary(repo_root)?;
    check_no_mdbook_dependency(repo_root)?;
    check_testing_doc_renames_stay_current(repo_root)?;
    check_justfile_stays_thin(repo_root)?;
    check_packaging_surface(repo_root)?;
    check_single_target_dir_contract(repo_root)?;
    check_crate_layout_contract(repo_root)?;
    check_default_feature_surface(repo_root)?;
    check_core_feature_cfg_contract(repo_root)?;
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
        "README.md",
        "01_FACTORY.md",
        "02_MODEL.md",
        "03_INVARIANTS.md",
        "04_BATTERIES.md",
        "05_TERMINALS.md",
        "06_EVENTS.md",
        "07_RECEIPTS.md",
        "08_CIRCUITS.md",
        "09_REPLAY.md",
        "10_PROJECTIONS.md",
        "11_INTEGRATION.md",
        "12_CONFORMANCE.md",
        "CONTRIBUTING.md",
        "archive/decisions/099_DECISION_INDEX.md",
        "bpk-lib/traceability/testing_ledger.yaml",
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
    check_root_markdown_allowlist(project_root)?;
    check_no_unprefixed_factory_docs(project_root)?;
    ensure(
        !project_root.join("deny.toml").exists(),
        "deny.toml belongs under bpk-lib/ (cargo deny runs from the workspace root)",
    )?;
    ensure(
        !project_root.join("clippy.toml").exists(),
        "clippy.toml belongs under bpk-lib/ (Cargo workspace root)",
    )?;
    ensure(
        repo_root.join("deny.toml").is_file(),
        "project layout contract requires bpk-lib/deny.toml",
    )?;
    ensure(
        repo_root.join("clippy.toml").is_file(),
        "project layout contract requires bpk-lib/clippy.toml",
    )?;
    ensure(
        !project_root.join("sgconfig.yml").exists(),
        "sgconfig.yml belongs under bpk-lib/tools/ast-grep/",
    )?;
    ensure(
        !project_root.join("ast-grep").exists(),
        "ast-grep calipers belong under bpk-lib/tools/ast-grep/",
    )?;
    ensure(
        repo_root.join("tools/ast-grep/sgconfig.yml").is_file(),
        "project layout contract requires bpk-lib/tools/ast-grep/sgconfig.yml",
    )?;

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

fn check_root_markdown_allowlist(project_root: &Path) -> Result<()> {
    let allowed: BTreeSet<&str> = [
        "AGENTS.md",
        "04_BATTERIES.md",
        "CHANGELOG.md",
        "08_CIRCUITS.md",
        "CODE_OF_CONDUCT.md",
        "12_CONFORMANCE.md",
        "CONTRIBUTING.md",
        "06_EVENTS.md",
        "01_FACTORY.md",
        "11_INTEGRATION.md",
        "03_INVARIANTS.md",
        "02_MODEL.md",
        "10_PROJECTIONS.md",
        "README.md",
        "07_RECEIPTS.md",
        "09_REPLAY.md",
        "SUPPORT.md",
        "05_TERMINALS.md",
    ]
    .into_iter()
    .collect();

    for entry in fs::read_dir(project_root).context("read project root")? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        ensure(
            allowed.contains(name),
            format!(
                "root markdown `{name}` is not canonical; move planning/debt docs under archive/legacy-docs or machine truth under bpk-lib/traceability"
            ),
        )?;
    }

    Ok(())
}

fn check_no_unprefixed_factory_docs(project_root: &Path) -> Result<()> {
    for legacy in [
        "FACTORY.md",
        "MODEL.md",
        "INVARIANTS.md",
        "BATTERIES.md",
        "TERMINALS.md",
        "EVENTS.md",
        "RECEIPTS.md",
        "CIRCUITS.md",
        "REPLAY.md",
        "PROJECTIONS.md",
        "INTEGRATION.md",
        "CONFORMANCE.md",
    ] {
        ensure(
            !project_root.join(legacy).exists(),
            format!(
                "unprefixed factory doc `{legacy}` must not exist at repo root; use numbered canonical names (01_FACTORY.md … 12_CONFORMANCE.md)"
            ),
        )?;
    }
    Ok(())
}

fn check_supply_chain_boundary(repo_root: &Path) -> Result<()> {
    let project_root = project_root(repo_root);
    check_no_node_package_manager_surface(project_root)?;
    check_workflow_authority_surface(project_root)?;
    Ok(())
}

fn check_no_node_package_manager_surface(project_root: &Path) -> Result<()> {
    let mut findings = Vec::new();
    for entry in walkdir::WalkDir::new(project_root)
        .into_iter()
        .filter_entry(|entry| !is_generated_or_local_state_dir(entry.path()))
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
    {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if is_node_package_manager_surface(name) {
            findings.push(relative(project_root, path));
        }
    }
    ensure(
        findings.is_empty(),
        format!(
            "supply-chain boundary: Node package-manager surface is retired from this repo; remove or explicitly redesign before adding: {}",
            findings.join(", ")
        ),
    )
}

fn check_workflow_authority_surface(project_root: &Path) -> Result<()> {
    let workflow_root = project_root.join(".github/workflows");
    if !workflow_root.exists() {
        return Ok(());
    }

    for path in files_with_extension(&workflow_root, "yml")
        .into_iter()
        .chain(files_with_extension(&workflow_root, "yaml"))
    {
        let rel = relative(project_root, &path);
        let content = fs::read_to_string(&path).with_context(|| format!("read {rel}"))?;
        for finding in workflow_authority_findings(&content) {
            ensure(false, format!("supply-chain boundary: {rel}: {finding}"))?;
        }
    }
    Ok(())
}

fn workflow_authority_findings(content: &str) -> Vec<String> {
    let mut findings = Vec::new();
    for (index, line) in content.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if line_declares_workflow_trigger(trimmed, "pull_request_target") {
            findings.push(format!(
                "line {line_number}: `pull_request_target` may run untrusted PR code with privileged context"
            ));
        }
        if line_declares_workflow_trigger(trimmed, "workflow_run") {
            findings.push(format!(
                "line {line_number}: `workflow_run` may bridge untrusted workflow output into privileged context"
            ));
        }
        if let Some(action) = workflow_uses_value(trimmed) {
            if let Some(reason) = external_action_pin_violation(action) {
                findings.push(format!("line {line_number}: {reason}"));
            }
        }
    }
    findings
}

fn line_declares_workflow_trigger(line: &str, trigger: &str) -> bool {
    line == trigger
        || line == format!("- {trigger}")
        || line.starts_with(&format!("{trigger}:"))
        || line.starts_with(&format!("- {trigger}:"))
}

fn workflow_uses_value(line: &str) -> Option<&str> {
    let after = line
        .strip_prefix("uses:")
        .or_else(|| line.strip_prefix("- uses:"))?;
    Some(after.trim().trim_matches('"').trim_matches('\''))
}

fn external_action_pin_violation(action: &str) -> Option<String> {
    if action.starts_with("./") || action.starts_with("../") {
        return None;
    }
    let action = action.split('#').next().unwrap_or(action).trim();
    let Some((name, reference)) = action.rsplit_once('@') else {
        return Some(format!(
            "external action `{action}` must be pinned by full 40-character commit SHA"
        ));
    };
    let reference = reference.split_whitespace().next().unwrap_or(reference);
    if reference.len() == 40 && reference.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!(
        "external action `{name}` is pinned to `{reference}`, not a full 40-character commit SHA"
    ))
}

fn is_node_package_manager_surface(name: &str) -> bool {
    matches!(
        name,
        "package.json"
            | "package-lock.json"
            | "npm-shrinkwrap.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "bun.lock"
            | "bun.lockb"
            | ".npmrc"
            | ".yarnrc"
            | ".yarnrc.yml"
    )
}

fn is_generated_or_local_state_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            matches!(
                name,
                ".git"
                    | ".agents"
                    | ".claude"
                    | ".codex"
                    | ".cursor"
                    | "target"
                    | "node_modules"
                    | ".next"
                    | "dist"
                    | "build"
            )
        })
}

fn check_testing_doc_renames_stay_current(repo_root: &Path) -> Result<()> {
    let harness_lints = fs::read_to_string(repo_root.join("tools/integrity/src/harness_lints.rs"))
        .context("read harness_lints.rs")?;
    ensure(
        !harness_lints.contains("HARNESS_LEDGER.md")
            && !harness_lints.contains("041_TESTING_LEDGER.md"),
        "harness lint diagnostics must name the live traceability/testing_ledger.yaml, not the retired HARNESS_LEDGER.md or archived 041_TESTING_LEDGER.md",
    )?;

    let docs_rs =
        fs::read_to_string(repo_root.join("tools/xtask/src/docs.rs")).context("read docs.rs")?;
    ensure(
        !docs_rs.contains("HARNESS_DIRECTIVE.html")
            && !docs_rs.contains("HARNESS_LEDGER.html")
            && !docs_rs.contains("TESTING_DOCTRINE.html")
            && !docs_rs.contains("TESTING_LEDGER.html"),
        "generated docs must not render retired harness docs as live pages",
    )?;
    Ok(())
}

fn check_no_mdbook_dependency(repo_root: &Path) -> Result<()> {
    let project_root = project_root(repo_root);
    let mut files = vec![
        project_root.join(".devcontainer/Dockerfile"),
        project_root.join(".github/workflows/ci.yml"),
        project_root.join(".github/workflows/perf.yml"),
        project_root.join("README.md"),
        project_root.join("01_FACTORY.md"),
        project_root.join("02_MODEL.md"),
        project_root.join("03_INVARIANTS.md"),
        project_root.join("12_CONFORMANCE.md"),
        project_root.join("CONTRIBUTING.md"),
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
    let mut current_recipe = None;
    for (index, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("set ") {
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') {
            current_recipe = trimmed.split(':').next();
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            let is_escape_hatch = matches!(current_recipe, Some("cargo +args"));
            ensure(
                trimmed.starts_with("cd bpk-lib; cargo xtask ")
                    || trimmed.starts_with("cd bpk-lib && cargo xtask ")
                    || trimmed.starts_with("cargo xtask ")
                    || trimmed.starts_with("just ")
                    || (is_escape_hatch && trimmed.starts_with("cd bpk-lib; cargo ")),
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
        package.contains("readme = \"README.md\""),
        "crates/core/Cargo.toml must point at the crate-local README.md so Cargo packaging stays warning-free",
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
        !cargo_config.contains("target-dir"),
        "bpk-lib/.cargo/config.toml must not override Cargo's default bpk-lib/target/ artifact directory",
    )?;

    let nextest_config = fs::read_to_string(repo_root.join(".config/nextest.toml"))
        .context("read .config/nextest.toml")?;
    ensure(
        nextest_config.contains("dir = \"target/nextest\"")
            && nextest_config.contains("path = \"junit.xml\"")
            && !nextest_config.contains("path = \"../target/"),
        "nextest reports and run metadata must stay under bpk-lib/target/nextest",
    )?;

    let devcontainer = fs::read_to_string(project_root.join(".devcontainer/devcontainer.json"))
        .context("read devcontainer.json")?;
    ensure(
        devcontainer.contains("\"CARGO_TARGET_DIR\": \"/workspace/batpak/bpk-lib/target\""),
        "devcontainer must keep Cargo output under bpk-lib/target/",
    )?;

    let ci = fs::read_to_string(project_root.join(".github/workflows/ci.yml"))
        .context("read ci workflow")?;
    ensure(
        ci.contains("path: bpk-lib/target/site") && !ci.contains("path: target/site"),
        "CI artifact paths must use bpk-lib/target/, not repo-root target/",
    )?;

    let perf = fs::read_to_string(project_root.join(".github/workflows/perf.yml"))
        .context("read perf workflow")?;
    ensure(
        perf.contains("path: bpk-lib/target/criterion") && !perf.contains("path: target/criterion"),
        "perf artifact paths must use bpk-lib/target/, not repo-root target/",
    )?;

    // Workspace-`exclude`d crates (e.g. the cargo-fuzz crate at `fuzz/`) are
    // their own build graphs and legitimately own a `<crate>/target/`. Exempt
    // those target dirs from the single-target-dir contract; only the main
    // workspace must funnel artifacts into bpk-lib/target/.
    let workspace_toml = fs::read_to_string(repo_root.join("Cargo.toml"))
        .context("read Cargo.toml for workspace exclude list")?;
    let exempt_target_dirs = excluded_crate_target_dirs(&workspace_toml);

    for entry in walkdir::WalkDir::new(repo_root)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_dir())
    {
        if entry.file_name() == "target" {
            let rel = relative(repo_root, entry.path());
            if rel == "target" {
                continue;
            }
            if exempt_target_dirs.contains(rel.as_str()) {
                continue;
            }
            let mut children = fs::read_dir(entry.path())
                .with_context(|| format!("read nested target dir `{rel}`"))?;
            ensure(
                children.next().is_none(),
                format!(
                    "nested Cargo target dir `{rel}` contains artifacts; use bpk-lib/target/ only"
                ),
            )?;
        }
    }

    Ok(())
}

/// Repo-relative `target` directories owned by workspace-`exclude`d crates.
/// Parses the `exclude = [ ... ]` array from the workspace `Cargo.toml` and maps
/// each excluded crate path `<dir>` to `<dir>/target`, so a freshly-built
/// excluded crate (e.g. cargo-fuzz) does not trip the single-target-dir gate.
fn excluded_crate_target_dirs(workspace_toml: &str) -> BTreeSet<String> {
    let mut targets = BTreeSet::new();
    let Some(after) = workspace_toml.split_once("exclude").and_then(|(_, rest)| {
        let rest = rest.trim_start();
        rest.strip_prefix('=')
    }) else {
        return targets;
    };
    let after = after.trim_start();
    let Some(start) = after.strip_prefix('[') else {
        return targets;
    };
    let Some((body, _)) = start.split_once(']') else {
        return targets;
    };
    for raw in body.split(',') {
        // Strip line comments and surrounding whitespace/quotes from each entry.
        let entry = raw.split('#').next().unwrap_or("").trim();
        let entry = entry.trim_matches(|c| c == '"' || c == '\'').trim();
        if entry.is_empty() {
            continue;
        }
        let normalized = entry.trim_end_matches('/');
        targets.insert(format!("{normalized}/target"));
    }
    targets
}

fn check_crate_layout_contract(repo_root: &Path) -> Result<()> {
    for dir in [
        "crates/core/src",
        "crates/core/tests",
        "crates/batpak-examples/src/bin",
        "crates/core/benches",
        "crates/core/fixtures",
        "crates/syncbat/src",
        "crates/syncbat/tests",
        "crates/syncbat/benches",
        "crates/netbat/src",
        "crates/netbat/tests",
        "crates/netbat/benches",
        "crates/macros/src",
        "crates/macros-support/src",
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

    ensure(
        !repo_root.join("crates/examples").exists(),
        "legacy `crates/examples` path retired; demos live in `crates/batpak-examples/src/bin/`",
    )?;

    // Demos live ONLY in the family-wide `crates/batpak-examples` crate — no per-crate
    // `examples/` folder anywhere else (locks the examples-out-of-core hoist).
    // Generalized over every crate so a future `crates/<x>/examples/` is caught.
    if let Ok(entries) = fs::read_dir(repo_root.join("crates")) {
        for entry in entries.flatten() {
            if entry.path().join("examples").is_dir() {
                let name = entry.file_name().to_string_lossy().into_owned();
                ensure(
                    false,
                    format!(
                        "`crates/{name}/examples` blurs ownership; demos live in the family-wide `crates/batpak-examples` crate"
                    ),
                )?;
            }
        }
    }

    // syncbat/netbat must not grow their own fixtures; a crate's genuinely-owned
    // cross-crate inputs live under that crate (e.g. `crates/core/fixtures`).
    for crate_name in ["syncbat", "netbat"] {
        ensure(
            !repo_root
                .join("crates")
                .join(crate_name)
                .join("fixtures")
                .exists(),
            format!(
                "`crates/{crate_name}/fixtures` would blur ownership; companion-crate cross-crate inputs belong under their owning crate or as explicit tests"
            ),
        )?;
    }

    let (macro_crate, owner) = ("crates/macros", "crates/core/tests/ui");
    ensure(
        repo_root.join(owner).is_dir(),
        format!(
            "`{macro_crate}` does not need its own integration-test folder, but `{owner}` must exist as its compile-fail owner"
        ),
    )?;

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

fn check_core_feature_cfg_contract(repo_root: &Path) -> Result<()> {
    const IMPOSSIBLE_FEATURE_GUARDS: &[&str] = &["async-store", "exponential-backoff", "sha256"];

    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(repo_root.join("Cargo.toml"))
        .no_deps()
        .exec()
        .context("read Cargo metadata for core feature cfg contract")?;
    let package = metadata
        .packages
        .iter()
        .find(|package| package.name == "batpak")
        .context("Cargo metadata must contain root batpak package")?;
    let declared_features = package.features.keys().cloned().collect::<BTreeSet<_>>();
    let impossible_features = IMPOSSIBLE_FEATURE_GUARDS
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();

    for path in files_with_extension(&repo_root.join("crates/core/src"), "rs") {
        let rel = relative(repo_root, &path);
        let content = fs::read_to_string(&path).with_context(|| format!("read {rel}"))?;
        let lines = content.lines().collect::<Vec<_>>();

        for feature in extract_cfg_feature_names(&content) {
            ensure(
                declared_features.contains(&feature)
                    || impossible_features.contains(feature.as_str()),
                format!(
                    "undeclared feature cfg `{feature}` in {rel}; declare it in crates/core/Cargo.toml or add a deliberate impossible-feature compile_error guard"
                ),
            )?;
        }

        for (index, line) in lines.iter().enumerate() {
            if !line.contains("allow(unexpected_cfgs)") {
                continue;
            }

            if line.trim_start().starts_with("#![") {
                ensure(
                    rel == "crates/core/src/lib.rs",
                    format!(
                        "crate-level unexpected_cfgs allowance is only permitted at crates/core/src/lib.rs, found in {rel}:{}",
                        index + 1
                    ),
                )?;
                ensure(
                    ["async-store", "sha256"]
                        .iter()
                        .all(|feature| content.contains(&format!("#[cfg(feature = \"{feature}\")]"))),
                    "crates/core/src/lib.rs crate-level unexpected_cfgs allowance must be paired with its impossible-feature compile_error guards",
                )?;
                continue;
            }

            let Some(feature) = following_cfg_feature(&lines, index + 1) else {
                ensure(
                    false,
                    format!(
                        "unexpected_cfgs allowance at {rel}:{} must directly guard an impossible-feature cfg",
                        index + 1
                    ),
                )?;
                continue;
            };
            ensure(
                impossible_features.contains(feature.as_str()),
                format!(
                    "unexpected_cfgs allowance at {rel}:{} guards `{feature}`, but only impossible-feature compile_error tripwires may suppress cfg warnings",
                    index + 1
                ),
            )?;
            ensure(
                following_lines_contain(&lines, index + 1, 4, "compile_error!"),
                format!(
                    "unexpected_cfgs allowance at {rel}:{} must lead to compile_error!, not dormant code",
                    index + 1
                ),
            )?;
        }
    }

    Ok(())
}

fn extract_cfg_feature_names(content: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut remaining = content;
    const NEEDLE: &str = "feature = \"";
    while let Some(start) = remaining.find(NEEDLE) {
        let after = &remaining[start + NEEDLE.len()..];
        let Some(end) = after.find('"') else {
            break;
        };
        names.push(after[..end].to_owned());
        remaining = &after[end + 1..];
    }
    names
}

fn following_cfg_feature(lines: &[&str], start: usize) -> Option<String> {
    lines.iter().skip(start).find_map(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            return None;
        }
        extract_cfg_feature_names(trimmed).into_iter().next()
    })
}

fn following_lines_contain(lines: &[&str], start: usize, count: usize, needle: &str) -> bool {
    lines
        .iter()
        .skip(start)
        .take(count)
        .any(|line| line.contains(needle))
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
        justfile_content.contains("bench-compile:\n    cd bpk-lib; cargo xtask bench --compile"),
        "justfile bench-compile recipe must remain a thin alias over `cd bpk-lib; cargo xtask bench --compile`",
    )?;
    ensure(
        justfile_content.contains("install-hooks:\n    cd bpk-lib; cargo xtask install-hooks"),
        "justfile install-hooks recipe must remain a thin alias over `cd bpk-lib; cargo xtask install-hooks`",
    )?;
    ensure(
        justfile_content.contains("stress:\n    cd bpk-lib; cargo xtask stress"),
        "justfile stress recipe must remain a thin alias over `cd bpk-lib; cargo xtask stress`",
    )?;
    ensure(
        justfile_content.contains("ci-fast:\n    cd bpk-lib; cargo xtask ci-fast"),
        "justfile ci-fast recipe must remain a thin alias over `cd bpk-lib; cargo xtask ci-fast`",
    )?;
    ensure(
        justfile_content.contains("ci-windows:\n    cd bpk-lib; cargo xtask ci-windows-surface"),
        "justfile ci-windows recipe must remain a thin alias over `cd bpk-lib; cargo xtask ci-windows-surface`",
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
                .contains("outln!(\"Running tests with coverage instrumentation...\");")
            && coverage_content
                .contains("outln!(\"Coverage export written to {}\", coverage_json.display());"),
        "xtask coverage banners must stay out of JSON mode",
    )?;
    ensure(
        coverage_content.contains("out!(\"{json_text}\");"),
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
        setup_devcontainer_action_content.contains("dtolnay/rust-toolchain@")
            && setup_devcontainer_action_content.contains("# 1.92.0")
            && setup_devcontainer_action_content.contains("toolchain: 1.92.0"),
        "setup-devcontainer action must install a pinned host Rust toolchain so the thin wrapper can delegate to cargo xtask",
    )?;
    ensure(
        dockerfile_content.contains("ENV PATH=\"/usr/local/cargo/bin:${PATH}\"")
            && dockerfile_content.contains("install-from-binstall-release.sh")
            && !dockerfile_content.contains("cargo-binstall/main/")
            && dockerfile_content.contains("cargo binstall --no-confirm cargo-deny@0.19.0")
            && dockerfile_content.contains("cargo binstall --no-confirm cargo-llvm-cov@0.8.5"),
        "devcontainer bootstrap must expose cargo on PATH and prefer binstall for pinned cargo helper tools before falling back to source builds; cargo-binstall bootstrap must not follow mutable main",
    )?;
    ensure(
        dockerfile_content.contains("FROM rust:1.92-bookworm@sha256:")
            && dockerfile_content.contains("nightly-2025-12-11"),
        "devcontainer base image and rustdoc-json nightly must stay pinned for reproducible supply-chain posture",
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

    // A file gates a family crate either by naming it explicitly, or by a
    // dynamic mechanism that provably covers every workspace member despite the
    // core-only `default-members`: a `--workspace` cargo invocation, or the
    // `FAMILY_CRATES` iteration constant. Dynamic coverage is preferred — it
    // cannot silently drop a newly-added crate the way a hardcoded list can.
    let gates_family = |content: &str, package: &str| -> bool {
        content.contains(&format!("\"{package}\""))
            || content.contains("--workspace")
            || content.contains("FAMILY_CRATES")
    };

    for (label, content) in [
        ("tools/xtask/src/main.rs", xtask_main.as_str()),
        ("tools/xtask/src/commands/ci.rs", ci_rs.as_str()),
    ] {
        for package in &active_family_crates {
            ensure(
                gates_family(content, package)
                    && content.contains("\"check\"")
                    && content.contains("\"test\"")
                    && content.contains("\"clippy\""),
                format!(
                    "{label} must gate {package} (explicitly, via --workspace, or via FAMILY_CRATES) with check, test, and clippy while default-members stays core-only"
                ),
            )?;
        }
    }

    for package in &active_family_crates {
        ensure(
            gates_family(&coverage_rs, package),
            format!("tools/xtask/src/coverage.rs must include {package} (explicitly or via --workspace) while default-members stays core-only"),
        )?;
        ensure(
            gates_family(&docs_rs, package),
            format!("tools/xtask/src/docs.rs must include {package} (explicitly or via --workspace) while default-members stays core-only"),
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

#[cfg(test)]
mod tests {
    use super::{
        excluded_crate_target_dirs, external_action_pin_violation, is_node_package_manager_surface,
        workflow_authority_findings,
    };

    #[test]
    fn excluded_crate_target_dirs_exempts_workspace_excludes() {
        let manifest = r#"
[workspace]
members = ["crates/core"]
exclude = [
    "fuzz",
    "scratch/", # trailing slash + comment
]
resolver = "2"
"#;
        let dirs = excluded_crate_target_dirs(manifest);
        assert!(
            dirs.contains("fuzz/target"),
            "the cargo-fuzz crate's target dir must be exempt, got {dirs:?}"
        );
        assert!(
            dirs.contains("scratch/target"),
            "trailing-slash + commented entries must normalize, got {dirs:?}"
        );
        assert!(
            !dirs.contains("target"),
            "the main workspace target dir must never be exempt"
        );
    }

    #[test]
    fn excluded_crate_target_dirs_empty_when_no_exclude_list() {
        let manifest = "[workspace]\nmembers = [\"crates/core\"]\nresolver = \"2\"\n";
        assert!(excluded_crate_target_dirs(manifest).is_empty());
    }

    #[test]
    fn supply_chain_boundary_detects_dangerous_workflow_triggers() {
        let workflow = r#"
on:
  pull_request_target:
  workflow_run:
jobs:
  test:
    steps:
      - uses: actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10
"#;
        let findings = workflow_authority_findings(workflow);
        assert!(
            findings
                .iter()
                .any(|finding| finding.contains("pull_request_target")),
            "expected pull_request_target finding, got {findings:?}"
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.contains("workflow_run")),
            "expected workflow_run finding, got {findings:?}"
        );
    }

    #[test]
    fn supply_chain_boundary_requires_external_action_sha_pins() {
        assert!(
            external_action_pin_violation("actions/checkout@v6").is_some(),
            "tag-pinned external actions must be rejected"
        );
        assert!(
            external_action_pin_violation(
                "actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10"
            )
            .is_none(),
            "full SHA-pinned external actions must pass"
        );
        assert!(
            external_action_pin_violation("./.github/actions/setup-devcontainer").is_none(),
            "local composite actions are repo-owned and do not need external SHA pins"
        );
    }

    #[test]
    fn supply_chain_boundary_recognizes_node_package_manager_surfaces() {
        for name in [
            "package.json",
            "package-lock.json",
            "pnpm-lock.yaml",
            "yarn.lock",
            "bun.lock",
            ".npmrc",
        ] {
            assert!(
                is_node_package_manager_surface(name),
                "{name} must be treated as a Node package-manager surface"
            );
        }
        assert!(!is_node_package_manager_surface("Cargo.toml"));
    }
}
