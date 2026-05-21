use crate::repo_surface::{
    core_benches_root, core_examples_root, core_src_root, core_tests_root, ensure, relative,
    repo_root, rust_files, tracked_repo_files,
};
use crate::shared_checks::{
    collect_dead_code_silencer_sites, line_carries_justification,
    load_dead_code_silencer_allowlist, load_known_invariants,
};
use crate::{
    agent_surface, architecture_lints, ci_parity, harness_lints, invariant_bridge, public_surface,
    store_pub_fn_coverage,
};
use anyhow::{anyhow, bail, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn run() -> Result<()> {
    let repo_root = repo_root()?;
    let tracked_files = tracked_repo_files(&repo_root)?;
    architecture_lints::check(&repo_root, &tracked_files)?;
    agent_surface::check(&repo_root)?;
    harness_lints::check(&repo_root, &tracked_files)?;
    invariant_bridge::check(&repo_root, &tracked_files)?;
    check_no_dead_code_silencers(&repo_root)?;
    check_no_placeholder_runtime_macros(&repo_root)?;
    check_canonical_encoding_boundary(&repo_root)?;
    check_allow_justifications(&repo_root)?;
    check_rust_file_size_pressure(&repo_root)?;
    public_surface::check(&repo_root)?;
    ci_parity::check(&repo_root)?;
    store_pub_fn_coverage::check(&repo_root)?;
    println!("structural-check: ok");
    Ok(())
}

fn check_rust_file_size_pressure(repo_root: &Path) -> Result<()> {
    const DEFAULT_LINE_BUDGET: usize = 850;
    const RATCHELED_OVER_BUDGET_FILES: &[(&str, usize)] = &[
        ("crates/core/src/store/index/columnar.rs", 1474),
        ("crates/core/src/store/cold_start/checkpoint.rs", 1326),
        ("crates/core/src/store/cold_start/rebuild.rs", 1194),
        ("crates/core/src/store/error.rs", 1129),
        ("crates/core/src/store/segment/sidx.rs", 995),
        ("crates/core/src/store/config.rs", 1003),
        ("crates/core/src/store/delivery/cursor.rs", 971),
        ("crates/core/src/store/index/mod.rs", 929),
        ("crates/macros/src/lib.rs", 915),
        ("crates/core/src/store/write/writer.rs", 903),
        ("crates/core/src/store/projection/flow/mod.rs", 893),
    ];

    for path in production_rust_files(repo_root) {
        let rel = relative(repo_root, &path);
        let content = fs::read_to_string(&path)?;
        let line_count = content.lines().count();
        let budget = RATCHELED_OVER_BUDGET_FILES
            .iter()
            .find_map(|(known_rel, budget)| (*known_rel == rel).then_some(*budget))
            .unwrap_or(DEFAULT_LINE_BUDGET);
        ensure(
            line_count <= budget,
            format!(
                "structural-check: production Rust file size pressure in {rel}: {line_count} lines exceeds budget {budget}.\n\
                 New production files must stay at or below {DEFAULT_LINE_BUDGET} lines. \
                 Existing oversized files are ratcheted at their current ceiling until they are extracted."
            ),
        )?;
    }
    Ok(())
}

fn check_no_placeholder_runtime_macros(repo_root: &Path) -> Result<()> {
    let mut paths = production_rust_files(repo_root);
    paths.extend(rust_files(&repo_root.join("tools/xtask/src")));
    paths.extend(rust_files(&repo_root.join("tools/integrity/src")));
    paths.push(repo_root.join("crates/core/build.rs"));

    for path in paths {
        let rel = relative(repo_root, &path);
        let content = fs::read_to_string(&path)?;
        for (line_no, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            for needle in [
                "to".to_owned() + "do!(",
                "unimplemented".to_owned() + "!(",
                "d".to_owned() + "bg!(",
            ] {
                if line.contains(&needle) {
                    bail!(
                        "structural-check: disallowed placeholder/debug macro `{}` in {}:{}.\n\
                         Remove the macro and implement explicit behavior or diagnostics.\n\
                         See INV-BUILD-FAIL-FAST and INV-TRACEABILITY-COMPLETE.",
                        needle,
                        rel,
                        line_no + 1
                    );
                }
            }
        }
    }
    Ok(())
}

fn check_no_dead_code_silencers(repo_root: &Path) -> Result<()> {
    let allowlisted = load_dead_code_silencer_allowlist(repo_root).map_err(|err| anyhow!(err))?;
    let mut paths = production_rust_files(repo_root);
    paths.extend(rust_files(&repo_root.join("tools/xtask/src")));
    paths.extend(rust_files(&repo_root.join("tools/integrity/src")));
    paths.extend(rust_files(&core_tests_root(repo_root)));
    paths.extend(rust_files(&core_examples_root(repo_root)));
    paths.extend(rust_files(&core_benches_root(repo_root)));
    paths.push(repo_root.join("crates/core/build.rs"));
    for path in paths {
        let content = fs::read_to_string(&path)?;
        let sites = collect_dead_code_silencer_sites(&content)
            .map_err(|err| anyhow!("parse {}: {}", relative(repo_root, &path), err))?;
        for site in sites {
            let allowlist_site = format!("{}:{}", relative(repo_root, &path), site.line);
            if allowlisted.contains(&allowlist_site) {
                continue;
            }
            bail!(
                "dead_code silencers are not tolerated in {}:{}:{}.\n\
                 Found `{}`.\n\
                 If code is test-only, use #[cfg(test)]. If it is unused, delete it.\n\
                 If it is shared infrastructure, restructure it so the compiler sees the real ownership surface.\n\
                 If this is the rare legitimate exception, add `{}` to traceability/dead_code_silencer_allowlist.yaml with `reason` and `adr`.",
                relative(repo_root, &path),
                site.line,
                site.column,
                site.rendered,
                allowlist_site,
            );
        }
    }
    Ok(())
}

fn check_allow_justifications(repo_root: &Path) -> Result<()> {
    let known_invariants = load_known_invariants(repo_root).map_err(|err| anyhow!(err))?;
    let mut paths = production_rust_files(repo_root);
    paths.extend(rust_files(&repo_root.join("tools/xtask/src")));
    paths.extend(rust_files(&repo_root.join("tools/integrity/src")));
    paths.extend(rust_files(&core_tests_root(repo_root)));
    paths.extend(rust_files(&core_examples_root(repo_root)));
    paths.extend(rust_files(&core_benches_root(repo_root)));
    paths.push(repo_root.join("crates/core/build.rs"));
    for path in paths {
        let content = fs::read_to_string(&path)?;
        let lines: Vec<&str> = content.lines().collect();
        let mut index = 0;
        while index < lines.len() {
            let start_index = index;
            let line = lines[index];
            let trimmed = line.trim();
            let mut attr_text = trimmed.to_owned();
            let starts_suppression_attr = trimmed.starts_with("#![allow(")
                || trimmed.starts_with("#[allow(")
                || trimmed.starts_with("#![expect(")
                || trimmed.starts_with("#[expect(")
                || trimmed.starts_with("#![cfg_attr(")
                || trimmed.starts_with("#[cfg_attr(");
            if starts_suppression_attr {
                while attr_text.contains("cfg_attr(")
                    && !attr_text.contains(']')
                    && index + 1 < lines.len()
                {
                    index += 1;
                    attr_text.push('\n');
                    attr_text.push_str(lines[index].trim());
                }
            }
            if starts_suppression_attr
                && (attr_text.contains("allow(") || attr_text.contains("expect("))
            {
                let justified = line_carries_justification(line, repo_root, &known_invariants)
                    || start_index
                        .checked_sub(1)
                        .and_then(|prev| lines.get(prev))
                        .map(|prev| line_carries_justification(prev, repo_root, &known_invariants))
                        .unwrap_or(false);
                ensure(
                    justified,
                    format!(
                        "unjustified lint suppression in {}:{} — every #[allow(...)], #[expect(...)], or cfg_attr-wrapped allow/expect must carry a `// justifies: <>=5 words + >=1 resolvable anchor>` comment. \
                         An anchor is an INV-id from traceability/invariants.yaml, an ADR-NNNN whose file exists as a root ADR file, \
                         or a concrete repo path (src/..., tests/..., examples/..., crates/macros/..., crates/macros-support/..., benches/..., tools/..., build.rs). \
                        See INV-ALLOW-IS-DESIGN.",
                        relative(repo_root, &path),
                        start_index + 1
                    ),
                )?;
            }
            index += 1;
        }
    }
    Ok(())
}

fn check_canonical_encoding_boundary(repo_root: &Path) -> Result<()> {
    for path in production_rust_files(repo_root) {
        let rel = relative(repo_root, &path);
        if rel == "crates/core/src/encoding.rs" {
            continue;
        }
        let content = fs::read_to_string(&path)?;
        for (line_no, line) in content.lines().enumerate() {
            if line.contains("rmp_serde::from_slice")
                || line.contains("rmp_serde::to_vec")
                || line.contains("rmp_serde::to_vec_named")
            {
                bail!(
                    "structural-check: raw rmp_serde call in {}:{}.\n\
                     Route production MessagePack through crate::encoding so ADR-0019 has one enforceable boundary.",
                    rel,
                    line_no + 1
                );
            }
        }
    }
    Ok(())
}

fn production_rust_files(repo_root: &Path) -> Vec<PathBuf> {
    let mut paths = rust_files(&core_src_root(repo_root));
    for rel in [
        "crates/macros/src",
        "crates/macros-support/src",
        "crates/syncbat-macros/src",
        "crates/syncbat/src",
        "crates/netbat/src",
        "crates/hbat/src",
    ] {
        paths.extend(rust_files(&repo_root.join(rel)));
    }
    paths
}
