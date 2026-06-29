//! INV-PERFORMANCE-GATES-ENFORCED: hardware-dependent perf gates stay ignored
//! and are runnable through the repo-owned `cargo xtask perf-gates` surface.

use crate::repo_surface::{core_tests_root, relative};
use crate::source_cache::SourceCache;
use anyhow::{anyhow, bail, Context, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const PERF_TEST_STEMS: &[&str] = &[
    "perf_gates",
    "perf_gates_throughput_latency",
    "perf_gates_cold_start",
    "perf_gates_correctness",
];

#[derive(Debug, Eq, PartialEq)]
struct PerfTestFn {
    name: String,
    line: usize,
    has_ignore: bool,
    ignore_mentions_xtask: bool,
}

pub(crate) fn check(repo_root: &Path, source_cache: &mut SourceCache) -> Result<BTreeSet<PathBuf>> {
    let mut inputs = BTreeSet::new();
    for stem in PERF_TEST_STEMS {
        let path = core_tests_root(repo_root).join(format!("{stem}.rs"));
        inputs.insert(path.clone());
        check_perf_test_file(repo_root, &path, source_cache)?;
    }

    let xtask_ci = repo_root.join("tools/xtask/src/commands/ci.rs");
    let xtask_main = repo_root.join("tools/xtask/src/main.rs");
    inputs.insert(xtask_ci.clone());
    inputs.insert(xtask_main.clone());
    check_xtask_perf_surface(repo_root, &xtask_ci, &xtask_main, source_cache)?;

    Ok(inputs)
}

fn check_perf_test_file(
    repo_root: &Path,
    path: &Path,
    source_cache: &mut SourceCache,
) -> Result<()> {
    let rel = relative(repo_root, path);
    let file = source_cache
        .parse_rust(path)
        .map_err(|err| anyhow!("parse perf gate file {rel}: {err}"))?;
    let tests = collect_test_fns(&file);
    for test in tests {
        if !is_hardware_perf_gate_name(&test.name) && !test.has_ignore {
            continue;
        }
        if !test.has_ignore {
            bail!(
                "perf-gates-contract (INV-PERFORMANCE-GATES-ENFORCED): hardware-dependent \
                 perf test `{}` in {}:{} is not #[ignore]d. Perf gates must not run in the \
                 ordinary test lane; route them through `cargo xtask perf-gates`.",
                test.name,
                rel,
                test.line,
            );
        }
        if !test.ignore_mentions_xtask {
            bail!(
                "perf-gates-contract (INV-PERFORMANCE-GATES-ENFORCED): ignored perf test `{}` \
                 in {}:{} does not name `cargo xtask perf-gates` in its ignore reason.",
                test.name,
                rel,
                test.line,
            );
        }
    }
    Ok(())
}

fn collect_test_fns(file: &syn::File) -> Vec<PerfTestFn> {
    file.items
        .iter()
        .filter_map(|item| {
            let syn::Item::Fn(item_fn) = item else {
                return None;
            };
            let has_test = item_fn
                .attrs
                .iter()
                .any(|attr| attr.path().is_ident("test"));
            if !has_test {
                return None;
            }
            let ignore_attr = item_fn
                .attrs
                .iter()
                .find(|attr| attr.path().is_ident("ignore"));
            Some(PerfTestFn {
                name: item_fn.sig.ident.to_string(),
                line: item_fn.sig.ident.span().start().line,
                has_ignore: ignore_attr.is_some(),
                ignore_mentions_xtask: ignore_attr.is_some_and(ignore_mentions_xtask),
            })
        })
        .collect()
}

fn ignore_mentions_xtask(attr: &syn::Attribute) -> bool {
    let syn::Meta::NameValue(meta) = &attr.meta else {
        return false;
    };
    let syn::Expr::Lit(expr_lit) = &meta.value else {
        return false;
    };
    let syn::Lit::Str(message) = &expr_lit.lit else {
        return false;
    };
    message.value().contains("cargo xtask perf-gates")
}

fn is_hardware_perf_gate_name(name: &str) -> bool {
    [
        "throughput",
        "latency",
        "under_threshold",
        "performance_feedback",
        "performance_gate",
        "cold_path",
        "open_only",
        "close_only",
        "restore",
    ]
    .iter()
    .any(|needle| name.contains(needle))
}

fn check_xtask_perf_surface(
    repo_root: &Path,
    xtask_ci: &Path,
    xtask_main: &Path,
    source_cache: &mut SourceCache,
) -> Result<()> {
    let ci_rel = relative(repo_root, xtask_ci);
    let main_rel = relative(repo_root, xtask_main);
    let ci = source_cache
        .read_to_string(xtask_ci)
        .with_context(|| format!("read {ci_rel}"))?;
    let main = source_cache
        .read_to_string(xtask_main)
        .with_context(|| format!("read {main_rel}"))?;

    ensure_contains(&ci, "pub(crate) fn perf_gates() -> Result<()>", &ci_rel)?;
    ensure_contains(&ci, "--run-ignored", &ci_rel)?;
    ensure_contains(&ci, "only", &ci_rel)?;
    for stem in PERF_TEST_STEMS {
        ensure_contains(&ci, stem, &ci_rel)?;
    }
    ensure_contains(
        &main,
        "XtaskCommand::PerfGates => commands::perf_gates()",
        &main_rel,
    )?;
    Ok(())
}

fn ensure_contains(contents: &str, needle: &str, rel: &str) -> Result<()> {
    if contents.contains(needle) {
        return Ok(());
    }
    bail!(
        "perf-gates-contract (INV-PERFORMANCE-GATES-ENFORCED): {rel} does not contain `{needle}`. \
         Hardware-dependent perf tests must stay runnable through the repo-owned \
         `cargo xtask perf-gates` command surface."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_repo(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "batpak-perf-gates-contract-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp repo");
        path
    }

    fn write_file(repo: &Path, rel: &str, body: &str) {
        let path = repo.join(rel);
        fs::create_dir_all(path.parent().expect("parent dir")).expect("create dirs");
        fs::write(path, body).expect("write fixture file");
    }

    fn write_green_xtask(repo: &Path) {
        write_file(
            repo,
            "tools/xtask/src/commands/ci.rs",
            "pub(crate) fn perf_gates() -> Result<()> {\n\
                 run_nextest_ci([\n\
                     \"--test\", \"perf_gates\",\n\
                     \"--test\", \"perf_gates_throughput_latency\",\n\
                     \"--test\", \"perf_gates_cold_start\",\n\
                     \"--test\", \"perf_gates_correctness\",\n\
                     \"--run-ignored\", \"only\",\n\
                 ])\n\
             }\n",
        );
        write_file(
            repo,
            "tools/xtask/src/main.rs",
            "fn dispatch(command: XtaskCommand) -> Result<()> {\n\
                 match command {\n\
                     XtaskCommand::PerfGates => commands::perf_gates(),\n\
                 }\n\
             }\n",
        );
    }

    fn write_green_perf_tests(repo: &Path) {
        for stem in PERF_TEST_STEMS {
            write_file(
                repo,
                &format!("crates/core/tests/{stem}.rs"),
                "#[test]\n\
                 #[ignore = \"hardware-dependent perf gate — run via `cargo xtask perf-gates`.\"]\n\
                 fn append_throughput_gate() {}\n\
                 #[test]\n\
                 fn correctness_gates_fire_on_violations() {}\n",
            );
        }
    }

    #[test]
    fn perf_contract_rejects_unignored_hardware_gate_and_missing_xtask_surface() {
        let repo = temp_repo("red");
        write_green_xtask(&repo);
        write_green_perf_tests(&repo);
        let mut cache = SourceCache::new(&repo);
        check(&repo, &mut cache).expect("green perf contract fixture passes");

        write_file(
            &repo,
            "crates/core/tests/perf_gates.rs",
            "#[test]\nfn append_throughput_gate() {}\n",
        );
        let mut cache = SourceCache::new(&repo);
        let err = check(&repo, &mut cache).expect_err("unignored perf gate is rejected");
        assert!(err.to_string().contains("not #[ignore]d"), "{err:?}");

        write_green_perf_tests(&repo);
        write_file(
            &repo,
            "tools/xtask/src/commands/ci.rs",
            "pub(crate) fn perf_gates() -> Result<()> {\n\
                 run_nextest_ci([\"--test\", \"perf_gates\"])\n\
             }\n",
        );
        let mut cache = SourceCache::new(&repo);
        let err = check(&repo, &mut cache).expect_err("missing --run-ignored is rejected");
        assert!(err.to_string().contains("--run-ignored"), "{err:?}");

        fs::remove_dir_all(repo).expect("remove temp repo");
    }
}
