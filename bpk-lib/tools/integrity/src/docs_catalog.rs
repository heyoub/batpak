//! GAUNTLET-DOCS-CURRENCY: make `INVARIANTS.md` a generated VIEW of the
//! `traceability/invariants.yaml` catalog instead of hand-synced prose.
//!
//! INV-DOCS-CATALOG-VIEW-CURRENT: the auto-generated INV catalog block in
//! `INVARIANTS.md` (between the `<!-- BEGIN INV-CATALOG -->` /
//! `<!-- END INV-CATALOG -->` markers) lists every catalog id + its one-line
//! statement. The human prose ABOVE the block stays authored; the block is a
//! pure projection of the catalog. `--check` mode fails CI on any drift so the
//! docs can never silently rot away from machine law.
//!
//! INV-INVARIANT-WITNESS-TEST: every catalog invariant names a `witness_test`
//! whose `path::fn` resolves to a real `#[test]` (or `proptest!`-defined test)
//! in the tree. This upgrades the weak "INV string appears in a header"
//! citation to a strong "a named test function exercises this INV" gate. The
//! gate is opt-in per-INV during burn-down: an INV with no `witness_test` is
//! skipped by the strong tier (it still rides the existing weak citation /
//! ledger escalation path in `invariant_bridge`), but once a `witness_test` is
//! declared it MUST resolve to a real test.

use crate::repo_surface::{ensure, load_yaml, project_root, resolve_repo_or_core_path};
use crate::source_cache::SourceCache;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::Path;

pub(crate) const BEGIN_MARKER: &str = "<!-- BEGIN INV-CATALOG -->";
pub(crate) const END_MARKER: &str = "<!-- END INV-CATALOG -->";

#[derive(Debug, Deserialize)]
pub(crate) struct CatalogInvariant {
    pub(crate) id: String,
    pub(crate) statement: String,
    /// Strong-tier citation: `path::fn` (relative to repo root) that exercises
    /// this invariant. Optional during burn-down; once present it is enforced.
    #[serde(default)]
    pub(crate) witness_test: Option<String>,
}

/// Top-level entry: generate the catalog block and either splice it into
/// `INVARIANTS.md` (write mode) or assert the file already matches (check mode).
/// The witness-test gate runs in BOTH modes (it is independent of MD drift).
pub(crate) fn run(repo_root: &Path, check: bool) -> Result<()> {
    let invariants = load_catalog(repo_root)?;
    let mut cache = SourceCache::new(repo_root);
    check_witness_tests(repo_root, &invariants, &mut cache)?;
    // Anti-rot: the README headline count must match the live catalog sizes (it is an
    // easy-to-look-legitimate claim that silently rots as the catalog grows). Runs in
    // both modes — it is independent of INVARIANTS.md drift.
    check_readme_counts(repo_root, invariants.len())?;

    let block = render_catalog_block(&invariants);
    // INVARIANTS.md lives at the true repo root (parent of the cargo workspace
    // `bpk-lib`); traceability/* lives inside the workspace. Resolve each from
    // its own root so the gate works regardless of where the binary is invoked.
    let md_path = project_root(repo_root).join("INVARIANTS.md");
    let current =
        std::fs::read_to_string(&md_path).with_context(|| format!("read {}", md_path.display()))?;
    let next = splice_catalog_block(&current, &block)?;

    if check {
        ensure(
            current == next,
            "INVARIANTS.md is stale: the generated INV catalog block does not match \
             traceability/invariants.yaml. Run `cargo xtask docs` (or \
             `cargo run -p batpak-integrity -- docs-catalog`) to regenerate it.",
        )?;
        outln!(
            "docs-catalog: ok ({} invariants, catalog block current)",
            invariants.len()
        );
    } else if current != next {
        std::fs::write(&md_path, &next).with_context(|| format!("write {}", md_path.display()))?;
        outln!(
            "docs-catalog: regenerated INVARIANTS.md catalog block ({} invariants)",
            invariants.len()
        );
    } else {
        outln!(
            "docs-catalog: INVARIANTS.md already current ({} invariants)",
            invariants.len()
        );
    }
    Ok(())
}

