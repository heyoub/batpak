use crate::repo_surface::{core_src_root, core_tests_root, ensure, relative, rust_files};
use crate::shared_checks::{ast_references_name, public_item_names};
use crate::source_cache::SourceCache;
use crate::typed_waivers::{self, WaiverKind};
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) fn check(repo_root: &Path, source_cache: &mut SourceCache) -> Result<()> {
    check_doc_hidden_public_surface(repo_root, source_cache)?;

    // The silent `pub_item_allowlist.yaml` is gone. The ONLY remaining
    // exemption path is a loud, expiring, owned typed waiver of kind `pub-item`
    // (`traceability/typed_waivers.yaml`). After the P0-2 triage this set is
    // empty: every public item is named directly by an AST-visible reference in
    // a test file, so the loop below proves coverage with zero waivers.
    let waivers = typed_waivers::load_waivers(repo_root)?;
    let waived: BTreeSet<String> = typed_waivers::targets_for(&waivers, WaiverKind::PubItem);

    let test_files: Vec<PathBuf> = rust_files(&core_tests_root(repo_root));
    let mut parsed_tests: Vec<(PathBuf, Arc<syn::File>)> = Vec::with_capacity(test_files.len());
    for path in test_files {
        let file = source_cache
            .parse_rust(&path)
            .with_context(|| format!("parse {}", relative(repo_root, &path)))?;
        parsed_tests.push((path, file));
    }

    for path in rust_files(&core_src_root(repo_root)) {
        if path.ends_with("prelude.rs") {
            continue;
        }
        let file = source_cache
            .parse_rust(&path)
            .with_context(|| format!("parse {}", relative(repo_root, &path)))?;
        for name in public_item_names(&file) {
            if waived.contains(name.as_str()) {
                continue;
            }
            let found = parsed_tests
                .iter()
                .any(|(_, ast)| ast_references_name(ast, &name));
            ensure(
                found,
                format!(
                    "pub item `{}` declared at {} has no test reference (checked {} test files via AST); either add a real test use, hide the item via `#[doc(hidden)]`, or — only if it genuinely cannot be directly test-named — add a typed expiring waiver of kind `pub-item` in traceability/typed_waivers.yaml.",
                    name,
                    relative(repo_root, &path),
                    parsed_tests.len(),
                ),
            )?;
        }
    }
    Ok(())
}

