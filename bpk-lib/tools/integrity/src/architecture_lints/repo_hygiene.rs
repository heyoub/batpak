use super::{ensure, relative};
use anyhow::{Context, Result};
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn check(repo_root: &Path, tracked_files: &[PathBuf]) -> Result<()> {
    check_no_tracked_docs_dir(repo_root, tracked_files)?;
    check_no_live_spec_markers(repo_root, tracked_files)?;
    check_no_legacy_topology_or_replay_names(repo_root, tracked_files)?;
    check_for_absolute_paths(repo_root, tracked_files)?;
    check_for_stale_references(repo_root, tracked_files)?;
    check_release_hardening_patterns(repo_root, tracked_files)?;
    check_bidirectional_substrate_lane_terms(repo_root, tracked_files)?;
    check_boundary_scripts_only(repo_root, tracked_files)?;
    check_removed_script_references(repo_root, tracked_files)?;
    Ok(())
}

fn check_no_tracked_docs_dir(repo_root: &Path, tracked_files: &[PathBuf]) -> Result<()> {
    for path in tracked_files {
        let rel = relative(repo_root, path);
        ensure(
            !rel.starts_with("docs/"),
            format!(
                "tracked docs/ material is no longer allowed; use ordered root docs or cookbook/ instead of `{rel}`"
            ),
        )?;
    }
    Ok(())
}

