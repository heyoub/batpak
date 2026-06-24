//! GAUNTLET-UNSAFE-LEDGER (kernel plan §10.8 / S-LEDGER plan:506).
//!
//! The fail-closed reconciliation between the `unsafe` blocks that live in the
//! sanctioned bvisor backend BASEMENTS (`crates/bvisor/src/backend/<os>/sys.rs`
//! or any file under a `backend/<os>/sys/` dir) and their justifications in
//! `traceability/unsafe_ledger.yaml`.
//!
//! The architecture lint exempts these basement files from the blanket
//! sync-first/safe-Rust ban; THIS gate is the compensating control that makes the
//! exemption sound. It fails closed when:
//!   - a basement `unsafe` block has NO matching ledger entry (unregistered), OR
//!   - a ledger entry matches NO live basement `unsafe` block (stale).
//!
//! In step (a) the basements are empty, so the ledger is empty and the gate is
//! VACUOUSLY green — but LIVE, so step (b)'s first `unsafe` block is forced to
//! register before `structural-check` passes.

use crate::repo_surface::{ensure, load_yaml, relative, rust_files};
use crate::source_cache::SourceCache;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
use syn::visit::Visit;

/// Repo-relative path to the unsafe ledger.
pub(crate) const LEDGER_REL: &str = "traceability/unsafe_ledger.yaml";

/// The bvisor backend root under which basements live.
const BACKEND_ROOT_REL: &str = "crates/bvisor/src/backend";

/// One ledger entry: a documented basement `unsafe` block.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct LedgerEntry {
    /// Repo-relative path to the basement file.
    pub(crate) file: String,
    /// 1-based line of the `unsafe` block's `unsafe` keyword.
    pub(crate) line: usize,
    /// The syscall / FFI call the block performs.
    pub(crate) syscall: String,
    /// The safety invariant the caller upholds.
    pub(crate) safety_invariant: String,
    /// The `RequirementKind` the unsafe backs.
    pub(crate) requirement: String,
}

/// The ledger document.
#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct Ledger {
    #[serde(default)]
    pub(crate) entries: Vec<LedgerEntry>,
}

/// A live `unsafe` block found in a basement file: its `file:line` key.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct UnsafeSite {
    pub(crate) file: String,
    pub(crate) line: usize,
}

impl UnsafeSite {
    fn key(&self) -> String {
        format!("{}:{}", self.file, self.line)
    }
}

/// Production entry: scan the basements, load the ledger, reconcile fail-closed.
pub(crate) fn check(repo_root: &Path, source_cache: &mut SourceCache) -> Result<()> {
    let ledger = load_ledger(repo_root)?;
    let sites = collect_unsafe_sites(repo_root, source_cache)?;
    reconcile(&sites, &ledger)
}

/// Load the ledger; an absent file is the empty ledger (first-run).
pub(crate) fn load_ledger(repo_root: &Path) -> Result<Ledger> {
    let path = repo_root.join(LEDGER_REL);
    if !path.exists() {
        return Ok(Ledger::default());
    }
    load_yaml(&path)
}

/// The basement files: `backend/<os>/sys.rs` or any file under `backend/<os>/sys/`.
pub(crate) fn basement_files(repo_root: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = rust_files(&repo_root.join(BACKEND_ROOT_REL))
        .into_iter()
        .filter(|path| is_basement(&relative(repo_root, path)))
        .collect();
    files.sort();
    files
}

/// Whether a repo-relative path is a sanctioned basement file. Mirrors the
/// architecture lint's `is_unsafe_basement` exactly (kept in lockstep on purpose:
/// the lint EXEMPTS exactly the set this gate RECONCILES).
fn is_basement(rel: &str) -> bool {
    rel.contains("crates/bvisor/src/backend/")
        && (rel.ends_with("/sys.rs") || rel.contains("/sys/"))
}

/// Collect every `unsafe` block site across the basements.
pub(crate) fn collect_unsafe_sites(
    repo_root: &Path,
    source_cache: &mut SourceCache,
) -> Result<Vec<UnsafeSite>> {
    let mut sites = Vec::new();
    for path in basement_files(repo_root) {
        let parsed = source_cache
            .parse_rust(&path)
            .with_context(|| format!("parse {}", relative(repo_root, &path)))?;
        let rel = relative(repo_root, &path);
        let mut visitor = UnsafeSiteVisitor {
            file: rel,
            sites: Vec::new(),
        };
        visitor.visit_file(&parsed);
        sites.extend(visitor.sites);
    }
    sites.sort();
    Ok(sites)
}

