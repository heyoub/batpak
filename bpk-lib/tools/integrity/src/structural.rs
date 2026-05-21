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
use syn::spanned::Spanned;

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
    check_no_store_read_dir_entry_error_swallowing(&repo_root)?;
    check_store_segment_classification_boundary(&repo_root)?;
    check_allow_justifications(&repo_root)?;
    check_rust_file_size_pressure(&repo_root)?;
    check_inline_test_island_pressure(&repo_root)?;
    public_surface::check(&repo_root)?;
    ci_parity::check(&repo_root)?;
    store_pub_fn_coverage::check(&repo_root)?;
    println!("structural-check: ok");
    Ok(())
}

fn check_rust_file_size_pressure(repo_root: &Path) -> Result<()> {
    const DEFAULT_LINE_BUDGET: usize = 850;
    const RATCHELED_OVER_BUDGET_FILES: &[(&str, usize)] = &[
        ("crates/core/src/store/index/mod.rs", 835),
        ("crates/macros/src/lib.rs", 849),
    ];

    for path in production_rust_files(repo_root) {
        let rel = relative(repo_root, &path);
        let content = fs::read_to_string(&path)?;
        let line_count = nonblank_line_count(&content);
        let budget = RATCHELED_OVER_BUDGET_FILES
            .iter()
            .find_map(|(known_rel, budget)| (*known_rel == rel).then_some(*budget))
            .unwrap_or(DEFAULT_LINE_BUDGET);
        ensure(
            line_count <= budget,
            format!(
                "structural-check: production Rust file size pressure in {rel}: {line_count} lines exceeds budget {budget}.\n\
                 New production files must stay at or below {DEFAULT_LINE_BUDGET} nonblank lines. \
                 Existing oversized files are ratcheted at their current ceiling until they are extracted."
            ),
        )?;
    }
    Ok(())
}