fn check_no_live_spec_markers(repo_root: &Path, tracked_files: &[PathBuf]) -> Result<()> {
    // justifies: INV-LITERAL-REGEX-UNWRAP-SAFE; literal regex pattern is compile-time-constant in tools/integrity/src/architecture_lints/repo_hygiene.rs, unwrap safe by construction
    let marker = Regex::new(r"\\?\[SPEC:")
        .expect("internal regex is a compile-time constant and will compile");
    let allow = [repo_root.join("tools/integrity/src/architecture_lints/repo_hygiene.rs")];
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
            || rel.starts_with("crates/core/src/")
            || rel.starts_with("src/")
            || rel.starts_with("tools/")
            || rel.starts_with("crates/core/tests/")
            || rel.starts_with("tests/")
            || rel.starts_with("crates/core/benches/")
            || rel.starts_with("benches/")
            || rel.starts_with("crates/core/examples/")
            || rel.starts_with("examples/")
            || rel.starts_with("traceability/")
            || rel.starts_with("cookbook/");
        if !is_live_surface || !matches!(ext, "rs" | "md" | "yml" | "yaml") {
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
    // justifies: INV-LITERAL-REGEX-UNWRAP-SAFE; literal regex pattern is compile-time-constant in tools/integrity/src/architecture_lints/repo_hygiene.rs, unwrap safe by construction
    let banned_terms = [
        (
            "IndexLayout",
            Regex::new(r"\bIndexLayout\b")
                .expect("internal regex is a compile-time constant and will compile"),
        ),
        (
            "ViewConfig",
            Regex::new(r"\bViewConfig\b")
                .expect("internal regex is a compile-time constant and will compile"),
        ),
        (
            "ProjectionMode",
            Regex::new(r"\bProjectionMode\b")
                .expect("internal regex is a compile-time constant and will compile"),
        ),
        (
            "ValueInput",
            Regex::new(r"\bValueInput\b")
                .expect("internal regex is a compile-time constant and will compile"),
        ),
    ];
    let allow = [
        repo_root.join("CHANGELOG.md"),
        repo_root.join("tools/integrity/src/architecture_lints/repo_hygiene.rs"),
    ];
    for path in tracked_files {
        if allow.iter().any(|allowed| allowed == path) {
            continue;
        }
        let rel = relative(repo_root, path);
        let is_live_surface = rel == "README.md"
            || rel == "FACTORY.md"
            || rel == "MODEL.md"
            || rel == "INVARIANTS.md"
            || rel == "BATTERIES.md"
            || rel == "TERMINALS.md"
            || rel == "EVENTS.md"
            || rel == "RECEIPTS.md"
            || rel == "CIRCUITS.md"
            || rel == "REPLAY.md"
            || rel == "PROJECTIONS.md"
            || rel == "INTEGRATION.md"
            || rel == "CONFORMANCE.md"
            || rel.starts_with("crates/core/src/")
            || rel.starts_with("src/")
            || rel.starts_with("crates/core/examples/")
            || rel.starts_with("examples/")
            || rel.starts_with("crates/core/tests/")
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
    // justifies: INV-LITERAL-REGEX-UNWRAP-SAFE; literal regex pattern is compile-time-constant in tools/integrity/src/architecture_lints/repo_hygiene.rs, unwrap safe by construction
    let absolute_windows = Regex::new(r"(^|[^A-Za-z])([A-Za-z]:\\)")
        .expect("internal regex is a compile-time constant and will compile");
    // justifies: INV-LITERAL-REGEX-UNWRAP-SAFE; literal regex pattern is compile-time-constant in tools/integrity/src/architecture_lints/repo_hygiene.rs, unwrap safe by construction
    let absolute_unix = Regex::new(r"(?m)(file://|/Users/|/home/|/opt/|/tmp/)")
        .expect("internal regex is a compile-time constant and will compile");
    let allow = [
        repo_root.join(".devcontainer/Dockerfile"),
        // devcontainer.json intentionally describes a Linux container's
        // filesystem (mount points, CARGO_TARGET_DIR, workspaceFolder). Same
        // rationale as the Dockerfile: not portable source, already scoped to
        // a single OS by construction.
        repo_root.join(".devcontainer/devcontainer.json"),
        repo_root.join("tools/integrity/src/main.rs"),
        repo_root.join("tools/integrity/src/architecture_lints/repo_hygiene.rs"),
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
        // Old flat store paths — renamed to subdirectory layout in v0.6 restructure.
        // Safe to ban: the new paths all have an intermediate directory component
        // (e.g. store/write/writer.rs, store/index/mod.rs) so these literal strings
        // cannot appear as substrings of current paths.
        "store/contracts.rs",
        "store/control_plane.rs",
        "store/reader.rs",
        "store/maintenance.rs",
        "store/mmap_index.rs",
        "store/index_rebuild.rs",
        "store/visibility_ranges.rs",
        "store/projection_flow.rs",
        "store/ancestors.rs",
        "store/ancestors_hash.rs",
        "store/ancestors_clock.rs",
        "store/subscription.rs",
        "store/writer.rs",
        "store/fanout.rs",
        "store/staging.rs",
        "store/cursor.rs",
        "store/watch.rs",
        "Six reference operations live",
        "six-op host profile",
        "six reference NETBAT operations",
        "for the 6 operations",
    ];
    let allow = [
        repo_root
            .parent()
            .unwrap_or(repo_root)
            .join("archive/decisions/100_ADR_0003_CACHE_SAFETY_ASSUMPTIONS.md"),
        repo_root.join("CHANGELOG.md"),
        repo_root.join("AGENTS.md"),
        repo_root.join("crates/core/build.rs"),
        repo_root.join("tools/integrity/src/main.rs"),
        repo_root.join("tools/integrity/src/architecture_lints/repo_hygiene.rs"),
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
        repo_root.join("crates/core/build.rs"),
        repo_root.join("tools/integrity/src/main.rs"),
        repo_root.join("tools/integrity/src/architecture_lints/repo_hygiene.rs"),
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

        if rel == "crates/core/src/store/mod.rs" || rel == "src/store/mod.rs" {
            ensure(
                !content.contains("pub fn subscribe("),
                "structural-check: src/store/mod.rs still exports ambiguous `subscribe`",
            )?;
            ensure(
                !content.contains("pub fn cursor("),
                "structural-check: src/store/mod.rs still exports ambiguous `cursor`",
            )?;
        }

        if rel.starts_with("crates/core/src/store/") || rel.starts_with("src/store/") {
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

fn check_bidirectional_substrate_lane_terms(
    repo_root: &Path,
    tracked_files: &[PathBuf],
) -> Result<()> {
    let forbidden_substrate_ops = [
        "mission.replay",
        "downstream.query",
        "workflow.events",
        "receipt.walk",
    ];
    for path in tracked_files {
        let rel = relative(repo_root, path);
        if rel == "tools/integrity/src/architecture_lints/repo_hygiene.rs" {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default();
        let is_substrate_wire_surface = rel.starts_with("crates/refbat/")
            || rel.starts_with("crates/netbat/")
            || rel.starts_with("crates/syncbat/")
            || rel.starts_with("bpk-ts/");
        if is_substrate_wire_surface && matches!(ext, "rs" | "ts" | "json" | "md") {
            let content =
                fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
            for op in forbidden_substrate_ops {
                ensure(
                    !content.contains(op),
                    format!(
                        "domain-specific operation `{op}` found in substrate wire surface {rel}; use domain-neutral event.query/event.get and decode above batpak"
                    ),
                )?;
            }
        }

        let is_live_doc = matches!(
            rel.as_str(),
            "REPLAY.md" | "INTEGRATION.md" | "EVENTS.md" | "TERMINALS.md" | "CONFORMANCE.md"
        );
        if is_live_doc && ext == "md" {
            let content =
                fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
            ensure(
                !content.contains("after_sequence"),
                format!(
                    "ambiguous traversal cursor `after_sequence` found in {rel}; say after_global_sequence"
                ),
            )?;
        }
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

    let project_root = repo_root.parent().unwrap_or(repo_root);
    let bootstrap_sh = fs::read_to_string(project_root.join("scripts/bootstrap.sh"))
        .context("read scripts/bootstrap.sh")?;
    ensure(
        bootstrap_sh.contains("cd \"$(dirname \"$0\")/../bpk-lib\"")
            && bootstrap_sh.contains("cargo xtask setup --install-tools"),
        "scripts/bootstrap.sh must stay a thin wrapper over `cargo xtask setup --install-tools` from bpk-lib",
    )?;

    let bootstrap_ps1 = fs::read_to_string(project_root.join("scripts/bootstrap.ps1"))
        .context("read scripts/bootstrap.ps1")?;
    ensure(
        bootstrap_ps1.contains("Set-Location \"$PSScriptRoot\\..\\bpk-lib\"")
            && bootstrap_ps1.contains("cargo xtask setup --install-tools"),
        "scripts/bootstrap.ps1 must stay a thin wrapper over `cargo xtask setup --install-tools` from bpk-lib",
    )?;

    let run_in_devcontainer =
        fs::read_to_string(project_root.join("scripts/run-in-devcontainer.sh"))
            .context("read scripts/run-in-devcontainer.sh")?;
    ensure(
        run_in_devcontainer.contains("cd \"${repo_root}/bpk-lib\"")
            && run_in_devcontainer.contains("exec cargo xtask devcontainer-exec -- \"$@\""),
        "scripts/run-in-devcontainer.sh must stay a thin wrapper over `cargo xtask devcontainer-exec` from bpk-lib",
    )?;

    Ok(())
}

fn check_removed_script_references(repo_root: &Path, tracked_files: &[PathBuf]) -> Result<()> {
    let removed = [
        "scripts/bench-report",
        "scripts/coverage-feedback",
        "scripts/verify-all.sh",
    ];
    let allow = [repo_root.join("tools/integrity/src/architecture_lints/repo_hygiene.rs")];
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