pub(crate) fn load_catalog(repo_root: &Path) -> Result<Vec<CatalogInvariant>> {
    let path = repo_root.join("traceability").join("invariants.yaml");
    load_yaml(&path).context("invariants")
}

/// Anti-rot gate: the README's "N named invariants traced to M concrete artifacts" line
/// must match the live `invariants.yaml` / `artifacts.yaml` sizes. Bind a headline claim
/// to machine reality so it cannot silently rot as the catalog grows.
pub(crate) fn check_readme_counts(repo_root: &Path, invariant_count: usize) -> Result<()> {
    // Count artifact rows without coupling to the full `ArtifactRecord` schema: each list
    // element deserializes as `IgnoredAny` (content ignored) and is counted.
    let artifacts: Vec<serde::de::IgnoredAny> =
        load_yaml(&repo_root.join("traceability").join("artifacts.yaml")).context("artifacts")?;
    let artifact_count = artifacts.len();
    let md_path = project_root(repo_root).join("README.md");
    let readme =
        std::fs::read_to_string(&md_path).with_context(|| format!("read {}", md_path.display()))?;
    let (named, traced) = parse_readme_counts(&readme).with_context(|| {
        "README.md is missing the `N named invariants traced to M concrete artifacts` line"
            .to_string()
    })?;
    ensure(
        named == invariant_count,
        format!(
            "README.md claims {named} named invariants, but traceability/invariants.yaml has \
             {invariant_count}. Update the README headline (or the catalog) so the claim is true."
        ),
    )?;
    ensure(
        traced == artifact_count,
        format!(
            "README.md claims {traced} concrete artifacts, but traceability/artifacts.yaml has \
             {artifact_count}. Update the README headline (or the catalog) so the claim is true."
        ),
    )?;
    Ok(())
}

/// Pull `(N, M)` out of `N named invariants traced to M concrete artifacts`. `N` is the
/// integer token immediately before the phrase; `M` the integer token immediately after.
pub(crate) fn parse_readme_counts(readme: &str) -> Option<(usize, usize)> {
    let marker = "named invariants traced to";
    let idx = readme.find(marker)?;
    let before = &readme[..idx];
    let after = &readme[idx + marker.len()..];
    let named = before.split_whitespace().last()?.parse().ok()?;
    let traced = after.split_whitespace().next()?.parse().ok()?;
    Some((named, traced))
}

/// Render the deterministic catalog block (sorted by id) that lives between the
/// markers. Newlines only; no trailing whitespace, so the round-trip is stable.
pub(crate) fn render_catalog_block(invariants: &[CatalogInvariant]) -> String {
    let mut sorted: Vec<&CatalogInvariant> = invariants.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    let mut out = String::new();
    out.push_str(
        "_Generated from `bpk-lib/traceability/invariants.yaml` by \
         `just docs`. Do not edit by hand; run the generator._\n\n",
    );
    out.push_str("| Invariant | Statement |\n");
    out.push_str("| --- | --- |\n");
    for invariant in sorted {
        let statement = one_line(&invariant.statement);
        out.push_str(&format!("| `{}` | {} |\n", invariant.id, statement));
    }
    out
}

