//! Function-complexity gate (GAUNT-CPLX-6, slug `function-complexity`).
//!
//! A syn-based structural lint over the production-Rust surface. Per `fn` /
//! method body it measures three hardware-independent metrics and enforces a
//! budget on each:
//!
//!   * nonblank body lines  <= [`FN_LINE_BUDGET`]
//!   * max block-nesting     <= [`NEST_BUDGET`]
//!   * cyclomatic proxy      <= [`CYCLO_BUDGET`]  (count of
//!     `if` / `match`-arm / `while` / `for` / `&&` / `||` / `?`)
//!
//! The gate lands GREEN on today's tree via a RATCHET allowlist
//! (`traceability/complexity_ratchet.yaml`): each current over-budget function is
//! pinned at its CURRENT measured values. An allowlisted function passes only
//! while it stays at-or-below its pinned values — so the allowlist can only
//! shrink (a function that regresses past its pin, or a brand-new over-budget
//! function, fails). Anti-rot: an allowlist entry whose function is now within
//! budget, or that names a function no longer present, is reported.
//!
//! Mirrors the existing `check_rust_file_size_pressure` ratchet discipline:
//! "split, don't bump". Reuses the syn parse via `SourceCache` and the
//! `production_rust_files` surface.

use crate::repo_surface::{ensure, load_yaml, relative};
use crate::source_cache::SourceCache;
use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
use syn::visit::Visit;

/// Max nonblank lines in a single function body (between the braces).
pub(crate) const FN_LINE_BUDGET: usize = 120;
/// Max block-nesting depth inside a function body.
pub(crate) const NEST_BUDGET: usize = 5;
/// Max cyclomatic proxy: `if`/match-arm/`while`/`for`/`&&`/`||`/`?` count.
pub(crate) const CYCLO_BUDGET: usize = 20;

/// Repo-relative path to the ratchet allowlist data file.
pub(crate) const RATCHET_REL: &str = "traceability/complexity_ratchet.yaml";

/// One function's measured complexity metrics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Metrics {
    pub(crate) lines: usize,
    pub(crate) nesting: usize,
    pub(crate) cyclomatic: usize,
}

impl Metrics {
    fn over_budget(self) -> bool {
        self.lines > FN_LINE_BUDGET || self.nesting > NEST_BUDGET || self.cyclomatic > CYCLO_BUDGET
    }

    /// True when `self` is no worse than the pinned `other` on every axis
    /// (the ratchet only lets a pinned function stay or improve).
    fn within(self, other: Metrics) -> bool {
        self.lines <= other.lines
            && self.nesting <= other.nesting
            && self.cyclomatic <= other.cyclomatic
    }
}

/// One ratchet allowlist entry: a pinned over-budget function.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct RatchetEntry {
    /// `<repo-relative file>::<fn path>` (the function's display key).
    pub(crate) key: String,
    pub(crate) lines: usize,
    pub(crate) nesting: usize,
    pub(crate) cyclomatic: usize,
}

impl RatchetEntry {
    fn metrics(&self) -> Metrics {
        Metrics {
            lines: self.lines,
            nesting: self.nesting,
            cyclomatic: self.cyclomatic,
        }
    }
}

/// Production entry: collect every production function's metrics and enforce the
/// budgets against the live ratchet allowlist.
pub(crate) fn check(repo_root: &Path, source_cache: &mut SourceCache) -> Result<()> {
    let files = crate::structural::production_rust_files(repo_root);
    let allowlist = load_ratchet(repo_root)?;
    let measured = collect_functions(repo_root, &files, source_cache)?;
    enforce(&measured, &allowlist)
}

/// Load the ratchet allowlist, returning an empty map when the file is absent
/// (first-run / report mode).
pub(crate) fn load_ratchet(repo_root: &Path) -> Result<Vec<RatchetEntry>> {
    let path = repo_root.join(RATCHET_REL);
    if !path.exists() {
        return Ok(Vec::new());
    }
    load_yaml(&path)
}

