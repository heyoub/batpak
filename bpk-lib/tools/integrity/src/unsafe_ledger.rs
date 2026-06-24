//! GAUNTLET-UNSAFE-LEDGER (kernel plan §10.8 / S-LEDGER plan:506).
//!
//! The fail-closed reconciliation between the `unsafe` blocks that live in the
//! sanctioned bvisor backend BASEMENTS (`crates/bvisor/src/backend/<os>/sys.rs`
//! or any file under a `backend/<os>/sys/` dir) and their justifications in
//! `traceability/unsafe_ledger.yaml`.
//!
//! Each `unsafe` block is matched to its ledger entry by a STABLE COMMENT ANCHOR
//! — a `LEDGER:<id>` token (`<id>` is kebab-case `[a-z0-9-]+`) embedded in the
//! contiguous `//` comment block IMMEDIATELY preceding the `unsafe` keyword (the
//! standard `// SAFETY:` comment site). Anchoring by id rather than `file:line`
//! lets blocks move freely as the basement source is edited.
//!
//! The architecture lint exempts these basement files from the blanket
//! sync-first/safe-Rust ban; THIS gate is the compensating control that makes the
//! exemption sound. It fails closed when:
//!   - a basement `unsafe` block carries NO `LEDGER:<id>` anchor (unanchored), OR
//!   - two live `unsafe` blocks share one anchor id (duplicate), OR
//!   - an anchored block has NO matching ledger entry (unregistered), OR
//!   - a ledger entry matches NO live anchored `unsafe` block (stale), OR
//!   - a ledger entry's `file` is not a sanctioned basement (outside).
//!
//! In step (a) the basements are empty, so the ledger is empty and the gate is
//! VACUOUSLY green — but LIVE, so step (b)'s first `unsafe` block is forced to
//! anchor + register before `structural-check` passes.

use crate::repo_surface::{ensure, load_yaml, relative, rust_files};
use crate::source_cache::SourceCache;
use anyhow::{Context, Result};
use regex::Regex;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
use syn::visit::Visit;

/// Repo-relative path to the unsafe ledger.
pub(crate) const LEDGER_REL: &str = "traceability/unsafe_ledger.yaml";

/// The bvisor backend root under which basements live.
const BACKEND_ROOT_REL: &str = "crates/bvisor/src/backend";

/// One ledger entry: a documented basement `unsafe` block, keyed by its anchor.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct LedgerEntry {
    /// Repo-relative path to the basement file.
    pub(crate) file: String,
    /// The `LEDGER:<id>` anchor id (kebab `[a-z0-9-]+`) in the block's SAFETY
    /// comment. Stable across line moves — this is what binds entry to block.
    pub(crate) anchor: String,
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

/// A live `unsafe` block found in a basement file: its file + 1-based line.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct UnsafeSite {
    pub(crate) file: String,
    pub(crate) line: usize,
}

/// A live `unsafe` block resolved to its comment anchor.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct AnchoredSite {
    pub(crate) file: String,
    pub(crate) line: usize,
    pub(crate) anchor: String,
}

impl AnchoredSite {
    fn location(&self) -> String {
        format!("{}:{}", self.file, self.line)
    }
}

