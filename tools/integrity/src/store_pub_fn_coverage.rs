use crate::repo_surface::rust_files;
use anyhow::{bail, Context, Result};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use syn::Item;

/// Assert that every `pub fn` declared in inherent `impl Store { ... }` blocks
/// under `src/store/` has at least one reference in the test or source tree.
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
    for store_source_path in rust_files(&repo_root.join("src/store")) {
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
             block under src/store/. The Store surface may have moved outside the declared owner."
        );
    }

    // 2. Build the reference corpus: all .rs files under tests/ and src/.
    let mut search_files: Vec<PathBuf> = rust_files(&repo_root.join("tests"));
    search_files.extend(rust_files(&repo_root.join("src")));

    // 3. For each pub fn, check that at least one file references it as a call.
    //    Patterns matched: `.name(`, `Store::name(`, `store.name(`
    let mut unreferenced: Vec<String> = Vec::new();
    for name in &pub_fns {
        if allowlist.contains(&name.as_str()) {
            continue;
        }
        // Build patterns that strongly indicate a method call or direct use.
        // We accept any of:
        //   `.name(`        — method call syntax
        //   `.name::<`      — method call with turbofish (e.g., `.watch_projection::<T>(...)`)
        //   `Store::name(`  — fully-qualified call
        //   `Store::name::<` — fully-qualified call with turbofish
        //   `store.name(`   — conventional variable name
        //   `store.name::<` — conventional variable name with turbofish
        // The turbofish variants are critical: we miss generic method calls
        // without them. Caught by the watch_projection false-positive when
        // this check first ran against the real codebase.
        let patterns = [
            format!(".{}(", name),
            format!(".{}::<", name),
            format!("Store::{}(", name),
            format!("Store::{}::<", name),
            format!("store.{}(", name),
            format!("store.{}::<", name),
        ];
        let mut found = false;
        'files: for path in &search_files {
            let content = match fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for pat in &patterns {
                if content.contains(pat.as_str()) {
                    found = true;
                    break 'files;
                }
            }
        }
        if !found {
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
             Investigate: src/store/ and add a test exercising each, or remove the\n\
             method if it's truly unused."
        );
    }

    Ok(())
}
