use crate::repo_surface::{
    core_src_root, core_tests_root, ensure, load_yaml, relative, resolve_repo_or_core_path,
    rust_files,
};
use crate::shared_checks::{ast_references_name, public_item_names};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
struct AllowlistEntry {
    name: String,
    justification: String,
    witness: Vec<AllowlistWitness>,
}

#[derive(Debug, Deserialize)]
struct AllowlistWitness {
    path: String,
    // justifies: INV-TRACEABILITY-COMPLETE; lines is supplementary line-number metadata for human review; the AST walker in tools/integrity/src/structural.rs verifies the path contains the item regardless of specific lines
    #[serde(default)]
    lines: Vec<u32>,
}

impl AllowlistWitness {
    fn line_hints(&self) -> &[u32] {
        &self.lines
    }
}

pub(crate) fn check(repo_root: &Path) -> Result<()> {
    check_doc_hidden_public_surface(repo_root)?;

    let allowlist: Vec<AllowlistEntry> =
        load_yaml(&repo_root.join("traceability/pub_item_allowlist.yaml"))?;
    check_internal_justification_grace(&allowlist)?;
    let allowed: HashMap<&str, &AllowlistEntry> = allowlist
        .iter()
        .map(|entry| (entry.name.as_str(), entry))
        .collect();

    // For every allowlist entry, validate every witness path:
    //   - file must exist
    //   - file must parse as Rust
    //   - file must contain a real AST reference to the item name (not just a
    //     substring in a string literal or comment)
    for entry in &allowlist {
        ensure(
            !entry.justification.trim().is_empty(),
            format!(
                "pub_item_allowlist entry `{}` must include a non-empty supplementary `justification:`",
                entry.name
            ),
        )?;
        ensure(
            !entry.witness.is_empty(),
            format!(
                "pub_item_allowlist entry `{}` must declare at least one `witness:` path pointing at a test that uses the item; narrative `justification:` is supplementary, not load-bearing",
                entry.name
            ),
        )?;
        for witness in &entry.witness {
            ensure(
                witness.path.starts_with("tests/")
                    || witness.path.starts_with("crates/core/tests/"),
                format!(
                    "pub_item_allowlist entry `{}` witness `{}` must point at a file under tests/, not production code",
                    entry.name, witness.path
                ),
            )?;
            ensure(
                !witness.line_hints().is_empty(),
                format!(
                    "pub_item_allowlist entry `{}` witness `{}` must include at least one concrete line hint",
                    entry.name, witness.path
                ),
            )?;
            let abs = resolve_repo_or_core_path(repo_root, &witness.path);
            ensure(
                abs.exists(),
                format!(
                    "pub_item_allowlist entry `{}` declares witness path `{}` but that file does not exist",
                    entry.name, witness.path
                ),
            )?;
            let content = fs::read_to_string(&abs)
                .with_context(|| format!("read witness {}", witness.path))?;
            let file = syn::parse_file(&content)
                .with_context(|| format!("parse witness {}", witness.path))?;
            ensure(
                ast_references_name(&file, &entry.name),
                format!(
                    "pub_item_allowlist entry `{}` witness `{}` (line hints {:?}) does not contain a real path-position reference to `{}`; either update the witness path or hide the item via `#[doc(hidden)]`",
                    entry.name,
                    witness.path,
                    witness.line_hints(),
                    entry.name,
                ),
            )?;
        }
    }

    let test_files: Vec<PathBuf> = rust_files(&core_tests_root(repo_root));
    let mut parsed_tests: Vec<(PathBuf, syn::File)> = Vec::with_capacity(test_files.len());
    for path in test_files {
        let content = fs::read_to_string(&path)
            .with_context(|| format!("read {}", relative(repo_root, &path)))?;
        let file = syn::parse_file(&content)
            .with_context(|| format!("parse {}", relative(repo_root, &path)))?;
        parsed_tests.push((path, file));
    }

    for path in rust_files(&core_src_root(repo_root)) {
        if path.ends_with("prelude.rs") {
            continue;
        }
        let content = fs::read_to_string(&path)?;
        let file = syn::parse_file(&content)
            .with_context(|| format!("parse {}", relative(repo_root, &path)))?;
        for name in public_item_names(&file) {
            if allowed.contains_key(name.as_str()) {
                continue;
            }
            let found = parsed_tests
                .iter()
                .any(|(_, ast)| ast_references_name(ast, &name));
            ensure(
                found,
                format!(
                    "pub item `{}` declared at {} has no test reference (checked {} test files via AST); either add a real test use, add an allowlist entry with a `witness:` path that points to an actual use, or hide the item via `#[doc(hidden)]`.",
                    name,
                    relative(repo_root, &path),
                    parsed_tests.len(),
                ),
            )?;
        }
    }
    Ok(())
}

fn check_internal_justification_grace(allowlist: &[AllowlistEntry]) -> Result<()> {
    let grace: BTreeSet<&str> = ["needs_rotation"].into_iter().collect();
    let mut unexpected = Vec::new();
    let mut stale_grace = grace.clone();

    for entry in allowlist {
        let justification = entry.justification.to_ascii_lowercase();
        if !justification.contains("internal") {
            continue;
        }
        stale_grace.remove(entry.name.as_str());
        if !grace.contains(entry.name.as_str()) {
            unexpected.push(entry.name.clone());
        }
    }

    ensure(
        unexpected.is_empty(),
        format!(
            "pub_item_allowlist justification may not describe public API as internal; fix or hide: {}",
            unexpected.join(", ")
        ),
    )?;
    ensure(
        stale_grace.is_empty(),
        format!(
            "pub_item_allowlist internal-justification grace is stale; remove grace for: {}",
            stale_grace.into_iter().collect::<Vec<_>>().join(", ")
        ),
    )
}

fn check_doc_hidden_public_surface(repo_root: &Path) -> Result<()> {
    let allowed: BTreeSet<&str> = [
        "crates/core/src/lib.rs::__private",
        "crates/core/src/lib.rs::batpak",
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
        let content = fs::read_to_string(&path)?;
        let file = syn::parse_file(&content)
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
    use super::doc_hidden_public_names;

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
