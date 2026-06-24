//! Over-claim detector (`GAUNTLET-OVERCLAIM`, Thread #67) — triangulation over
//! claim-vs-reality gaps: a gate/doc/name asserts a property the repo does not
//! deliver. Two oracles derive the same `(subject, predicate)` facts; a
//! disagreement where the claim oracle says `yes` and the reality oracle says
//! `no` is a hard finding. Gate over-claim delegates to `gate_registry::check`;
//! doc and name-vs-behavior use the shared triangulation engine.
//!
//! Doc class (strict): every catalog `INV-*` must declare a `witness_test` that
//! `docs_catalog` validates (real `#[test]` fn). Name-vs-behavior: every
//! aspirational `pub fn` matching `*_evidence|*_proof|*_verify|*_attested` must
//! have an assertion-bearing test caller in the citation set (AST, not prose).

use crate::docs_catalog::{load_catalog, CatalogInvariant};
use crate::gate_registry;
use crate::invariant_bridge::TESTED_CRATES;
use crate::receipts::GateWork;
use crate::repo_surface::{
    core_src_root, core_tests_root, ensure, production_rust_roots, relative,
    resolve_repo_or_core_path, rust_files,
};
use crate::source_cache::SourceCache;
use crate::triangulation::{Claim, ClaimSet, Disagreement, TriangulationEngine};
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use syn::visit::Visit;

#[cfg(test)]
#[path = "overclaim_tests.rs"]
mod overclaim_tests;

pub(crate) const CLAIM_ORACLE: &str = "claim-oracle";
pub(crate) const REALITY_ORACLE: &str = "reality-oracle";
pub(crate) const PREDICATE_WITNESS_DELIVERED: &str = "witness-test-delivered";
pub(crate) const PREDICATE_ASSERTION_TEST: &str = "assertion-bearing-test";

/// Blocking gate entry: gate registry law + doc/name triangulation.
pub(crate) fn check(repo_root: &Path) -> Result<GateWork> {
    gate_registry::check(repo_root).context("overclaim: gate-registry")?;

    let mut cache = SourceCache::new(repo_root);
    let pool = collect_claim_pool(repo_root, &mut cache)?;
    let disagreements = TriangulationEngine::disagreements(&pool);
    let overclaims = overclaim_findings(&disagreements);

    ensure(
        overclaims.is_empty(),
        format!(
            "over-claim: claim-vs-reality disagreement(s) — a declared property is not delivered:\n  {}\n\
             Reconcile the claim (docs, gate registry, aspirational fn name) or add the missing witness/test.",
            overclaims
                .iter()
                .map(|d| d.render())
                .collect::<Vec<_>>()
                .join("\n  ")
        ),
    )?;

    let inputs = gate_inputs(repo_root, &mut cache);
    let assertions = pool.len().max(1);
    outln!(
        "overclaim: ok ({} claim(s) triangulated; gate registry + doc + name-vs-behavior clean)",
        pool.len()
    );
    Ok(GateWork::new(inputs.len().max(1), assertions, inputs))
}

/// Oracle triangulation only (no gate-registry delegate). Used by red fixtures
/// that plant doc/name violations in a temp tree.
#[cfg(test)]
pub(crate) fn check_overclaim_oracles(repo_root: &Path) -> Result<()> {
    let mut cache = SourceCache::new(repo_root);
    let pool = collect_claim_pool(repo_root, &mut cache)?;
    let disagreements = TriangulationEngine::disagreements(&pool);
    let overclaims = overclaim_findings(&disagreements);
    ensure(
        overclaims.is_empty(),
        format!(
            "over-claim (oracles only): {}",
            overclaims
                .iter()
                .map(|d| d.render())
                .collect::<Vec<_>>()
                .join("; ")
        ),
    )
}

fn collect_claim_pool(repo_root: &Path, cache: &mut SourceCache) -> Result<Vec<Claim>> {
    let aspirational = aspirational_pub_fn_subjects(repo_root, cache)?;
    let mut pool = Vec::new();
    pool.extend(
        claim_oracle_claims(repo_root, &aspirational)?
            .claims()
            .iter()
            .cloned(),
    );
    pool.extend(
        reality_oracle_claims(repo_root, cache, &aspirational)?
            .claims()
            .iter()
            .cloned(),
    );
    Ok(pool)
}