/// Production entry: scan the basements, resolve anchors, load the ledger,
/// reconcile fail-closed.
pub(crate) fn check(repo_root: &Path, source_cache: &mut SourceCache) -> Result<()> {
    let ledger = load_ledger(repo_root)?;
    let anchored = collect_anchored_sites(repo_root, source_cache)?;
    reconcile(&anchored, &ledger)
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

/// Collect every basement `unsafe` block and resolve it to its comment anchor.
///
/// syn gives each block's line; comments are NOT in the AST, so the RAW source
/// of each basement file is also read and scanned for the `LEDGER:<id>` token in
/// the comment/attribute run immediately above the block (see [`resolve_anchors`]).
pub(crate) fn collect_anchored_sites(
    repo_root: &Path,
    source_cache: &mut SourceCache,
) -> Result<Vec<AnchoredSite>> {
    let mut anchored = Vec::new();
    for path in basement_files(repo_root) {
        let rel = relative(repo_root, &path);
        let parsed = source_cache
            .parse_rust(&path)
            .with_context(|| format!("parse {rel}"))?;
        let mut visitor = UnsafeSiteVisitor {
            file: rel.clone(),
            sites: Vec::new(),
        };
        visitor.visit_file(&parsed);
        let lines: Vec<usize> = visitor.sites.iter().map(|s| s.line).collect();
        let src = source_cache
            .read_to_string(&path)
            .with_context(|| format!("read {rel}"))?;
        anchored.extend(resolve_anchors(&rel, &src, &lines)?);
    }
    anchored.sort();
    Ok(anchored)
}

/// The `LEDGER:<id>` token regex (`<id>` is kebab `[a-z0-9-]+`).
const ANCHOR_PATTERN: &str = r"LEDGER:([a-z0-9-]+)";

/// Resolve each `unsafe` site (by 1-based line) to its comment anchor, if any.
///
/// For a site at line `L`, scan UPWARD from line `L-1` over the contiguous run of
/// lines that are `//` comments or `#[...]` attributes (stop at the first blank
/// line or other non-comment/non-attribute line); search that run for a
/// `LEDGER:(<id>)` token. A site with no token resolves to an EMPTY anchor — the
/// caller turns that into an `unanchored` violation, fail-closed.
pub(crate) fn resolve_anchors(
    file: &str,
    src: &str,
    unsafe_lines: &[usize],
) -> Result<Vec<AnchoredSite>> {
    // 1-based line index into the source text.
    let lines: Vec<&str> = src.lines().collect();
    let anchor_re =
        Regex::new(ANCHOR_PATTERN).with_context(|| format!("compile anchor regex for {file}"))?;
    Ok(unsafe_lines
        .iter()
        .map(|&line| {
            let anchor = anchor_for_line(&lines, line, &anchor_re).unwrap_or_default();
            AnchoredSite {
                file: file.to_string(),
                line,
                anchor,
            }
        })
        .collect())
}

/// Scan upward from `unsafe_line - 1` over the contiguous comment/attribute run
/// and return the first `LEDGER:<id>` id found, or `None`.
fn anchor_for_line(lines: &[&str], unsafe_line: usize, anchor_re: &Regex) -> Option<String> {
    if unsafe_line == 0 || unsafe_line > lines.len() {
        return None;
    }
    let mut found: Option<String> = None;
    // `unsafe_line` is 1-based; line above it is index `unsafe_line - 2`.
    let mut idx = unsafe_line.checked_sub(2);
    while let Some(i) = idx {
        let Some(raw) = lines.get(i) else { break };
        let trimmed = raw.trim_start();
        if trimmed.is_empty() {
            break;
        }
        if !(trimmed.starts_with("//") || trimmed.starts_with("#[")) {
            break;
        }
        if found.is_none() {
            if let Some(caps) = anchor_re.captures(raw) {
                if let Some(id) = caps.get(1) {
                    found = Some(id.as_str().to_string());
                }
            }
        }
        idx = i.checked_sub(1);
    }
    found
}

/// Reconcile live anchored sites against the ledger; fail closed in EITHER
/// direction. Violations (all tagged `[GAUNT-UNSAFE-LEDGER]`):
///   - `unanchored unsafe block at file:line`  — a live block has no `LEDGER:<id>`.
///   - `duplicate ledger anchor '<id>'`        — two live blocks share an id.
///   - `unregistered unsafe block (anchor '<id>')` — anchored block, no entry.
///   - `stale ledger entry (anchor '<id>')`    — entry, no live anchored block.
///   - ledger `file` outside a sanctioned basement.
pub(crate) fn reconcile(sites: &[AnchoredSite], ledger: &Ledger) -> Result<()> {
    let mut violations: Vec<String> = Vec::new();

    // Live anchors, by id → the (sorted) site that owns it. Unanchored + dup are
    // detected here; only uniquely-anchored live sites populate `live_anchors`.
    let mut live_anchors: BTreeMap<String, &AnchoredSite> = BTreeMap::new();
    let mut reported_dup: BTreeSet<String> = BTreeSet::new();
    for site in sites {
        if site.anchor.is_empty() {
            violations.push(format!(
                "{}: unanchored `unsafe` block at {} — add a `LEDGER:<id>` token to its \
                 `// SAFETY:` comment and a matching entry in {LEDGER_REL} [GAUNT-UNSAFE-LEDGER]",
                site.location(),
                site.location()
            ));
            continue;
        }
        match live_anchors.entry(site.anchor.clone()) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(site);
            }
            std::collections::btree_map::Entry::Occupied(_) => {
                // Report each duplicated id ONCE.
                if reported_dup.insert(site.anchor.clone()) {
                    violations.push(format!(
                        "{}: duplicate ledger anchor '{}' — two `unsafe` blocks share one anchor \
                         id; each block needs a unique `LEDGER:<id>` [GAUNT-UNSAFE-LEDGER]",
                        site.location(),
                        site.anchor
                    ));
                }
            }
        }
    }

    // Ledger entries, by id. Outside-basement is reported before id matching.
    let mut registered: BTreeSet<String> = BTreeSet::new();
    for entry in &ledger.entries {
        if !is_basement(&entry.file) {
            violations.push(format!(
                "{}: ledger entry (anchor '{}') points outside a sanctioned basement \
                 (`backend/<os>/sys.rs` or `sys/`). Remove it from {LEDGER_REL} \
                 [GAUNT-UNSAFE-LEDGER]",
                entry.file, entry.anchor
            ));
            continue;
        }
        registered.insert(entry.anchor.clone());
    }

    // Each uniquely-anchored live block must have a registered entry.
    for (anchor, site) in &live_anchors {
        if !registered.contains(anchor) {
            violations.push(format!(
                "{}: unregistered `unsafe` block (anchor '{anchor}') in a sanctioned basement. \
                 Add a ledger entry (file/anchor/syscall/safety_invariant/requirement) to \
                 {LEDGER_REL} [GAUNT-UNSAFE-LEDGER]",
                site.location()
            ));
        }
    }

    // Each registered (in-basement) entry must match a live anchored block.
    for anchor in &registered {
        if !live_anchors.contains_key(anchor) {
            violations.push(format!(
                "stale ledger entry (anchor '{anchor}') matches no live `unsafe` block. Remove it \
                 from {LEDGER_REL} (the ledger only tracks live unsafe) [GAUNT-UNSAFE-LEDGER]"
            ));
        }
    }

    violations.sort();
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
    use super::{is_basement, reconcile, resolve_anchors, AnchoredSite, Ledger, LedgerEntry};

    const BASEMENT: &str = "crates/bvisor/src/backend/linux/sys.rs";

    fn entry(anchor: &str) -> LedgerEntry {
        LedgerEntry {
            file: BASEMENT.to_string(),
            anchor: anchor.to_string(),
            syscall: "landlock".to_string(),
            safety_invariant: "documented".to_string(),
            requirement: "Filesystem".to_string(),
        }
    }

    fn site(line: usize, anchor: &str) -> AnchoredSite {
        AnchoredSite {
            file: BASEMENT.to_string(),
            line,
            anchor: anchor.to_string(),
        }
    }

    /// (g) Empty basements ⇒ empty ledger ⇒ pass, but the gate is LIVE.
    #[test]
    fn empty_ledger_with_no_unsafe_is_vacuously_green() {
        assert!(reconcile(&[], &Ledger::default()).is_ok());
    }

    /// The basement matcher mirrors the architecture-lint exemption set.
    #[test]
    fn basement_matcher_mirrors_the_lint_exemption() {
        assert!(is_basement("crates/bvisor/src/backend/linux/sys.rs"));
        assert!(is_basement("crates/bvisor/src/backend/windows/sys.rs"));
        assert!(is_basement("crates/bvisor/src/backend/linux/sys/raw.rs"));
        assert!(!is_basement("crates/bvisor/src/backend/linux/mod.rs"));
        assert!(!is_basement("crates/bvisor/src/contract/registry.rs"));
    }

    /// The association algorithm scans the contiguous comment/attribute run above
    /// each `unsafe` for `LEDGER:<id>`, stopping at the first blank/code line, and
    /// resolves an empty anchor when no token is present.
    #[test]
    fn resolve_anchors_reads_the_comment_run_above_each_block() {
        let src = "\
fn a() {
    // SAFETY (LEDGER:abc-one): documented form.
    #[rustfmt::skip]
    unsafe { ffi() }
}