fn check_doc_hidden_public_surface(repo_root: &Path, source_cache: &mut SourceCache) -> Result<()> {
    let allowed: BTreeSet<&str> = [
        "crates/core/src/lib.rs::__private",
        "crates/core/src/lib.rs::batpak",
        // GAUNT-FUZZ-1: the `#[cfg(feature = "dangerous-test-hooks")]`
        // `#[doc(hidden)] pub mod __fuzz` exposes thin wrappers over real decode
        // entry points for the workspace-excluded `batpak-fuzz` cargo-fuzz crate.
        // It is feature-gated (absent from any default/published build) and
        // doc-hidden by design; these entries are the reviewed escape-hatch.
        "crates/core/src/lib.rs::__fuzz",
        "crates/core/src/__fuzz.rs::FuzzProjectionState",
        "crates/core/src/__fuzz.rs::__fuzz_segment_header",
        "crates/core/src/__fuzz.rs::__fuzz_sidx_entry",
        "crates/core/src/__fuzz.rs::__fuzz_checkpoint_data",
        "crates/core/src/__fuzz.rs::__fuzz_checkpoint_snapshot_v6",
        "crates/core/src/__fuzz.rs::__fuzz_mmap_entry",
        "crates/core/src/__fuzz.rs::__fuzz_cache_meta",
        "crates/core/src/__fuzz.rs::__fuzz_projection_state",
        "crates/core/src/__fuzz.rs::__fuzz_hidden_ranges",
        "crates/core/src/__fuzz.rs::__fuzz_mmap_index_load",
        "crates/core/src/__fuzz.rs::__fuzz_sidx_footer",
        // GAUNT-SIM-2c: the `#[cfg(feature = "dangerous-test-hooks")]`
        // `#[doc(hidden)] pub mod __sim` exposes the seeded deterministic
        // simulation driver (run_seeded_workload / replay_seed) to the
        // `sim_is_deterministic` integration test. Feature-gated (absent from
        // any default/published build) and doc-hidden by design; mirrors the
        // reviewed __fuzz escape-hatch above.
        "crates/core/src/lib.rs::__sim",
        "crates/core/src/store/sim/mod.rs::run_seeded_workload",
        "crates/core/src/store/sim/mod.rs::replay_seed",
        "crates/core/src/store/delivery/subscription.rs::receiver",
        "crates/core/src/store/projection/flow/mod.rs::ReplayInput",
        "crates/core/src/store/projection/flow/replay_input.rs::ReplayInput",
        "crates/core/src/store/projection/watch.rs::subscription",
        "crates/core/src/store/segment/scan/mod.rs::Reader",
        "crates/core/src/store/test_support.rs::panic_writer_for_test",
        "crates/core/src/typestate/transition.rs::sealed",
    ]
    .into_iter()
    .collect();

    let mut unexpected = Vec::new();
    for path in rust_files(&core_src_root(repo_root)) {
        let rel = relative(repo_root, &path);
        let file = source_cache
            .parse_rust(&path)
            .with_context(|| format!("parse {}", relative(repo_root, &path)))?;
        for name in doc_hidden_public_names(&file) {
            let key = format!("{rel}::{name}");
            if !allowed.contains(key.as_str()) {
                unexpected.push(key);
            }
        }
    }

    ensure(
        unexpected.is_empty(),
        format!(
            "public-surface: unexpected #[doc(hidden)] public item(s): {}\n\
             Hidden public API is allowed only for explicit compatibility or Rust visibility escape hatches. \
             Make the item non-public, add a real public-surface witness, or add a reviewed detector allowlist entry.",
            unexpected.join(", ")
        ),
    )
}

fn doc_hidden_public_names(file: &syn::File) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for item in &file.items {
        match item {
            syn::Item::Const(item) => {
                record_doc_hidden_public(&mut names, &item.vis, &item.attrs, item.ident.to_string())
            }
            syn::Item::Enum(item) => {
                record_doc_hidden_public(&mut names, &item.vis, &item.attrs, item.ident.to_string())
            }
            syn::Item::ExternCrate(item) => {
                let name = item
                    .rename
                    .as_ref()
                    .map(|(_, ident)| ident.to_string())
                    .unwrap_or_else(|| item.ident.to_string());
                record_doc_hidden_public(&mut names, &item.vis, &item.attrs, name);
            }
            syn::Item::Fn(item) => record_doc_hidden_public(
                &mut names,
                &item.vis,
                &item.attrs,
                item.sig.ident.to_string(),
            ),
            syn::Item::Impl(item) => {
                let impl_hidden = has_doc_hidden(&item.attrs);
                for impl_item in &item.items {
                    if let syn::ImplItem::Fn(method) = impl_item {
                        let method_hidden = impl_hidden || has_doc_hidden(&method.attrs);
                        if method_hidden {
                            record_doc_hidden_public(
                                &mut names,
                                &method.vis,
                                if impl_hidden {
                                    &item.attrs
                                } else {
                                    &method.attrs
                                },
                                method.sig.ident.to_string(),
                            );
                        }
                    }
                }
            }
            syn::Item::Mod(item) => {
                record_doc_hidden_public(&mut names, &item.vis, &item.attrs, item.ident.to_string())
            }
            syn::Item::Struct(item) => {
                record_doc_hidden_public(&mut names, &item.vis, &item.attrs, item.ident.to_string())
            }
            syn::Item::Trait(item) => {
                record_doc_hidden_public(&mut names, &item.vis, &item.attrs, item.ident.to_string())
            }
            syn::Item::Type(item) => {
                record_doc_hidden_public(&mut names, &item.vis, &item.attrs, item.ident.to_string())
            }
            syn::Item::Use(item) => {
                if matches!(item.vis, syn::Visibility::Public(_)) && has_doc_hidden(&item.attrs) {
                    collect_use_tree_names(&item.tree, &mut names);
                }
            }
            syn::Item::ForeignMod(_)
            | syn::Item::Macro(_)
            | syn::Item::Static(_)
            | syn::Item::TraitAlias(_)
            | syn::Item::Union(_)
            | syn::Item::Verbatim(_) => {}
            _ => {}
        }
    }
    names
}