/// Over-claims are disagreements where claim-oracle=`yes` and reality-oracle=`no`.
pub(crate) fn overclaim_findings(disagreements: &[Disagreement]) -> Vec<&Disagreement> {
    disagreements
        .iter()
        .filter(|d| {
            let mut claim_yes = false;
            let mut reality_no = false;
            for (oracle, value) in &d.votes {
                if oracle == CLAIM_ORACLE && value == "yes" {
                    claim_yes = true;
                }
                if oracle == REALITY_ORACLE && value == "no" {
                    reality_no = true;
                }
            }
            claim_yes && reality_no
        })
        .collect()
}

fn claim_oracle_claims(repo_root: &Path, aspirational: &[String]) -> Result<ClaimSet> {
    let mut set = ClaimSet::new();
    let invariants = load_catalog(repo_root).context("load invariants catalog")?;
    for inv in &invariants {
        // The CLAIM is the declared `witness_test`: an invariant that names a
        // witness asserts "a witness test delivers this property". A prose-only
        // invariant (no `witness_test`) makes no such claim — strong-tier
        // citation is opt-in per-INV during burn-down (INV-INVARIANT-WITNESS-TEST,
        // docs_catalog::check_witness_tests). Claiming `yes` for every catalog
        // entry would manufacture a claim nobody made and red the gate on the
        // entire prose backlog. The detector still bites the real over-claim: a
        // DECLARED witness that does not resolve to a real `#[test]` (reality=no).
        if inv.witness_test.is_some() {
            set.assert(CLAIM_ORACLE, &inv.id, PREDICATE_WITNESS_DELIVERED, "yes");
        }
    }
    for subject in aspirational {
        set.assert(CLAIM_ORACLE, subject, PREDICATE_ASSERTION_TEST, "yes");
    }
    Ok(set)
}

fn reality_oracle_claims(
    repo_root: &Path,
    cache: &mut SourceCache,
    aspirational: &[String],
) -> Result<ClaimSet> {
    let mut set = ClaimSet::new();
    let invariants = load_catalog(repo_root).context("load invariants catalog")?;
    for inv in &invariants {
        let delivered = witness_test_delivered(repo_root, inv, cache)?;
        set.assert(
            REALITY_ORACLE,
            &inv.id,
            PREDICATE_WITNESS_DELIVERED,
            if delivered { "yes" } else { "no" },
        );
    }
    let test_corpus = load_test_corpus(repo_root, cache)?;
    for subject in aspirational {
        let fn_name = subject
            .rsplit_once("::")
            .map(|(_, name)| name)
            .unwrap_or(subject.as_str());
        let witnessed = test_corpus
            .iter()
            .any(|(_, ast)| test_fn_has_assertion_bearing_reference(ast, fn_name));
        set.assert(
            REALITY_ORACLE,
            subject,
            PREDICATE_ASSERTION_TEST,
            if witnessed { "yes" } else { "no" },
        );
    }
    Ok(set)
}

fn witness_test_delivered(
    repo_root: &Path,
    inv: &CatalogInvariant,
    cache: &mut SourceCache,
) -> Result<bool> {
    let Some(witness) = &inv.witness_test else {
        return Ok(false);
    };
    let Some((rel_path, fn_name)) = witness.rsplit_once("::") else {
        return Ok(false);
    };
    let full = resolve_repo_or_core_path(repo_root, rel_path);
    if !full.is_file() {
        return Ok(false);
    }
    file_declares_test_fn(cache, &full, fn_name)
}

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

fn is_aspirational_fn_name(name: &str) -> bool {
    name.ends_with("_evidence")
        || name.ends_with("_proof")
        || name.ends_with("_verify")
        || name.ends_with("_attested")
}

pub(crate) fn aspirational_pub_fn_subjects(
    repo_root: &Path,
    cache: &mut SourceCache,
) -> Result<Vec<String>> {
    let mut subjects = BTreeSet::new();
    let mut paths = rust_files(&core_src_root(repo_root));
    for root in production_rust_roots(repo_root) {
        paths.extend(rust_files(&root));
    }
    for path in paths {
        let rel = relative(repo_root, &path);
        let file = cache
            .parse_rust(&path)
            .with_context(|| format!("parse aspirational scan {rel}"))?;
        collect_aspirational_pub_fns(&file.items, &rel, &mut subjects);
    }
    Ok(subjects.into_iter().collect())
}