fn b() {
    let x = 1; // not a comment-only line
    unsafe { ffi() }
}

fn c() {
    // SAFETY (LEDGER:should-not-bind): separated by a blank line below.

    unsafe { ffi() }
}
";
        // unsafe keywords are on lines 4, 9, 15 (1-based).
        let resolved = resolve_anchors("f.rs", src, &[4, 9, 15]).expect("anchor regex compiles");
        assert_eq!(resolved[0].anchor, "abc-one");
        // line 9: comment run stops at the `let x` code line above it (which has a
        // trailing comment but is not a comment-only line) ⇒ unanchored.
        assert!(resolved[1].anchor.is_empty());
        // line 15: blank line separates the comment ⇒ run stops ⇒ unanchored.
        assert!(resolved[2].anchor.is_empty());
    }

    /// (a) A matched anchor passes.
    #[test]
    fn matched_anchor_passes() {
        let ledger = Ledger {
            entries: vec![entry("linux-landlock-abi-probe")],
        };
        assert!(reconcile(&[site(73, "linux-landlock-abi-probe")], &ledger).is_ok());
    }

    /// (b) An unanchored live `unsafe` block fails closed.
    #[test]
    fn unanchored_unsafe_block_fails_closed() {
        let err = reconcile(&[site(42, "")], &Ledger::default())
            .expect_err("an unanchored unsafe block must fail closed");
        assert!(err.to_string().contains("unanchored `unsafe` block"));
    }

    /// (c) An anchored block with no ledger entry fails closed.
    #[test]
    fn unregistered_anchor_fails_closed() {
        let err = reconcile(&[site(42, "linux-orphan")], &Ledger::default())
            .expect_err("an unregistered anchored block must fail closed");
        assert!(err.to_string().contains("unregistered `unsafe` block"));
        assert!(err.to_string().contains("linux-orphan"));
    }

    /// (d) A ledger entry with no live anchored block fails closed.
    #[test]
    fn stale_ledger_entry_fails_closed() {
        let ledger = Ledger {
            entries: vec![entry("linux-vanished")],
        };
        let err = reconcile(&[], &ledger).expect_err("a stale ledger entry must fail closed");
        assert!(err.to_string().contains("stale ledger entry"));
        assert!(err.to_string().contains("linux-vanished"));
    }

    /// (e) Two live blocks sharing one anchor id fail closed.
    #[test]
    fn duplicate_anchor_fails_closed() {
        let ledger = Ledger {
            entries: vec![entry("linux-dup")],
        };
        let err = reconcile(&[site(10, "linux-dup"), site(20, "linux-dup")], &ledger)
            .expect_err("a duplicate anchor must fail closed");
        assert!(err.to_string().contains("duplicate ledger anchor"));
        assert!(err.to_string().contains("linux-dup"));
    }

    /// (f) A ledger entry pointing outside any basement fails closed.
    #[test]
    fn ledger_entry_outside_basement_fails_closed() {
        let ledger = Ledger {
            entries: vec![LedgerEntry {
                file: "crates/bvisor/src/backend/linux/mod.rs".to_string(),
                anchor: "linux-misplaced".to_string(),
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