/// Collapse a possibly multi-line YAML statement into a single Markdown table
/// cell: squash whitespace and escape the pipe so it never breaks the row.
fn one_line(statement: &str) -> String {
    statement
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

/// Replace the content between the markers with `block`. Both markers must be
/// present exactly once; this keeps the human prose outside the markers intact.
pub(crate) fn splice_catalog_block(current: &str, block: &str) -> Result<String> {
    let begin = find_unique(current, BEGIN_MARKER)?;
    let end = find_unique(current, END_MARKER)?;
    if end <= begin {
        bail!("INVARIANTS.md: {END_MARKER} appears before {BEGIN_MARKER}");
    }
    let prefix = &current[..begin + BEGIN_MARKER.len()];
    let suffix = &current[end..];
    Ok(format!("{prefix}\n\n{block}\n{suffix}"))
}

fn find_unique(haystack: &str, needle: &str) -> Result<usize> {
    let first = haystack
        .find(needle)
        .with_context(|| format!("INVARIANTS.md is missing the `{needle}` marker"))?;
    if haystack[first + needle.len()..].contains(needle) {
        bail!("INVARIANTS.md contains more than one `{needle}` marker");
    }
    Ok(first)
}

/// Strong-tier citation gate: for every invariant that declares a
/// `witness_test: "path::fn"`, assert the file exists, parses, and declares a
/// function named `fn` that is a `#[test]` (or a `proptest!`-defined test). A
/// plain non-test `fn`, a missing fn, or a missing file is a hard failure.
pub(crate) fn check_witness_tests(
    repo_root: &Path,
    invariants: &[CatalogInvariant],
    cache: &mut SourceCache,
) -> Result<()> {
    let mut seen_witnessed = BTreeSet::new();
    for invariant in invariants {
        let Some(witness) = &invariant.witness_test else {
            continue;
        };
        seen_witnessed.insert(invariant.id.clone());
        let (rel_path, fn_name) = witness.rsplit_once("::").with_context(|| {
            format!(
                "invariant {} witness_test `{witness}` must be `path::fn`",
                invariant.id
            )
        })?;
        let full = resolve_repo_or_core_path(repo_root, rel_path);
        ensure(
            full.is_file(),
            format!(
                "invariant {} witness_test `{witness}` points at a missing file {rel_path}",
                invariant.id
            ),
        )?;
        let declares = file_declares_test_fn(cache, &full, fn_name)?;
        ensure(
            declares,
            format!(
                "invariant {} witness_test `{witness}` names no `#[test]`/`fn {fn_name}` in {rel_path}",
                invariant.id
            ),
        )?;
    }
    if !seen_witnessed.is_empty() {
        outln!(
            "docs-catalog: {} invariant(s) carry a resolved witness_test",
            seen_witnessed.len()
        );
    }
    Ok(())
}

/// Parse the file via the shared cache and report whether it declares a
/// TEST named `fn_name`: either an `Item::Fn` carrying a `#[test]` attribute,
/// or a `fn fn_name` that appears inside a `proptest!{...}` macro body (which
/// expands into a `#[test]` fn syn cannot see directly). A plain non-`#[test]`
/// `fn` is intentionally REJECTED so the strong tier proves a real test.
fn file_declares_test_fn(cache: &mut SourceCache, path: &Path, fn_name: &str) -> Result<bool> {
    let parsed = cache.parse_rust(path)?;
    Ok(item_test_fns_match(&parsed.items, fn_name))
}

fn item_test_fns_match(items: &[syn::Item], fn_name: &str) -> bool {
    for item in items {
        if let syn::Item::Fn(item_fn) = item {
            if item_fn.sig.ident == fn_name && fn_has_test_attr(&item_fn.attrs) {
                return true;
            }
        } else if let syn::Item::Macro(item_macro) = item {
            if macro_is_proptest(&item_macro.mac)
                && proptest_body_declares_fn(&item_macro.mac.tokens, fn_name)
            {
                return true;
            }
        } else if let syn::Item::Mod(item_mod) = item {
            if let Some((_, nested)) = &item_mod.content {
                if item_test_fns_match(nested, fn_name) {
                    return true;
                }
            }
        }
    }
    false
}

fn fn_has_test_attr(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        let path = attr.path();
        path.is_ident("test")
            || path
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "test")
    })
}

fn macro_is_proptest(mac: &syn::Macro) -> bool {
    mac.path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "proptest")
}

/// proptest!{ fn name(...) {...} } — scan the raw token stream for `fn <name>`.
fn proptest_body_declares_fn(tokens: &proc_macro2::TokenStream, fn_name: &str) -> bool {
    let mut prev_was_fn = false;
    for tree in tokens.clone() {
        if let proc_macro2::TokenTree::Ident(ident) = tree {
            if prev_was_fn && ident == fn_name {
                return true;
            }
            prev_was_fn = ident == "fn";
        } else if let proc_macro2::TokenTree::Group(group) = tree {
            if proptest_body_declares_fn(&group.stream(), fn_name) {
                return true;
            }
            prev_was_fn = false;
        } else {
            prev_was_fn = false;
        }
    }
    false
}

#[cfg(test)]
#[path = "docs_catalog_tests.rs"]
mod docs_catalog_tests;
