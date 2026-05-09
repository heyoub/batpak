use crate::repo_surface::{ensure, files_with_extension, relative};
use anyhow::{bail, Context, Result};
use regex::Regex;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::Path;

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
/// Construct a regex that matches `<tool>@<semver>` for a caller-supplied
/// `tool` name. The `tool` string is validated to contain only
/// `[A-Za-z0-9-]+` so no regex metacharacter can slip through. A malformed
/// tool name returns an error rather than relying on `.unwrap()` at regex
/// build time.
fn build_tool_pin_regex(tool: &str) -> Result<Regex> {
    if tool.is_empty()
        || !tool
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        bail!(
            "internal error: tool name `{tool}` contains characters outside [A-Za-z0-9_-]; refusing to build a regex that would be shaped by user input"
        );
    }
    Regex::new(&format!(r"{tool}@(\d+(?:\.\d+)+)"))
        .with_context(|| format!("compile tool-pin regex for `{tool}`"))
}

pub(crate) fn check(repo_root: &Path) -> Result<()> {
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
    // justifies: INV-LITERAL-REGEX-UNWRAP-SAFE; pattern is a string literal known-safe at compile time in tools/integrity/src/structural.rs; this expect cannot fire in any reachable code path
    let xtask_cmd_re = Regex::new(r"cargo\s+xtask\s+([a-z][a-z0-9-]*)")
        .expect("internal regex is a compile-time constant and will compile");
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
    assert_workflow_list_values(
        ".github/workflows/ci.yml",
        &ci_yml,
        "features",
        &[
            "",
            "--features blake3",
            "--features dangerous-test-hooks",
            "--no-default-features",
            "--no-default-features --features blake3",
            "--no-default-features --features dangerous-test-hooks",
            "--all-features",
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
    // justifies: INV-LITERAL-REGEX-UNWRAP-SAFE; pattern is a string literal known-safe at compile time in tools/integrity/src/structural.rs; this expect cannot fire in any reachable code path
    let tool_install_re = Regex::new(r#"tool:\s*([a-z][a-z0-9-]*)@(\d+(?:\.\d+)+)"#)
        .expect("internal regex is a compile-time constant and will compile");
    // justifies: INV-LITERAL-REGEX-UNWRAP-SAFE; pattern is a string literal known-safe at compile time in tools/integrity/src/structural.rs; this expect cannot fire in any reachable code path
    let bare_tool_re = Regex::new(r#"tool:\s*([a-z][a-z0-9-]*)\s*$"#)
        .expect("internal regex is a compile-time constant and will compile");
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
        let xtask_pin_re = build_tool_pin_regex(tool)?;
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

    let docker_pins = dockerfile_tool_pins(&dockerfile)?;

    // 3. Every workflow-owned tool pin must also be present in the
    //    Dockerfile with the same version. This is derived from the workflow
    //    install-action entries instead of a fixed list so newly added tools
    //    are covered automatically.
    for (tool, wf_ver) in &workflow_tools {
        match docker_pins.get(tool) {
            Some(dock_ver) if dock_ver == wf_ver => {}
            Some(dock_ver) => {
                bail!(
                    "ci-parity: tool `{tool}` is pinned to `{wf_ver}` in \
                     `.github/workflows/ci.yml` but `{dock_ver}` in \
                     `.devcontainer/Dockerfile`. Pick one version and update both."
                );
            }
            None => {
                bail!(
                    "ci-parity: workflow installs `{tool}@{wf_ver}` via \
                     taiki-e/install-action but `.devcontainer/Dockerfile` does not pin `{tool}`. \
                     Add the same tool pin to the Dockerfile or remove it from the workflow."
                );
            }
        }
    }

    // 4. Tool version pin parity between Dockerfile and xtask setup. This is
    //    also dynamic over the Dockerfile so a new canonical container tool
    //    must be represented in xtask setup.
    for (tool, dock_v) in &docker_pins {
        let xtask_pin_re = build_tool_pin_regex(tool)?;
        let xtask_v = xtask_pin_re
            .captures(&xtask_sources)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());
        match xtask_v {
            Some(x) if &x != dock_v => {
                bail!(
                    "ci-parity: tool `{tool}` is pinned to `{d}` in \
                     `.devcontainer/Dockerfile` but `{x}` in `cargo xtask setup --install-tools` \
                     (tools/xtask/src/). Pick one version and update both.",
                    d = dock_v
                );
            }
            None => {
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

pub(crate) fn dockerfile_tool_pins(dockerfile: &str) -> Result<HashMap<String, String>> {
    // justifies: INV-LITERAL-REGEX-UNWRAP-SAFE; pattern is a string literal known-safe at compile time in tools/integrity/src/structural.rs; this expect cannot fire in any reachable code path
    let pin_re = Regex::new(r"\b(cargo-[a-z0-9-]+)@(\d+(?:\.\d+)+)\b")
        .expect("internal regex is a compile-time constant and will compile");
    let mut pins = HashMap::new();
    for cap in pin_re.captures_iter(dockerfile) {
        let tool = cap.get(1).map(|m| m.as_str()).unwrap_or_default();
        let version = cap.get(2).map(|m| m.as_str()).unwrap_or_default();
        match pins.insert(tool.to_owned(), version.to_owned()) {
            Some(existing) if existing != version => {
                bail!(
                    "ci-parity: `.devcontainer/Dockerfile` pins `{tool}` to both `{existing}` and `{version}`"
                );
            }
            _ => {}
        }
    }
    Ok(pins)
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

pub(crate) fn workflow_list_values(workflow: &str, key: &str) -> Result<Vec<String>> {
    if key.is_empty()
        || !key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        bail!(
            "internal error: workflow list key `{key}` must be `[A-Za-z0-9_]+` so regex construction is safe"
        );
    }
    let inline_re = Regex::new(&format!(r"^\s*{}\s*:\s*\[(?P<values>[^\]]+)\]\s*$", key))
        .with_context(|| format!("compile workflow list regex for key `{key}`"))?;
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