fn check_inline_test_island_pressure(repo_root: &Path) -> Result<()> {
    const DEFAULT_TEST_ISLAND_BUDGET: usize = 200;
    const RATCHELED_OVER_BUDGET_TEST_ISLANDS: &[(&str, usize)] =
        &[("crates/core/src/store/segment/scan/mod.rs", 351)];

    for path in production_rust_files(repo_root) {
        let rel = relative(repo_root, &path);
        let content = fs::read_to_string(&path)?;
        let file = syn::parse_file(&content)
            .map_err(|err| anyhow!("parse inline test islands in {rel}: {err}"))?;
        let lines = content.lines().collect::<Vec<_>>();
        for island in inline_test_islands(&file, &lines) {
            let budget = RATCHELED_OVER_BUDGET_TEST_ISLANDS
                .iter()
                .find_map(|(known_rel, budget)| (*known_rel == rel).then_some(*budget))
                .unwrap_or(DEFAULT_TEST_ISLAND_BUDGET);
            ensure(
                island.nonblank_lines <= budget,
                format!(
                    "structural-check: oversized inline `mod tests` island in {rel}:{}-{} has {} nonblank lines, exceeding budget {budget}.\n\
                     New inline test islands in production src files must stay at or below {DEFAULT_TEST_ISLAND_BUDGET} nonblank lines. \
                     Existing oversized islands are ratcheted at their current ceiling; extract growth into integration tests or focused test modules.",
                    island.start_line,
                    island.end_line,
                    island.nonblank_lines
                ),
            )?;
        }
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

fn check_no_store_read_dir_entry_error_swallowing(repo_root: &Path) -> Result<()> {
    for path in production_rust_files(repo_root) {
        let rel = relative(repo_root, &path);
        if !rel.starts_with("crates/core/src/store/") {
            continue;
        }
        let content = fs::read_to_string(&path)?;
        let lines = content.lines().collect::<Vec<_>>();
        for line_no in 0..lines.len() {
            if read_dir_entry_error_is_swallowed(&lines, line_no) {
                bail!(
                    "structural-check: read_dir entry errors must not be swallowed in {rel}:{}.\n\
                     Collect or iterate directory entries as Result values and propagate DirEntry errors through StoreError.",
                    line_no + 1
                );
            }
        }
    }
    Ok(())
}

fn check_store_segment_classification_boundary(repo_root: &Path) -> Result<()> {
    for path in production_rust_files(repo_root) {
        let rel = relative(repo_root, &path);
        if !rel.starts_with("crates/core/src/store/")
            || rel == "crates/core/src/store/file_classification.rs"
            || rel == "crates/core/src/store/segment/mod.rs"
        {
            continue;
        }
        let content = fs::read_to_string(&path)?;
        let lines = content.lines().collect::<Vec<_>>();
        for line_no in 0..lines.len() {
            if local_segment_extension_classification(&lines, line_no) {
                bail!(
                    "structural-check: local segment filename classification in {rel}:{}.\n\
                     Use StoreFileKind::from_path so malformed segment names, cold-start artifacts, snapshots, and lifecycle cleanup share one store-owned classifier.",
                    line_no + 1
                );
            }
        }
    }
    Ok(())
}

fn read_dir_entry_error_is_swallowed(lines: &[&str], line_no: usize) -> bool {
    if !line_swallows_iterator_error(lines[line_no]) {
        return false;
    }

    let start = line_no.saturating_sub(4);
    let end = (line_no + 5).min(lines.len());
    lines[start..end]
        .iter()
        .any(|line| code_line_contains(line, "read_dir("))
}

fn local_segment_extension_classification(lines: &[&str], line_no: usize) -> bool {
    let start = line_no.saturating_sub(3);
    let end = (line_no + 4).min(lines.len());
    let window = &lines[start..end];
    window
        .iter()
        .any(|line| code_line_contains(line, ".extension()"))
        && window
            .iter()
            .any(|line| code_line_contains(line, "SEGMENT_EXTENSION"))
}

fn line_swallows_iterator_error(line: &str) -> bool {
    code_line_contains(line, ".filter_map(Result::ok)")
        || (code_line_contains(line, ".filter_map(|") && code_line_contains(line, ".ok())"))
        || code_line_contains(line, ".flatten()")
}

fn code_line_contains(line: &str, needle: &str) -> bool {
    let trimmed = line.trim_start();
    !(trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with("//!"))
        && line.contains(needle)
}

#[derive(Debug, Eq, PartialEq)]
struct InlineTestIsland {
    start_line: usize,
    end_line: usize,
    nonblank_lines: usize,
}

fn inline_test_islands(file: &syn::File, source_lines: &[&str]) -> Vec<InlineTestIsland> {
    let mut islands = Vec::new();
    collect_inline_test_islands(&file.items, source_lines, &mut islands);
    islands
}

fn collect_inline_test_islands(
    items: &[syn::Item],
    source_lines: &[&str],
    islands: &mut Vec<InlineTestIsland>,
) {
    for item in items {
        let syn::Item::Mod(module) = item else {
            continue;
        };
        if module.ident == "tests" && module.content.is_some() {
            let span = module.span();
            let start_line = span.start().line;
            let end_line = span.end().line;
            islands.push(InlineTestIsland {
                start_line,
                end_line,
                nonblank_lines: nonblank_line_count_in_range(source_lines, start_line, end_line),
            });
        }
        if let Some((_, nested_items)) = &module.content {
            collect_inline_test_islands(nested_items, source_lines, islands);
        }
    }
}

fn nonblank_line_count(content: &str) -> usize {
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
}

fn nonblank_line_count_in_range(lines: &[&str], start_line: usize, end_line: usize) -> usize {
    if start_line == 0 || end_line < start_line {
        return 0;
    }
    lines
        .iter()
        .skip(start_line - 1)
        .take(end_line - start_line + 1)
        .filter(|line| !line.trim().is_empty())
        .count()
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

#[cfg(test)]
mod tests {
    use super::{
        inline_test_islands, local_segment_extension_classification, nonblank_line_count,
        read_dir_entry_error_is_swallowed,
    };

    #[test]
    fn file_size_ratchet_counts_nonblank_lines() {
        assert_eq!(nonblank_line_count("one\n\n  \n two\n"), 2);
    }

    #[test]
    fn inline_test_island_detection_counts_inline_tests_only() {
        let source = r#"
mod production {
    fn helper() {}

    mod tests {
        #[test]
        fn nested_island() {}

    }
}

mod tests {
    #[test]
    fn top_level_island() {}

}

mod external_tests;
"#;
        let file = syn::parse_file(source).expect("parse fixture");
        let lines = source.lines().collect::<Vec<_>>();
        let islands = inline_test_islands(&file, &lines);

        assert_eq!(islands.len(), 2);
        assert_eq!(islands[0].nonblank_lines, 4);
        assert_eq!(islands[1].nonblank_lines, 4);
    }

    #[test]
    fn read_dir_swallow_gate_is_scoped_to_directory_entries() {
        let unrelated = [
            "fn helper(items: Vec<Option<u8>>) {",
            "    let _values = items.into_iter().flatten().collect::<Vec<_>>();",
            "}",
        ];
        assert!(!read_dir_entry_error_is_swallowed(&unrelated, 1));

        let directory = [
            "fn helper(path: &Path) {",
            "    let _paths = std::fs::read_dir(path)",
            "        .expect(\"read dir\")",
            "        .flatten()",
            "        .map(|entry| entry.path())",
            "        .collect::<Vec<_>>();",
            "}",
        ];
        assert!(read_dir_entry_error_is_swallowed(&directory, 3));
    }

    #[test]
    fn segment_classification_gate_allows_generation_but_rejects_extension_tests() {
        let generation = [
            "fn segment_path(segment_id: u64) -> String {",
            "    format!(\"{segment_id:06}.{}\", SEGMENT_EXTENSION)",
            "}",
        ];
        assert!(!local_segment_extension_classification(&generation, 1));

        let classification = [
            "fn helper(path: &Path) -> bool {",
            "    path",
            "        .extension()",
            "        .map(|ext| ext == segment::SEGMENT_EXTENSION)",
            "        .unwrap_or(false)",
            "}",
        ];
        assert!(local_segment_extension_classification(&classification, 2));
    }
}
