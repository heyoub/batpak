use crate::repo_surface::{core_src_root, core_tests_root, rust_files};
use anyhow::{bail, Context, Result};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use syn::visit::{self, Visit};
use syn::Item;

/// Assert that every `pub fn` declared in inherent `impl Store { ... }` blocks
/// under `crates/core/src/store/` has at least one reference in the test or source tree.
///
/// This is a structural guard: if a method is added to `Store` and no test
/// exercises it, a developer (or agent) has likely forgotten to write the test.
/// The check uses `syn` to parse the AST — no regex heuristics for pub fn
/// detection — so methods inside `#[cfg(...)]` blocks or across multiple `impl`
/// blocks are handled correctly.
///
/// Reference detection uses regex against the combined text of `tests/` and
/// `src/` (which covers both standalone test files and `#[cfg(test)] mod tests`
/// inline in source files).
pub(crate) fn check(repo_root: &Path) -> Result<()> {
    // Methods that are deliberately exercised only indirectly or are
    // intentionally infrastructure-only. Start empty and add only proven
    // false positives with a justification comment.
    let allowlist: &[&str] = &[
        // `subscription` is doc(hidden) glue for async integration, exercised
        // indirectly via `subscribe` in every subscription test.
        "subscription",
    ];

    // 1. Parse every store source file with syn and walk all inherent
    // `impl Store` blocks. Store's public surface is intentionally split by
    // owner modules; the detector follows that architecture instead of
    // hardcoding `src/store/mod.rs`.
    let mut pub_fns: BTreeSet<String> = BTreeSet::new();
    for store_source_path in rust_files(&core_src_root(repo_root).join("store")) {
        let source = fs::read_to_string(&store_source_path)
            .with_context(|| format!("read {}", store_source_path.display()))?;
        let ast = syn::parse_file(&source)
            .with_context(|| format!("syn parse {}", store_source_path.display()))?;

        for item in &ast.items {
            if let Item::Impl(impl_block) = item {
                // Match `impl Store`, `impl Store<Open>`, and `impl<T> Store<T>`.
                // We only care about blocks whose self type path segment is
                // exactly `Store`.
                let is_store_impl = match impl_block.self_ty.as_ref() {
                    syn::Type::Path(tp) => tp
                        .path
                        .segments
                        .last()
                        .map(|s| s.ident == "Store")
                        .unwrap_or(false),
                    _ => false,
                };
                // Trait impls (e.g., `impl Drop for Store`) are excluded — we
                // only want inherent impls.
                if !is_store_impl || impl_block.trait_.is_some() {
                    continue;
                }
                for impl_item in &impl_block.items {
                    if let syn::ImplItem::Fn(method) = impl_item {
                        if matches!(method.vis, syn::Visibility::Public(_)) {
                            let name = method.sig.ident.to_string();
                            // Skip names starting with `_` (private convention).
                            if !name.starts_with('_') {
                                pub_fns.insert(name);
                            }
                        }
                    }
                }
            }
        }
    }

    if pub_fns.is_empty() {
        bail!(
            "structural-check: Store pub fn coverage — could not find any `impl Store` \
             block under crates/core/src/store/. The Store surface may have moved outside the declared owner."
        );
    }

    // 2. Build the reference corpus: parseable .rs files under tests/ and src/.
    // Compile-fail UI fixtures are intentionally invalid Rust and are skipped;
    // comments and string literals never count because reference detection is AST-based.
    let mut search_asts = Vec::new();
    for path in rust_files(&core_tests_root(repo_root))
        .into_iter()
        .chain(rust_files(&core_src_root(repo_root)))
    {
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let Ok(ast) = syn::parse_file(&content) else {
            continue;
        };
        search_asts.push(ast);
    }

    // 3. For each pub fn, check that at least one file references it as a call.
    //    Patterns matched: `.name(`, `Store::name(`, `store.name(`
    let mut unreferenced: Vec<String> = Vec::new();
    for name in &pub_fns {
        if allowlist.contains(&name.as_str()) {
            continue;
        }
        if !search_asts
            .iter()
            .any(|ast| ast_references_store_method(ast, name))
        {
            unreferenced.push(name.clone());
        }
    }

    if !unreferenced.is_empty() {
        let list = unreferenced
            .iter()
            .map(|n| format!("  - {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        bail!(
            "structural-check: Store pub fn coverage failure — the following methods on\n\
             `impl Store` have ZERO test or source references and are likely orphaned:\n\
             {list}\n\
             Investigate: crates/core/src/store/ and add a test exercising each, or remove the\n\
             method if it's truly unused."
        );
    }

    Ok(())
}

fn ast_references_store_method(ast: &syn::File, name: &str) -> bool {
    struct MethodCallFinder<'a> {
        name: &'a str,
        found: bool,
    }

    impl<'ast> Visit<'ast> for MethodCallFinder<'_> {
        fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
            if node.method == self.name {
                self.found = true;
                return;
            }
            visit::visit_expr_method_call(self, node);
        }

        fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
            if let syn::Expr::Path(path) = node.func.as_ref() {
                let segments = path
                    .path
                    .segments
                    .iter()
                    .map(|segment| segment.ident.to_string())
                    .collect::<Vec<_>>();
                if matches!(
                    segments.as_slice(),
                    [owner, method] if owner == "Store" && method == self.name
                ) || matches!(
                    segments.as_slice(),
                    [.., owner, method] if owner == "Store" && method == self.name
                ) {
                    self.found = true;
                    return;
                }
            }
            visit::visit_expr_call(self, node);
        }
    }

    let mut finder = MethodCallFinder { name, found: false };
    finder.visit_file(ast);
    finder.found
}

#[cfg(test)]
mod tests {
    use super::ast_references_store_method;

    #[test]
    fn ast_reference_detection_ignores_comments_and_strings() {
        let ast = syn::parse_file(
            r#"
// store.forgotten_method()
const TEXT: &str = "Store::forgotten_method()";
fn unrelated() {}
"#,
        )
        .expect("parse fixture");
        assert!(
            !ast_references_store_method(&ast, "forgotten_method"),
            "comments and strings must not satisfy Store pub fn coverage"
        );
    }

    #[test]
    fn ast_reference_detection_accepts_method_and_associated_calls() {
        let method = syn::parse_file(
            r#"
fn exercise(store: &batpak::store::Store) {
    let _ = store.watch_projection::<Projection>("entity");
}
"#,
        )
        .expect("parse method fixture");
        assert!(ast_references_store_method(&method, "watch_projection"));

        let associated = syn::parse_file(
            r#"
fn exercise(config: batpak::store::StoreConfig) {
    let _ = batpak::store::Store::open(config);
}
"#,
        )
        .expect("parse associated fixture");
        assert!(ast_references_store_method(&associated, "open"));
    }
}