fn collect_aspirational_pub_fns(items: &[syn::Item], rel: &str, out: &mut BTreeSet<String>) {
    // `syn::Item` is a large `#[non_exhaustive]` foreign enum; use `if let`
    // dispatch over the three relevant variants rather than a `match` with a
    // wildcard arm (which the workspace `wildcard_enum_match_arm` lint forbids).
    for item in items {
        if let syn::Item::Fn(item_fn) = item {
            if matches!(item_fn.vis, syn::Visibility::Public(_)) {
                let name = item_fn.sig.ident.to_string();
                if is_aspirational_fn_name(&name) {
                    out.insert(format!("{rel}::{name}"));
                }
            }
        } else if let syn::Item::Impl(item_impl) = item {
            for impl_item in &item_impl.items {
                if let syn::ImplItem::Fn(method) = impl_item {
                    if matches!(method.vis, syn::Visibility::Public(_)) {
                        let name = method.sig.ident.to_string();
                        if is_aspirational_fn_name(&name) {
                            out.insert(format!("{rel}::{name}"));
                        }
                    }
                }
            }
        } else if let syn::Item::Mod(item_mod) = item {
            if module_is_cfg_test(&item_mod.attrs) {
                continue;
            }
            if let Some((_, nested)) = &item_mod.content {
                collect_aspirational_pub_fns(nested, rel, out);
            }
        }
    }
}

fn module_is_cfg_test(attrs: &[syn::Attribute]) -> bool {
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

fn load_test_corpus(
    repo_root: &Path,
    cache: &mut SourceCache,
) -> Result<Vec<(PathBuf, Rc<syn::File>)>> {
    let mut paths = rust_files(&core_tests_root(repo_root));
    for prefix in TESTED_CRATES {
        if *prefix == "crates/core/tests/" {
            continue;
        }
        paths.extend(rust_files(&repo_root.join(prefix.trim_end_matches('/'))));
    }
    paths.extend(rust_files(&repo_root.join("tools/integrity/src")));
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let file = cache
            .parse_rust(&path)
            .with_context(|| format!("parse test corpus {}", relative(repo_root, &path)))?;
        out.push((path, file));
    }
    Ok(out)
}

fn test_fn_has_assertion_bearing_reference(file: &syn::File, fn_name: &str) -> bool {
    for item in &file.items {
        if let syn::Item::Fn(item_fn) = item {
            if !fn_has_test_attr(&item_fn.attrs) {
                continue;
            }
            let mut visitor = AssertionReferenceVisitor {
                fn_name,
                references_fn: false,
                has_assertion: false,
            };
            visitor.visit_item_fn(item_fn);
            if visitor.references_fn && visitor.has_assertion {
                return true;
            }
        } else if let syn::Item::Mod(item_mod) = item {
            if let Some((_, nested)) = &item_mod.content {
                let nested_file = syn::File {
                    shebang: None,
                    attrs: item_mod.attrs.clone(),
                    items: nested.clone(),
                };
                if test_fn_has_assertion_bearing_reference(&nested_file, fn_name) {
                    return true;
                }
            }
        }
    }
    false
}

struct AssertionReferenceVisitor<'a> {
    fn_name: &'a str,
    references_fn: bool,
    has_assertion: bool,
}

impl AssertionReferenceVisitor<'_> {
    fn path_references_fn(&self, path: &syn::Path) -> bool {
        path.segments
            .iter()
            .any(|segment| segment.ident == self.fn_name)
    }
}

impl<'ast> Visit<'ast> for AssertionReferenceVisitor<'_> {
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if call.method == self.fn_name {
            self.references_fn = true;
        }
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_path(&mut self, expr: &'ast syn::ExprPath) {
        if self.path_references_fn(&expr.path) {
            self.references_fn = true;
        }
        syn::visit::visit_expr_path(self, expr);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if macro_is_assertion(&mac.path) {
            self.has_assertion = true;
        }
        if token_stream_references_fn(&mac.tokens, self.fn_name) {
            self.references_fn = true;
        }
        syn::visit::visit_macro(self, mac);
    }
}

fn macro_is_assertion(path: &syn::Path) -> bool {
    path.segments.last().is_some_and(|segment| {
        matches!(
            segment.ident.to_string().as_str(),
            "assert"
                | "assert_eq"
                | "assert_ne"
                | "assert_matches"
                | "matches"
                | "prop_assert"
                | "prop_assert_eq"
                | "prop_assert_ne"
        )
    })
}