/// Enforce budgets: each measured function must be within budget, or pinned in
/// `allowlist` at values it does not exceed. Anti-rot finds stale pins.
pub(crate) fn enforce(measured: &[(String, Metrics)], allowlist: &[RatchetEntry]) -> Result<()> {
    let pinned: BTreeMap<&str, Metrics> = allowlist
        .iter()
        .map(|entry| (entry.key.as_str(), entry.metrics()))
        .collect();
    let measured_keys: BTreeMap<&str, Metrics> =
        measured.iter().map(|(k, m)| (k.as_str(), *m)).collect();

    let mut violations: Vec<String> = Vec::new();
    for (key, metrics) in measured {
        if !metrics.over_budget() {
            continue;
        }
        match pinned.get(key.as_str()) {
            Some(pin) if metrics.within(*pin) => continue,
            Some(pin) => violations.push(format!(
                "{key}: regressed past its ratchet pin (now lines={} nesting={} cyclo={}; \
                 pinned lines={} nesting={} cyclo={}). Split it — the ratchet only shrinks.",
                metrics.lines,
                metrics.nesting,
                metrics.cyclomatic,
                pin.lines,
                pin.nesting,
                pin.cyclomatic
            )),
            None => violations.push(format!(
                "{key}: over budget (lines={} > {FN_LINE_BUDGET}? nesting={} > {NEST_BUDGET}? \
                 cyclo={} > {CYCLO_BUDGET}?) and not in {RATCHET_REL}. Split it into smaller \
                 functions; do not bump a budget.",
                metrics.lines, metrics.nesting, metrics.cyclomatic
            )),
        }
    }

    // Anti-rot: pins that are now within budget, or name a vanished function.
    for entry in allowlist {
        match measured_keys.get(entry.key.as_str()) {
            Some(metrics) if !metrics.over_budget() => violations.push(format!(
                "{}: ratchet pin is stale — the function is now within budget. Remove its \
                 entry from {RATCHET_REL} (the ratchet only shrinks).",
                entry.key
            )),
            None => violations.push(format!(
                "{}: ratchet pin names no live production function. Remove the stale entry \
                 from {RATCHET_REL}.",
                entry.key
            )),
            Some(_) => {}
        }
    }

    ensure(
        violations.is_empty(),
        format!(
            "structural-check (function-complexity): {} complexity violation(s) \
             [GAUNT-CPLX-6]:\n  {}",
            violations.len(),
            violations.join("\n  ")
        ),
    )
}

/// Parse every file in `paths` and return `(key, Metrics)` for every function /
/// method, where `key` is `<repo-relative file>::<fn path>`.
pub(crate) fn collect_functions(
    repo_root: &Path,
    paths: &[PathBuf],
    source_cache: &mut SourceCache,
) -> Result<Vec<(String, Metrics)>> {
    let mut out = Vec::new();
    for path in paths {
        let rel = relative(repo_root, path);
        let content = source_cache.read_to_string(path)?;
        let lines: Vec<&str> = content.lines().collect();
        let file = source_cache
            .parse_rust(path)
            .map_err(|err| anyhow!("parse function complexity in {rel}: {err}"))?;
        let mut visitor = FnVisitor {
            rel: &rel,
            source_lines: &lines,
            out: &mut out,
            scope: Vec::new(),
        };
        visitor.visit_file(&file);
    }
    Ok(out)
}

/// Walks items, tracking an enclosing-scope path (module / impl) so each
/// function gets a stable, unique display key.
struct FnVisitor<'a> {
    rel: &'a str,
    source_lines: &'a [&'a str],
    out: &'a mut Vec<(String, Metrics)>,
    scope: Vec<String>,
}

impl<'a> FnVisitor<'a> {
    fn key(&self, name: &str) -> String {
        if self.scope.is_empty() {
            format!("{}::{name}", self.rel)
        } else {
            format!("{}::{}::{name}", self.rel, self.scope.join("::"))
        }
    }

    fn record(&mut self, name: &str, block: &syn::Block) {
        let span = block.span();
        let lines = nonblank_lines_in_span(self.source_lines, span.start().line, span.end().line);
        let nesting = block_nesting_depth(block);
        let cyclomatic = cyclomatic_proxy(block);
        let key = self.key(name);
        self.out.push((
            key,
            Metrics {
                lines,
                nesting,
                cyclomatic,
            },
        ));
    }
}

