//! 02_MODEL.md → exported-symbol bindings (D5).
//!
//! 02_MODEL.md is narrative ontology. `traceability/model_bindings.yaml` pins each
//! concept it names (and each Beginner-Store-Path step) to a REAL entry in the
//! sealed public surface (`traceability/public_api/batpak.txt`). This gate makes
//! that binding NON-VACUOUS in BOTH directions:
//!   - the `doc_phrase` must appear verbatim in 02_MODEL.md (so a binding cannot
//!     drift away from a concept the doc actually names — delete the concept from
//!     02_MODEL.md and the binding fails), and
//!   - the `symbol` must appear in the public-API seal (so the model cannot
//!     describe a renamed/removed export — the exact docs-currency rot, e.g. the
//!     deleted `refbat` Family-Stack row).
//!
//! Folded into `traceability-check`.

use crate::repo_surface::load_yaml;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;

#[cfg(test)]
#[path = "model_bindings_tests.rs"]
mod model_bindings_tests;

#[derive(Debug, Deserialize)]
struct Binding {
    concept: String,
    doc_phrase: String,
    symbol: String,
    kind: String,
}

#[derive(Debug, Deserialize)]
struct ModelBindings {
    bindings: Vec<Binding>,
}

/// The recognized binding kinds (so a typo'd `kind` is a finding, not a silent
/// pass — the field is otherwise free-form).
const RECOGNIZED_KINDS: [&str; 2] = ["type", "method"];

pub(crate) fn model_bindings_path(repo_root: &Path) -> std::path::PathBuf {
    repo_root.join("traceability/model_bindings.yaml")
}

/// The repo-root 02_MODEL.md (one directory ABOVE the `bpk-lib` workspace root).
fn model_md_path(repo_root: &Path) -> std::path::PathBuf {
    repo_root
        .parent()
        .map(|p| p.join("02_MODEL.md"))
        .unwrap_or_else(|| repo_root.join("02_MODEL.md"))
}

fn public_seal_path(repo_root: &Path) -> std::path::PathBuf {
    repo_root.join("traceability/public_api/batpak.txt")
}

pub(crate) fn check(repo_root: &Path) -> Result<()> {
    let bindings: ModelBindings = load_yaml(&model_bindings_path(repo_root))?;
    let model_md = std::fs::read_to_string(model_md_path(repo_root))
        .with_context(|| format!("read {}", model_md_path(repo_root).display()))?;
    let seal = std::fs::read_to_string(public_seal_path(repo_root))
        .with_context(|| format!("read {}", public_seal_path(repo_root).display()))?;
    check_bindings(&bindings, &model_md, &seal)
}

fn check_bindings(bindings: &ModelBindings, model_md: &str, seal: &str) -> Result<()> {
    if bindings.bindings.is_empty() {
        bail!(
            "model-bindings: model_bindings.yaml binds no concepts — the surface would be vacuous."
        );
    }
    for b in &bindings.bindings {
        if !RECOGNIZED_KINDS.contains(&b.kind.as_str()) {
            bail!(
                "model-bindings: binding `{}` has unrecognized kind `{}` (want one of {:?}).",
                b.concept,
                b.kind,
                RECOGNIZED_KINDS
            );
        }
        if !model_md.contains(&b.doc_phrase) {
            bail!(
                "model-bindings: concept `{}` doc_phrase `{}` is NOT present in 02_MODEL.md. \
                 Either 02_MODEL.md dropped the concept (update the binding) or the phrase drifted.",
                b.concept,
                b.doc_phrase
            );
        }
        if !seal.contains(&b.symbol) {
            bail!(
                "model-bindings: concept `{}` binds symbol `{}` which is NOT in the public-API \
                 seal (traceability/public_api/batpak.txt). The model names an export that does \
                 not exist (or was renamed/removed) — fix 02_MODEL.md/the binding or re-seal.",
                b.concept,
                b.symbol
            );
        }
    }
    Ok(())
}
