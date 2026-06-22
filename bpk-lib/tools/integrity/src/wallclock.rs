//! Wall-clock-in-correctness-tests detector (GAUNT-FLAKE-7 sub-item, slug
//! `no-wallclock-asserts`).
//!
//! A wall-clock assertion in a correctness test is a flake source: it asserts a
//! property of the host's scheduler, not of batpak. This check scans
//! `crates/core/tests/**` (EXCLUDING `perf_gates*.rs`, where wall-clock floors
//! are deliberate and `#[ignore]`d) for a function that BOTH starts a wall clock
//! (`Instant::now()`) AND asserts on the elapsed duration
//! (`assert*!(... .elapsed() ...)` or an `assert*!` mentioning a `Duration`
//! comparison) in the same function body. Such pairs are reported.
//!
//! Current offenders are pinned in an allowlist
//! (`traceability/wallclock_allowlist.yaml`) so the gate lands GREEN; the list
//! can only shrink. Anti-rot: an allowlist entry that no longer matches a live
//! offender is reported.

use crate::repo_surface::{core_tests_root, ensure, load_yaml, relative, rust_files};
use crate::source_cache::SourceCache;
use anyhow::Result;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

/// Repo-relative path to the offender allowlist data file.
pub(crate) const ALLOWLIST_REL: &str = "traceability/wallclock_allowlist.yaml";

/// One pinned current offender: `<repo-relative test file>::<fn name>`.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct AllowEntry {
    pub(crate) key: String,
}

/// Production entry: scan the core integration-test surface and enforce.
pub(crate) fn check(repo_root: &Path, source_cache: &mut SourceCache) -> Result<()> {
    let files = test_files(repo_root);
    let allowlist = load_allowlist(repo_root)?;
    let offenders = collect_offenders(repo_root, &files, source_cache)?;
    enforce(&offenders, &allowlist)
}

/// The integration-test files scanned: `crates/core/tests/**/*.rs` minus any
/// `perf_gates*.rs` (deliberate, ignored wall-clock floors live there).
pub(crate) fn test_files(repo_root: &Path) -> Vec<PathBuf> {
    let mut files = rust_files(&core_tests_root(repo_root));
    files.retain(|path| {
        !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("perf_gates"))
    });
    files.sort();
    files
}

/// Load the offender allowlist, empty when absent (first-run / report mode).
pub(crate) fn load_allowlist(repo_root: &Path) -> Result<Vec<AllowEntry>> {
    let path = repo_root.join(ALLOWLIST_REL);
    if !path.exists() {
        return Ok(Vec::new());
    }
    load_yaml(&path)
}

/// Enforce: every offender must be pinned in `allowlist`. Anti-rot: a pin that
/// matches no live offender is reported.
pub(crate) fn enforce(offenders: &[String], allowlist: &[AllowEntry]) -> Result<()> {
    let pinned: BTreeSet<&str> = allowlist.iter().map(|e| e.key.as_str()).collect();
    let live: BTreeSet<&str> = offenders.iter().map(String::as_str).collect();

    let mut violations: Vec<String> = Vec::new();
    for offender in offenders {
        if !pinned.contains(offender.as_str()) {
            violations.push(format!(
                "{offender}: wall-clock assertion in a correctness test (Instant::now() paired \
                 with an `.elapsed()`/Duration assert). This is a flake source — drive timing with \
                 an injected Clock and assert the logical value, or move it to a `perf_gates*.rs` \
                 lane. [GAUNT-FLAKE-7]"
            ));
        }
    }
    for entry in allowlist {
        if !live.contains(entry.key.as_str()) {
            violations.push(format!(
                "{}: wall-clock allowlist pin matches no live offender. Remove the stale entry \
                 from {ALLOWLIST_REL} (the allowlist only shrinks).",
                entry.key
            ));
        }
    }

    ensure(
        violations.is_empty(),
        format!(
            "structural-check (no-wallclock-asserts): {} violation(s) [GAUNT-FLAKE-7]:\n  {}",
            violations.len(),
            violations.join("\n  ")
        ),
    )
}

/// Parse every test file and return the `<file>::<fn>` key of each function that
/// pairs `Instant::now()` with an elapsed/Duration assertion.
pub(crate) fn collect_offenders(
    repo_root: &Path,
    paths: &[PathBuf],
    source_cache: &mut SourceCache,
) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for path in paths {
        let rel = relative(repo_root, path);
        // Test files may use unstable / edition features the integrity parser
        // tolerates; skip unparseable ones rather than fail the whole gate.
        let Some(file) = source_cache.parse_rust_if_valid(path)? else {
            continue;
        };
        let mut visitor = TestFnVisitor {
            rel: &rel,
            out: &mut out,
        };
        visitor.visit_file(&file);
    }
    out.sort();
    Ok(out)
}

struct TestFnVisitor<'a> {
    rel: &'a str,
    out: &'a mut Vec<String>,
}

impl<'a, 'ast> Visit<'ast> for TestFnVisitor<'a> {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        let text = quote::quote!(#node).to_string();
        if function_body_has_wallclock_assert(&text) {
            self.out.push(format!("{}::{}", self.rel, node.sig.ident));
        }
        syn::visit::visit_item_fn(self, node);
    }
}

/// True when `text` (a function's token stream rendered to a string) both starts
/// a wall clock and asserts on elapsed time. Token-stream rendering normalizes
/// whitespace, so we match on the canonical token spellings.
pub(crate) fn function_body_has_wallclock_assert(text: &str) -> bool {
    let starts_clock = text.contains("Instant :: now")
        || text.contains("Instant::now")
        || text.contains("SystemTime :: now")
        || text.contains("SystemTime::now");
    if !starts_clock {
        return false;
    }
    (text.contains("assert") || text.contains("debug_assert"))
        && (text.contains(". elapsed")
            || text.contains(".elapsed")
            || text.contains("Duration :: from")
            || text.contains("Duration::from")
            || text.contains("as_millis")
            || text.contains("as_micros")
            || text.contains("as_secs")
            || text.contains("as_nanos"))
}