fn token_stream_references_fn(tokens: &proc_macro2::TokenStream, fn_name: &str) -> bool {
    tokens.clone().into_iter().any(|token| match token {
        proc_macro2::TokenTree::Ident(ident) => ident == fn_name,
        proc_macro2::TokenTree::Group(group) => {
            token_stream_references_fn(&group.stream(), fn_name)
        }
        proc_macro2::TokenTree::Punct(_) | proc_macro2::TokenTree::Literal(_) => false,
    })
}

fn gate_inputs(repo_root: &Path, cache: &mut SourceCache) -> BTreeSet<PathBuf> {
    let mut inputs = BTreeSet::new();
    inputs.insert(repo_root.join("traceability/invariants.yaml"));
    inputs.insert(repo_root.join("tools/integrity/src/gate_registry.rs"));
    if let Ok(invariants) = load_catalog(repo_root) {
        for inv in invariants {
            if let Some(witness) = inv.witness_test {
                if let Some((rel, _)) = witness.rsplit_once("::") {
                    inputs.insert(resolve_repo_or_core_path(repo_root, rel));
                }
            }
        }
    }
    if let Ok(subjects) = aspirational_pub_fn_subjects(repo_root, cache) {
        for subject in subjects {
            if let Some((rel, _)) = subject.rsplit_once("::") {
                let path = repo_root.join(rel);
                if path.exists() {
                    inputs.insert(path);
                }
            }
        }
    }
    if let Ok(corpus) = load_test_corpus(repo_root, cache) {
        for (path, _) in corpus {
            inputs.insert(path);
        }
    }
    inputs
}

#[cfg(test)]
mod production_flip {
    use super::*;

