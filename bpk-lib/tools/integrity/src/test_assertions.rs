//! Test assertion rigor gate for INV-TEST-PANIC-AS-ASSERTION.
//!
//! The gate scans Rust test bodies, including integration tests and inline
//! `#[cfg(test)]` modules, for assertion shapes that create weak negative tests:
//! direct `panic!`, `.unwrap()`, discarded `.expect_err(..)`, and bare
//! message-less `assert!(result.is_err())` checks.

use crate::repo_surface::{ensure, relative, tracked_repo_files};
use crate::source_cache::SourceCache;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Expr, ExprMacro, ExprMethodCall, ItemFn, ItemMod, Macro, Pat, Stmt, Token};

pub(crate) fn check(repo_root: &Path, source_cache: &mut SourceCache) -> Result<()> {
    let files = test_assertion_files(repo_root)?;
    let offenders = collect_offenders(repo_root, &files, source_cache)?;
    enforce(&offenders)
}

pub(crate) fn test_assertion_files(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for path in tracked_repo_files(repo_root)? {
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let rel = relative(repo_root, &path);
        if rel.starts_with("crates/core/fixtures/") {
            continue;
        }
        if rel.contains("/tests/")
            || rel.ends_with("_tests.rs")
            || rel.ends_with("/tests.rs")
            || rel.contains("/src/")
            || rel.starts_with("tools/")
        {
            files.push(path);
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

pub(crate) fn collect_offenders(
    repo_root: &Path,
    paths: &[PathBuf],
    source_cache: &mut SourceCache,
) -> Result<Vec<String>> {
    let mut offenders = Vec::new();
    for path in paths {
        let rel = relative(repo_root, path);
        let file = source_cache
            .parse_rust_if_valid(path)
            .with_context(|| format!("parse test assertion surface {rel}"))?;
        let Some(file) = file else {
            continue;
        };
        let mut visitor = TestBodyVisitor {
            rel: &rel,
            file_is_test_surface: file_is_test_surface(&rel),
            test_context_depth: 0,
            offenders: &mut offenders,
        };
        visitor.visit_file(&file);
    }
    offenders.sort();
    offenders.dedup();
    Ok(offenders)
}

pub(crate) fn enforce(offenders: &[String]) -> Result<()> {
    ensure(
        offenders.is_empty(),
        format!(
            "structural-check (test-assertion-rigor): {} weak test assertion(s) [INV-TEST-PANIC-AS-ASSERTION]:\n  {}",
            offenders.len(),
            offenders.join("\n  ")
        ),
    )
}

fn file_is_test_surface(rel: &str) -> bool {
    rel.contains("/tests/")
        || rel.ends_with("_tests.rs")
        || rel.ends_with("/tests.rs")
        || rel.starts_with("tools/integrity/src/")
}

struct TestBodyVisitor<'a> {
    rel: &'a str,
    file_is_test_surface: bool,
    test_context_depth: usize,
    offenders: &'a mut Vec<String>,
}

impl<'a, 'ast> Visit<'ast> for TestBodyVisitor<'a> {
    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        let enters_test_context = item_has_cfg_test(&node.attrs);
        if enters_test_context {
            self.test_context_depth = self.test_context_depth.saturating_add(1);
        }
        syn::visit::visit_item_mod(self, node);
        if enters_test_context {
            self.test_context_depth = self.test_context_depth.saturating_sub(1);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        let is_test_body = self.file_is_test_surface
            || self.test_context_depth > 0
            || item_has_test_attr(&node.attrs)
            || item_has_cfg_test(&node.attrs);
        if is_test_body {
            let mut visitor = FunctionAssertionVisitor {
                rel: self.rel,
                fn_name: node.sig.ident.to_string(),
                offenders: self.offenders,
            };
            visitor.visit_block(&node.block);
            return;
        }
        syn::visit::visit_item_fn(self, node);
    }
}

struct FunctionAssertionVisitor<'a> {
    rel: &'a str,
    fn_name: String,
    offenders: &'a mut Vec<String>,
}

impl<'a, 'ast> Visit<'ast> for FunctionAssertionVisitor<'a> {
    fn visit_stmt(&mut self, node: &'ast Stmt) {
        match node {
            Stmt::Expr(expr, Some(_)) if expr_ends_with_method(expr, "expect_err") => {
                self.push(
                    expr.span().start().line,
                    "discarded `.expect_err(..)` result; bind the error and assert its variant or code",
                );
            }
            Stmt::Local(local) if local_discards_expect_err(local) => {
                self.push(
                    local.span().start().line,
                    "`let _ = ...expect_err(..)` discards the error; assert its variant or code",
                );
            }
            Stmt::Macro(stmt_macro) if macro_ends_with(&stmt_macro.mac.path, "panic") => {
                self.push(
                    stmt_macro.span().start().line,
                    "direct `panic!` in a test body",
                );
            }
            Stmt::Macro(stmt_macro)
                if macro_ends_with(&stmt_macro.mac.path, "assert")
                    && macro_tokens_are_bare_is_err(&stmt_macro.mac) =>
            {
                self.push(
                    stmt_macro.span().start().line,
                    "bare message-less `assert!(..is_err())`; bind the error and assert its variant/code or name the single failure contract",
                );
            }
            Stmt::Local(_) | Stmt::Item(_) | Stmt::Macro(_) | Stmt::Expr(_, _) => {}
        }
        syn::visit::visit_stmt(self, node);
    }

    fn visit_expr_macro(&mut self, node: &'ast ExprMacro) {
        if macro_ends_with(&node.mac.path, "panic") {
            self.push(node.span().start().line, "direct `panic!` in a test body");
        }
        if macro_ends_with(&node.mac.path, "assert") && macro_tokens_are_bare_is_err(&node.mac) {
            self.push(
                node.span().start().line,
                "bare message-less `assert!(..is_err())`; bind the error and assert its variant/code or name the single failure contract",
            );
        }
        syn::visit::visit_expr_macro(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        if node.method == "unwrap" {
            self.push(
                node.span().start().line,
                "`.unwrap()` in a test body; use `.expect(..)` for setup or assert the exact error",
            );
        }
        syn::visit::visit_expr_method_call(self, node);
    }
}

impl FunctionAssertionVisitor<'_> {
    fn push(&mut self, line: usize, reason: &str) {
        self.offenders.push(format!(
            "{}::{}:{}: {}",
            self.rel, self.fn_name, line, reason
        ));
    }
}

fn local_discards_expect_err(local: &syn::Local) -> bool {
    matches!(local.pat, Pat::Wild(_))
        && local
            .init
            .as_ref()
            .is_some_and(|init| expr_contains_method(&init.expr, "expect_err"))
}

fn macro_tokens_are_bare_is_err(mac: &Macro) -> bool {
    let parser = Punctuated::<Expr, Token![,]>::parse_terminated;
    let Ok(args) = parser.parse2(mac.tokens.clone()) else {
        return false;
    };
    args.len() == 1 && args.first().is_some_and(is_bare_is_err_expr)
}

fn is_bare_is_err_expr(expr: &Expr) -> bool {
    match expr {
        Expr::MethodCall(call) => call.method == "is_err",
        Expr::Paren(paren) => is_bare_is_err_expr(&paren.expr),
        Expr::Group(group) => is_bare_is_err_expr(&group.expr),
        Expr::Reference(reference) => is_bare_is_err_expr(&reference.expr),
        Expr::Array(_)
        | Expr::Assign(_)
        | Expr::Async(_)
        | Expr::Await(_)
        | Expr::Binary(_)
        | Expr::Block(_)
        | Expr::Break(_)
        | Expr::Call(_)
        | Expr::Cast(_)
        | Expr::Closure(_)
        | Expr::Const(_)
        | Expr::Continue(_)
        | Expr::Field(_)
        | Expr::ForLoop(_)
        | Expr::If(_)
        | Expr::Index(_)
        | Expr::Infer(_)
        | Expr::Let(_)
        | Expr::Lit(_)
        | Expr::Loop(_)
        | Expr::Macro(_)
        | Expr::Match(_)
        | Expr::Path(_)
        | Expr::Range(_)
        | Expr::RawAddr(_)
        | Expr::Repeat(_)
        | Expr::Return(_)
        | Expr::Struct(_)
        | Expr::Try(_)
        | Expr::TryBlock(_)
        | Expr::Tuple(_)
        | Expr::Unary(_)
        | Expr::Unsafe(_)
        | Expr::Verbatim(_)
        | Expr::While(_)
        | Expr::Yield(_)
        | _ => false,
    }
}

fn expr_ends_with_method(expr: &Expr, method: &str) -> bool {
    match expr {
        Expr::MethodCall(call) => call.method == method,
        Expr::Paren(paren) => expr_ends_with_method(&paren.expr, method),
        Expr::Group(group) => expr_ends_with_method(&group.expr, method),
        Expr::Try(try_expr) => expr_ends_with_method(&try_expr.expr, method),
        Expr::Array(_)
        | Expr::Assign(_)
        | Expr::Async(_)
        | Expr::Await(_)
        | Expr::Binary(_)
        | Expr::Block(_)
        | Expr::Break(_)
        | Expr::Call(_)
        | Expr::Cast(_)
        | Expr::Closure(_)
        | Expr::Const(_)
        | Expr::Continue(_)
        | Expr::Field(_)
        | Expr::ForLoop(_)
        | Expr::If(_)
        | Expr::Index(_)
        | Expr::Infer(_)
        | Expr::Let(_)
        | Expr::Lit(_)
        | Expr::Loop(_)
        | Expr::Macro(_)
        | Expr::Match(_)
        | Expr::Path(_)
        | Expr::Range(_)
        | Expr::RawAddr(_)
        | Expr::Reference(_)
        | Expr::Repeat(_)
        | Expr::Return(_)
        | Expr::Struct(_)
        | Expr::TryBlock(_)
        | Expr::Tuple(_)
        | Expr::Unary(_)
        | Expr::Unsafe(_)
        | Expr::Verbatim(_)
        | Expr::While(_)
        | Expr::Yield(_)
        | _ => false,
    }
}

fn expr_contains_method(expr: &Expr, method: &str) -> bool {
    let mut visitor = MethodPresenceVisitor {
        method,
        found: false,
    };
    visitor.visit_expr(expr);
    visitor.found
}

struct MethodPresenceVisitor<'a> {
    method: &'a str,
    found: bool,
}

impl<'a, 'ast> Visit<'ast> for MethodPresenceVisitor<'a> {
    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        if node.method == self.method {
            self.found = true;
            return;
        }
        syn::visit::visit_expr_method_call(self, node);
    }
}

fn macro_ends_with(path: &syn::Path, ident: &str) -> bool {
    path.segments
        .last()
        .is_some_and(|segment| segment.ident == ident)
}

fn item_has_test_attr(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| attr.path().is_ident("test"))
}

fn item_has_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("cfg") {
            return false;
        }
        let mut found = false;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("test") {
                found = true;
            }
            Ok(())
        });
        found
    })
}