/// Reconcile live sites against the ledger; fail closed on EITHER direction.
pub(crate) fn reconcile(sites: &[UnsafeSite], ledger: &Ledger) -> Result<()> {
    let live: BTreeSet<String> = sites.iter().map(UnsafeSite::key).collect();
    let registered: BTreeSet<String> = ledger
        .entries
        .iter()
        .map(|e| format!("{}:{}", e.file, e.line))
        .collect();

    let mut violations: Vec<String> = Vec::new();

    for site in sites {
        if !registered.contains(&site.key()) {
            violations.push(format!(
                "{}: unregistered `unsafe` block in a sanctioned basement. Add a ledger entry \
                 (file/line/syscall/safety_invariant/requirement) to {LEDGER_REL} \
                 [GAUNT-UNSAFE-LEDGER]",
                site.key()
            ));
        }
    }
    for entry in &ledger.entries {
        let key = format!("{}:{}", entry.file, entry.line);
        if !is_basement(&entry.file) {
            violations.push(format!(
                "{key}: ledger entry points outside a sanctioned basement \
                 (`backend/<os>/sys.rs` or `sys/`). Remove it from {LEDGER_REL} \
                 [GAUNT-UNSAFE-LEDGER]"
            ));
            continue;
        }
        if !live.contains(&key) {
            violations.push(format!(
                "{key}: stale ledger entry matches no live `unsafe` block. Remove it from \
                 {LEDGER_REL} (the ledger only tracks live unsafe) [GAUNT-UNSAFE-LEDGER]"
            ));
        }
    }

    ensure(
        violations.is_empty(),
        format!(
            "structural-check (unsafe-ledger): {} violation(s) [GAUNT-UNSAFE-LEDGER]:\n  {}",
            violations.len(),
            violations.join("\n  ")
        ),
    )
}

/// A syn visitor recording the line of every `unsafe` block in one file.
struct UnsafeSiteVisitor {
    file: String,
    sites: Vec<UnsafeSite>,
}

impl<'ast> Visit<'ast> for UnsafeSiteVisitor {
    fn visit_expr_unsafe(&mut self, node: &'ast syn::ExprUnsafe) {
        self.sites.push(UnsafeSite {
            file: self.file.clone(),
            line: node.unsafe_token.span().start().line,
        });
        syn::visit::visit_expr_unsafe(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::{is_basement, reconcile, Ledger, LedgerEntry, UnsafeSite};

    #[test]
    fn empty_ledger_with_no_unsafe_is_vacuously_green() {
        // Step (a): empty basements ⇒ empty ledger ⇒ pass, but the gate is LIVE.
        assert!(reconcile(&[], &Ledger::default()).is_ok());
    }

    #[test]
    fn basement_matcher_mirrors_the_lint_exemption() {
        assert!(is_basement("crates/bvisor/src/backend/linux/sys.rs"));
        assert!(is_basement("crates/bvisor/src/backend/windows/sys.rs"));
        assert!(is_basement("crates/bvisor/src/backend/linux/sys/raw.rs"));
        // The safe orchestration + contract are NOT basements.
        assert!(!is_basement("crates/bvisor/src/backend/linux/mod.rs"));
        assert!(!is_basement("crates/bvisor/src/contract/registry.rs"));
    }

    #[test]
    fn unregistered_unsafe_block_fails_closed() {
        let site = UnsafeSite {
            file: "crates/bvisor/src/backend/linux/sys.rs".to_string(),
            line: 42,
        };
        let err = reconcile(&[site], &Ledger::default())
            .expect_err("an unregistered unsafe block must fail closed");
        assert!(err.to_string().contains("unregistered `unsafe`"));
    }

    #[test]
    fn stale_ledger_entry_fails_closed() {
        let ledger = Ledger {
            entries: vec![LedgerEntry {
                file: "crates/bvisor/src/backend/linux/sys.rs".to_string(),
                line: 7,
                syscall: "clone3".to_string(),
                safety_invariant: "args validated".to_string(),
                requirement: "ChildSpawn".to_string(),
            }],
        };
        let err = reconcile(&[], &ledger).expect_err("a stale ledger entry must fail closed");
        assert!(err.to_string().contains("stale ledger entry"));
    }

    #[test]
    fn matched_entry_passes() {
        let site = UnsafeSite {
            file: "crates/bvisor/src/backend/linux/sys.rs".to_string(),
            line: 7,
        };
        let ledger = Ledger {
            entries: vec![LedgerEntry {
                file: "crates/bvisor/src/backend/linux/sys.rs".to_string(),
                line: 7,
                syscall: "clone3".to_string(),
                safety_invariant: "args validated".to_string(),
                requirement: "ChildSpawn".to_string(),
            }],
        };
        assert!(reconcile(&[site], &ledger).is_ok());
    }

    #[test]
    fn ledger_entry_outside_basement_fails_closed() {
        let ledger = Ledger {
            entries: vec![LedgerEntry {
                file: "crates/bvisor/src/backend/linux/mod.rs".to_string(),
                line: 1,
                syscall: "x".to_string(),
                safety_invariant: "y".to_string(),
                requirement: "z".to_string(),
            }],
        };
        let err = reconcile(&[], &ledger)
            .expect_err("a ledger entry outside a basement must fail closed");
        assert!(err.to_string().contains("outside a sanctioned basement"));
    }
}
