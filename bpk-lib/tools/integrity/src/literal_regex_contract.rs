//! INV-LITERAL-REGEX-UNWRAP-SAFE: literal `Regex::new(...).expect/unwrap`
//! sites in tooling must carry an adjacent justification, and non-literal
//! patterns must use fallible propagation instead.

use crate::repo_surface::{ensure, relative, rust_files};
use crate::source_cache::SourceCache;
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Expr, ExprMethodCall, Lit};

const JUSTIFICATION: &str = "justifies: INV-LITERAL-REGEX-UNWRAP-SAFE";

pub(crate) fn check(repo_root: &Path, source_cache: &mut SourceCache) -> Result<BTreeSet<PathBuf>> {
    let mut inputs = BTreeSet::new();
    for root in ["tools/integrity/src", "tools/xtask/src"] {
        for path in rust_files(&repo_root.join(root)) {
            inputs.insert(path.clone());
            let rel = relative(repo_root, &path);
            let content = source_cache
                .read_to_string(&path)
                .with_context(|| format!("read {rel}"))?;
            let findings = regex_expect_findings(&rel, &content)?;
            ensure(
                findings.is_empty(),
                format!(
                    "literal-regex-contract (INV-LITERAL-REGEX-UNWRAP-SAFE): {} finding(s):\n  {}",
                    findings.len(),
                    findings.join("\n  ")
                ),
            )?;
        }
    }
    Ok(inputs)
}

fn regex_expect_findings(rel: &str, content: &str) -> Result<Vec<String>> {
    let file = syn::parse_file(content).with_context(|| format!("parse {rel}"))?;
    let lines = content.lines().collect::<Vec<_>>();
    let mut findings = Vec::new();
    let mut visitor = RegexExpectVisitor {
        rel,
        lines: &lines,
        findings: &mut findings,
    };
    visitor.visit_file(&file);
    findings.sort();
    findings.dedup();
    Ok(findings)
}

struct RegexExpectVisitor<'a> {
    rel: &'a str,
    lines: &'a [&'a str],
    findings: &'a mut Vec<String>,
}

impl<'a, 'ast> Visit<'ast> for RegexExpectVisitor<'a> {
    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        if (node.method == "expect" || node.method == "unwrap")
            && regex_new_call_arg(&node.receiver).is_some()
        {
            let line = node.span().start().line;
            let literal = regex_new_call_arg(&node.receiver).is_some_and(expr_is_string_literal);
            if !literal {
                self.findings.push(format!(
                    "{}:{}: Regex::new(...).{} uses a non-literal pattern; return Result instead",
                    self.rel, line, node.method
                ));
            } else if !has_nearby_justification(self.lines, line) {
                self.findings.push(format!(
                    "{}:{}: literal Regex::new(...).{} lacks `{JUSTIFICATION}` on a nearby comment",
                    self.rel, line, node.method
                ));
            }
        }
        syn::visit::visit_expr_method_call(self, node);
    }
}

fn regex_new_call_arg(expr: &Expr) -> Option<&Expr> {
    let Expr::Call(call) = expr else {
        return None;
    };
    let Expr::Path(path) = call.func.as_ref() else {
        return None;
    };
    if !path_ends_with_regex_new(&path.path) {
        return None;
    }
    call.args.first()
}

fn path_ends_with_regex_new(path: &syn::Path) -> bool {
    let mut segments = path.segments.iter().rev();
    let Some(last) = segments.next() else {
        return false;
    };
    let Some(prev) = segments.next() else {
        return false;
    };
    last.ident == "new" && prev.ident == "Regex"
}

fn expr_is_string_literal(expr: &Expr) -> bool {
    matches!(expr, Expr::Lit(lit) if matches!(&lit.lit, Lit::Str(_)))
}

fn has_nearby_justification(lines: &[&str], one_based_line: usize) -> bool {
    let current = one_based_line.saturating_sub(1);
    let start = current.saturating_sub(4);
    lines[start..current]
        .iter()
        .any(|line| line.contains(JUSTIFICATION))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_regex_contract_rejects_unjustified_or_dynamic_expect() {
        let unjustified = r#"
use regex::Regex;
fn gate() {
    let _ = Regex::new(r"\d+").expect("literal regex compiles");
}
"#;
        let unjustified_findings = regex_expect_findings("tools/integrity/src/red.rs", unjustified)
            .expect("red fixture parses");
        assert!(
            !unjustified_findings.is_empty(),
            "unjustified literal regex expect must be rejected"
        );

        let dynamic = r#"
use regex::Regex;
fn gate(input: &str) {
    // justifies: INV-LITERAL-REGEX-UNWRAP-SAFE; this comment cannot launder a dynamic pattern.
    let _ = Regex::new(input).expect("dynamic regex compiles");
}
"#;
        assert!(regex_expect_findings("tools/integrity/src/red.rs", dynamic)
            .expect("red fixture parses")
            .iter()
            .any(|finding| finding.contains("non-literal")));
    }

    #[test]
    fn literal_regex_contract_accepts_justified_literal_expect() {
        let green = r#"
use regex::Regex;
fn gate() {
    // justifies: INV-LITERAL-REGEX-UNWRAP-SAFE; literal regex pattern is compile-time-known.
    let _ = Regex::new(r"\d+").expect("literal regex compiles");
}
"#;
        assert!(regex_expect_findings("tools/integrity/src/green.rs", green)
            .expect("green fixture parses")
            .is_empty());
    }
}
