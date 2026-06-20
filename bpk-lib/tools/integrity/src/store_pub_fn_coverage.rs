use crate::repo_surface::{core_src_root, core_tests_root, rust_files};
use crate::rust_ast::{callee_path_segments, tail_owner_method};
use crate::source_cache::SourceCache;
use anyhow::{bail, Context, Result};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use syn::visit::{self, Visit};
use syn::Item;

const ALLOWLIST: &[&str] = &[
    // `subscription` is doc(hidden) glue for async integration, exercised
    // indirectly via `subscribe` in every subscription test.
    "subscription",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StorePubFnCoverage {
    pub(crate) name: String,
    pub(crate) covered: bool,
    pub(crate) allowlisted: bool,
}

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
pub(crate) fn check(repo_root: &Path, source_cache: &mut SourceCache) -> Result<()> {
    let inventory = inventory(repo_root, source_cache)?;
    let unreferenced = inventory
        .iter()
        .filter(|entry| !entry.covered && !entry.allowlisted)
        .map(|entry| entry.name.clone())
        .collect::<Vec<_>>();

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

pub(crate) fn inventory(
    repo_root: &Path,
    source_cache: &mut SourceCache,
) -> Result<Vec<StorePubFnCoverage>> {
    // 1. Parse every store source file with syn and walk all inherent
    // `impl Store` blocks. Store's public surface is intentionally split by
    // owner modules; the detector follows that architecture instead of
    // hardcoding `src/store/mod.rs`.
    let mut pub_fns: BTreeSet<String> = BTreeSet::new();
    for store_source_path in rust_files(&core_src_root(repo_root).join("store")) {
        let ast = source_cache
            .parse_rust(&store_source_path)
            .with_context(|| format!("syn parse {}", store_source_path.display()))?;

        for item in &ast.items {
            if let Item::Impl(impl_block) = item {
                // Match `impl Store`, `impl Store<Open>`, and `impl<T> Store<T>`.
                // We only care about blocks whose self type path segment is
                // exactly `Store`.
                let is_store_impl = if let syn::Type::Path(tp) = impl_block.self_ty.as_ref() {
                    tp.path
                        .segments
                        .last()
                        .map(|s| s.ident == "Store")
                        .unwrap_or(false)
                } else {
                    false
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
    let mut search_asts: Vec<Arc<syn::File>> = Vec::new();
    for path in rust_files(&core_tests_root(repo_root))
        .into_iter()
        .chain(rust_files(&core_src_root(repo_root)))
    {
        if let Some(ast) = source_cache.parse_rust_if_valid(&path)? {
            search_asts.push(ast);
        }
    }

    Ok(pub_fns
        .into_iter()
        .map(|name| StorePubFnCoverage {
            allowlisted: ALLOWLIST.contains(&name.as_str()),
            covered: search_asts
                .iter()
                .any(|ast| ast_references_store_method(ast, &name)),
            name,
        })
        .collect())
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
            if let Some(segments) = callee_path_segments(&node.func) {
                if let Some((owner, method)) = tail_owner_method(&segments) {
                    if owner == "Store" && method == self.name {
                        self.found = true;
                        return;
                    }
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
    use super::{ast_references_store_method, check};
    use crate::source_cache::SourceCache;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_repo(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "batpak-store-pub-fn-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp repo");
        path
    }

    fn write_file(repo: &Path, rel: &str, body: &str) {
        let path = repo.join(rel);
        fs::create_dir_all(path.parent().expect("parent dir")).expect("create dirs");
        fs::write(&path, body).expect("write fixture file");
    }

    /// END-TO-END RED FIXTURE: a `pub fn` on `impl Store` with ZERO test/source
    /// references must make the gate's `check(..)` fail; a single witnessing
    /// reference must make it pass. Both halves run so the test cannot pass
    /// vacuously — neutralizing the violation (adding the witness) turns it red.
    #[test]
    fn store_pub_fn_coverage_rejects_uncovered_store_method() {
        let repo = temp_repo("uncovered");
        write_file(
            &repo,
            "crates/core/src/store/mod.rs",
            "pub struct Store;\nimpl Store {\n    pub fn orphaned_probe(&self) -> u8 { 0 }\n}\n",
        );

        // GREEN: a test file that actually calls the method satisfies coverage.
        write_file(
            &repo,
            "crates/core/tests/probe.rs",
            "#[test]\nfn exercises() {\n    let store = batpak::store::Store;\n    let _ = store.orphaned_probe();\n}\n",
        );
        let mut cache = SourceCache::new(&repo);
        check(&repo, &mut cache).expect("a witnessed Store pub fn is accepted");

        // RED: remove the witness — the orphaned pub fn must be flagged.
        write_file(
            &repo,
            "crates/core/tests/probe.rs",
            "#[test]\nfn unrelated() {\n    assert_eq!(1, 1);\n}\n",
        );
        let mut cache = SourceCache::new(&repo);
        let err = check(&repo, &mut cache).expect_err("orphaned Store pub fn is rejected");
        assert!(
            err.to_string().contains("Store pub fn coverage failure")
                && err.to_string().contains("orphaned_probe"),
            "{err:?}"
        );

        fs::remove_dir_all(repo).expect("remove temp repo");
    }

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