impl<'a, 'ast> Visit<'ast> for FnVisitor<'a> {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        // Skip `#[cfg(test)]` modules — test islands are governed by a separate
        // cap and are not production complexity.
        if crate::structural::module_is_cfg_test(&node.attrs) {
            return;
        }
        self.scope.push(node.ident.to_string());
        syn::visit::visit_item_mod(self, node);
        self.scope.pop();
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let label = impl_label(node);
        self.scope.push(label);
        syn::visit::visit_item_impl(self, node);
        self.scope.pop();
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.record(&node.sig.ident.to_string(), &node.block);
        // Descend for nested fns / closures-as-items (rare) but do not double
        // count: nested item fns get their own record via this same visit.
        syn::visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.record(&node.sig.ident.to_string(), &node.block);
        syn::visit::visit_impl_item_fn(self, node);
    }
}

fn impl_label(node: &syn::ItemImpl) -> String {
    let ty = type_label(&node.self_ty);
    match &node.trait_ {
        Some((_, path, _)) => format!("<{} as {}>", ty, path_label(path)),
        None => ty,
    }
}

fn type_label(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(p) => path_label(&p.path),
        other => quote_to_compact(other),
    }
}

fn path_label(path: &syn::Path) -> String {
    path.segments
        .last()
        .map(|seg| seg.ident.to_string())
        .unwrap_or_else(|| "?".to_owned())
}

fn quote_to_compact(ty: &syn::Type) -> String {
    let text = quote::quote!(#ty).to_string();
    text.replace(' ', "")
}

/// Count nonblank source lines spanned by `[start_line, end_line]` (1-based,
/// inclusive). Mirrors `nonblank_line_count_in_range` in structural.rs.
fn nonblank_lines_in_span(lines: &[&str], start_line: usize, end_line: usize) -> usize {
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

/// Maximum brace-nesting depth of any statement inside `block`, counting the
/// function body itself as depth 1.
fn block_nesting_depth(block: &syn::Block) -> usize {
    let mut visitor = NestVisitor { depth: 0, max: 0 };
    visitor.enter_block(block);
    visitor.max
}

struct NestVisitor {
    depth: usize,
    max: usize,
}

impl NestVisitor {
    fn enter_block(&mut self, block: &syn::Block) {
        self.depth += 1;
        self.max = self.max.max(self.depth);
        for stmt in &block.stmts {
            self.visit_stmt(stmt);
        }
        self.depth -= 1;
    }
}

impl<'ast> Visit<'ast> for NestVisitor {
    fn visit_block(&mut self, block: &'ast syn::Block) {
        self.enter_block(block);
    }
    fn visit_item(&mut self, _item: &'ast syn::Item) {
        // Do not descend into nested item definitions (nested fns get their own
        // top-level record); their bodies are not this function's nesting.
    }
}

/// Cheap cyclomatic proxy: number of decision points in `block`.
fn cyclomatic_proxy(block: &syn::Block) -> usize {
    let mut visitor = CycloVisitor { count: 0 };
    for stmt in &block.stmts {
        visitor.visit_stmt(stmt);
    }
    visitor.count
}

struct CycloVisitor {
    count: usize,
}

impl<'ast> Visit<'ast> for CycloVisitor {
    fn visit_item(&mut self, _item: &'ast syn::Item) {
        // Nested item definitions account for their own complexity separately.
    }

    fn visit_expr_if(&mut self, node: &'ast syn::ExprIf) {
        self.count += 1;
        syn::visit::visit_expr_if(self, node);
    }

    fn visit_expr_while(&mut self, node: &'ast syn::ExprWhile) {
        self.count += 1;
        syn::visit::visit_expr_while(self, node);
    }

    fn visit_expr_for_loop(&mut self, node: &'ast syn::ExprForLoop) {
        self.count += 1;
        syn::visit::visit_expr_for_loop(self, node);
    }

    fn visit_expr_match(&mut self, node: &'ast syn::ExprMatch) {
        // Each arm is a decision branch.
        self.count += node.arms.len();
        syn::visit::visit_expr_match(self, node);
    }

    fn visit_expr_binary(&mut self, node: &'ast syn::ExprBinary) {
        if matches!(node.op, syn::BinOp::And(_) | syn::BinOp::Or(_)) {
            self.count += 1;
        }
        syn::visit::visit_expr_binary(self, node);
    }

    fn visit_expr_try(&mut self, node: &'ast syn::ExprTry) {
        self.count += 1;
        syn::visit::visit_expr_try(self, node);
    }
}