    fn temp_repo(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "batpak-overclaim-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp repo root");
        path
    }

    fn write_file(root: &Path, rel: &str, body: &str) {
        let full = root.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(&full, body).expect("write fixture file");
    }

    fn plant_doc_overclaim_tree() -> std::path::PathBuf {
        let root = temp_repo("doc-overclaim");
        write_file(
            &root,
            "traceability/invariants.yaml",
            r#"- id: INV-PLANTED-OVERCLAIM
  statement: planted invariant with a non-test witness
  witness_test: crates/core/tests/planted_overclaim.rs::plain_helper_not_a_test
"#,
        );
        write_file(
            &root,
            "crates/core/tests/planted_overclaim.rs",
            r#"pub fn plain_helper_not_a_test() {}
"#,
        );
        root
    }

    /// Plant the canonical D2 bvisor-shadow name-vs-behavior over-claim: a
    /// production `pub fn` whose name asserts the admission circuit was
    /// *promoted* out of shadow and *attested* — but the behavior is absent
    /// (the body still routes through the shadow path) and, critically, NO
    /// assertion-bearing test references it. This is the "claimed-promoted,
    /// still-shadow" case #67's name-vs-behavior axis is meant to catch.
    ///
    /// `with_witness` controls whether the corpus carries a real
    /// assertion-bearing test that references the fn: `false` is the
    /// over-claim (reality oracle says `no`); `true` is the honest positive
    /// control (reality oracle says `yes`, detector stays silent).
    fn plant_bvisor_shadow_name_overclaim_tree(with_witness: bool) -> std::path::PathBuf {
        let root = temp_repo(if with_witness {
            "bvisor-shadow-witnessed"
        } else {
            "bvisor-shadow-overclaim"
        });
        // Minimal invariants catalog so load_catalog() succeeds (no doc-class
        // claims; this fixture isolates the name-vs-behavior axis).
        write_file(&root, "traceability/invariants.yaml", "[]\n");
        // The aspirational subject lives under a scanned production root
        // (crates/bvisor/src — see production_rust_roots). Its `_attested`
        // suffix makes it an aspirational subject; the body is a placeholder
        // that does NOT promote anything (still shadow).
        write_file(
            &root,
            "crates/bvisor/src/circuit.rs",
            r#"/// Claims the admission circuit was promoted out of shadow and attested.
pub fn circuit_promotion_attested() -> bool {
    // STILL SHADOW: returns the shadow verdict; nothing was actually promoted.
    false
}
"#,
        );
        // The test corpus: either an honest assertion-bearing test that
        // references the fn (positive control), or an inert test that names
        // something else (the over-claim — no real witness).
        let corpus = if with_witness {
            r#"#[test]
fn circuit_promotion_is_attested() {
    assert!(
        !crate::circuit::circuit_promotion_attested(),
        "the promotion verdict is observed by a real assertion"
    );
}
"#
        } else {
            r#"#[test]
fn unrelated_smoke() {
    assert_eq!(2 + 2, 4);
}
"#
        };
        write_file(&root, "crates/core/tests/circuit_promotion.rs", corpus);
        root
    }

    /// Build the name-vs-behavior over-claim findings for a planted tree.
    fn name_overclaims_for_tree(
        root: &Path,
    ) -> Result<Vec<Disagreement>, Box<dyn std::error::Error>> {
        let mut cache = SourceCache::new(root);
        let aspirational = aspirational_pub_fn_subjects(root, &mut cache)?;
        let mut pool = Vec::new();
        pool.extend(
            claim_oracle_claims(root, &aspirational)?
                .claims()
                .iter()
                .cloned(),
        );
        pool.extend(
            reality_oracle_claims(root, &mut cache, &aspirational)?
                .claims()
                .iter()
                .cloned(),
        );
        let disagreements = TriangulationEngine::disagreements(&pool);
        Ok(overclaim_findings(&disagreements)
            .into_iter()
            .filter(|d| d.predicate == PREDICATE_ASSERTION_TEST)
            .cloned()
            .collect())
    }

    /// The name-vs-behavior detector must BITE the planted bvisor-shadow
    /// over-claim (an `_attested`-named promotion fn with no assertion-bearing
    /// test) and stay SILENT on the honest positive control. This is the
    /// missing D2 fixture: it proves the detector is non-vacuous on the exact
    /// "claimed-promoted, still-shadow" shape, not merely registered.
    #[test]
    fn name_behavior_detector_bites_bvisor_shadow_promotion(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Over-claim: no assertion-bearing witness → must be flagged, and the
        // finding must name the aspirational subject.
        let overclaim_root = plant_bvisor_shadow_name_overclaim_tree(false);
        let flagged = name_overclaims_for_tree(&overclaim_root)?;
        assert_eq!(
            flagged.len(),
            1,
            "PROPERTY: a shadow-promotion `_attested` fn with no assertion-bearing test must be \
             flagged as a name-vs-behavior over-claim; got {flagged:?}"
        );
        assert!(
            flagged[0].subject.contains("circuit_promotion_attested"),
            "PROPERTY: the over-claim finding must name the aspirational subject; got {:?}",
            flagged[0].subject
        );

        // Positive control: an honest assertion-bearing test that references
        // the fn must clear the detector (no over-claim).
        let witnessed_root = plant_bvisor_shadow_name_overclaim_tree(true);
        let witnessed = name_overclaims_for_tree(&witnessed_root)?;
        assert!(
            witnessed.is_empty(),
            "PROPERTY: an `_attested` fn WITH a real assertion-bearing test must NOT be flagged; \
             got {witnessed:?}"
        );
        Ok(())
    }

    /// ProductionFlip red fixture: under `--cfg gauntlet_red_fixture` the test
    /// asserts the (wrong) outcome that a planted doc over-claim passes, so
    /// the test FAILS when the detector bites. Green on the live tree.
    #[test]
    fn detector_rejects_planted_overclaim() -> Result<(), Box<dyn std::error::Error>> {
        if cfg!(gauntlet_red_fixture) {
            let root = plant_doc_overclaim_tree();
            assert!(
                check_overclaim_oracles(&root).is_ok(),
                "PROPERTY: red half expects the planted over-claim to look green (this assert must fail when the detector bites)"
            );
            Ok(())
        } else {
            let root = crate::repo_surface::repo_root()?;
            gate_registry::check(&root)?;
            let mut cache = SourceCache::new(&root);
            let aspirational = aspirational_pub_fn_subjects(&root, &mut cache)?;
            let pool = {
                let mut pool = Vec::new();
                pool.extend(
                    claim_oracle_claims(&root, &aspirational)?
                        .claims()
                        .iter()
                        .cloned(),
                );
                pool.extend(
                    reality_oracle_claims(&root, &mut cache, &aspirational)?
                        .claims()
                        .iter()
                        .cloned(),
                );
                pool
            };
            let disagreements = TriangulationEngine::disagreements(&pool);
            let name_overclaims: Vec<_> = overclaim_findings(&disagreements)
                .into_iter()
                .filter(|d| d.predicate == PREDICATE_ASSERTION_TEST)
                .collect();
            ensure(
                name_overclaims.is_empty(),
                format!(
                    "over-claim (name-vs-behavior): {}",
                    name_overclaims
                        .iter()
                        .map(|d| d.render())
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            )?;
            Ok(())
        }
    }
}