fn record_doc_hidden_public(
    names: &mut BTreeSet<String>,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
    name: String,
) {
    if matches!(vis, syn::Visibility::Public(_)) && has_doc_hidden(attrs) {
        names.insert(name);
    }
}

fn collect_use_tree_names(tree: &syn::UseTree, names: &mut BTreeSet<String>) {
    match tree {
        syn::UseTree::Name(name) => {
            names.insert(name.ident.to_string());
        }
        syn::UseTree::Rename(rename) => {
            names.insert(rename.rename.to_string());
        }
        syn::UseTree::Group(group) => {
            for item in &group.items {
                collect_use_tree_names(item, names);
            }
        }
        syn::UseTree::Path(path) => collect_use_tree_names(&path.tree, names),
        syn::UseTree::Glob(_) => {}
    }
}

fn has_doc_hidden(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("doc")
            && match &attr.meta {
                syn::Meta::List(list) => list.tokens.to_string().contains("hidden"),
                syn::Meta::Path(_) | syn::Meta::NameValue(_) => false,
            }
    })
}

#[cfg(test)]
mod tests {
    use super::{check, doc_hidden_public_names};
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
            "batpak-public-surface-{name}-{}-{nanos}",
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

    /// END-TO-END RED FIXTURE: a `pub` item with NO witnessing test reference must
    /// make `check(..)` fail; a real AST-visible test use must make it pass. Both
    /// halves run so the test cannot pass vacuously — adding the witness (which
    /// neutralizes the violation) turns it red.
    #[test]
    fn pub_items_have_tests_rejects_unwitnessed_pub_item() {
        let repo = temp_repo("unwitnessed");
        write_file(
            &repo,
            "crates/core/src/widgets.rs",
            "pub struct WidgetProbe {\n    pub value: u8,\n}\n",
        );

        // GREEN: a test that constructs the type is an AST-visible witness.
        write_file(
            &repo,
            "crates/core/tests/widget.rs",
            "#[test]\nfn exercises() {\n    let _ = WidgetProbe { value: 1 };\n}\n",
        );
        let mut cache = SourceCache::new(&repo);
        check(&repo, &mut cache).expect("a witnessed pub item is accepted");

        // RED: remove the witness — the unreferenced pub item must be flagged.
        write_file(
            &repo,
            "crates/core/tests/widget.rs",
            "#[test]\nfn unrelated() {\n    assert_eq!(1, 1);\n}\n",
        );
        let mut cache = SourceCache::new(&repo);
        let err = check(&repo, &mut cache).expect_err("unwitnessed pub item is rejected");
        assert!(
            err.to_string().contains("WidgetProbe")
                && err.to_string().contains("has no test reference"),
            "{err:?}"
        );

        fs::remove_dir_all(repo).expect("remove temp repo");
    }

    #[test]
    fn doc_hidden_public_detector_finds_items_uses_and_impl_methods() {
        let ast = syn::parse_file(
            r#"
#[doc(hidden)]
pub struct HiddenStruct;

#[doc(hidden)]
pub use inner::HiddenUse;

#[doc(hidden)]
impl Store {
    pub fn hidden_method(&self) {}
}

impl Store {
    #[doc(hidden)]
    pub fn hidden_attr_method(&self) {}
}
"#,
        )
        .expect("parse fixture");
        let names = doc_hidden_public_names(&ast);
        assert!(names.contains("HiddenStruct"));
        assert!(names.contains("HiddenUse"));
        assert!(names.contains("hidden_method"));
        assert!(names.contains("hidden_attr_method"));
    }
}
