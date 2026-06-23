//! Traceability anchor resolution for the batpak-integrity binary.
//!
//! An "anchor" is a token that points at a real piece of the assurance corpus:
//! an `INV-<NAME>` from the invariant catalog, an `ADR-NNNN` whose root ADR file
//! exists, or a concrete repo path. `invariant_bridge` and `typed_waivers` use
//! these to prove that a cited rationale resolves to something real rather than
//! narrative-only prose.
//!
//! These helpers previously lived in `crates/core/build_support/shared_checks.rs`
//! because build.rs's old allow-justification check reused them. Under the
//! zero-allow doctrine (INV-ALLOW-IS-DESIGN) build.rs no longer justifies allows,
//! so anchor resolution is now an integrity-binary-only concern and lives here.
//! The lower-level ADR/path primitives (`adr_file_with_prefix_exists`,
//! `resolve_repo_or_core_path`) stay in `shared_checks` because build.rs's
//! dead-code-silencer allowlist loader still uses them; we call back into them.

use crate::shared_checks::{adr_file_with_prefix_exists, resolve_repo_or_core_path};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// A resolvable traceability anchor extracted from a rationale body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JustifiesAnchor {
    Invariant(String),
    Adr(u32),
    Path(PathBuf),
}

/// Extract resolvable anchors from a rationale body. Anchors are `INV-<NAME>`,
/// `ADR-NNNN`, and repo-relative paths (`src/...`, `tests/...`, `examples/...`,
/// etc. — ending in `.rs`, `.md`, `.yaml`, or `.toml`, with an optional `:line`
/// suffix).
pub(crate) fn extract_anchors(body: &str) -> Vec<JustifiesAnchor> {
    let mut out = Vec::new();
    for tok in body.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
        let tok = tok.trim_matches(|c: char| {
            c == '(' || c == ')' || c == '\'' || c == '"' || c == '.' || c == '`'
        });
        if tok.is_empty() {
            continue;
        }
        if let Some(rest) = tok.strip_prefix("INV-") {
            if !rest.is_empty()
                && rest
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-' || c == '_')
            {
                out.push(JustifiesAnchor::Invariant(format!("INV-{rest}")));
                continue;
            }
        }
        if let Some(digits) = tok.strip_prefix("ADR-") {
            let digits = digits.trim_end_matches(|c: char| !c.is_ascii_digit());
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                if let Ok(n) = digits.parse::<u32>() {
                    out.push(JustifiesAnchor::Adr(n));
                    continue;
                }
            }
        }
        let starts_with_dir = [
            "src/",
            "tests/",
            "examples/",
            "crates/core/src/",
            "crates/core/tests/",
            "crates/examples/examples/",
            "crates/core/benches/",
            "crates/core/fixtures/",
            "crates/macros/",
            "crates/macros-support/",
            "benches/",
            "tools/",
            "fixtures/",
            "archive/decisions/",
            "cookbook/",
            "traceability/",
        ]
        .iter()
        .any(|p| tok.starts_with(p));
        let is_build_rs = tok == "build.rs" || tok.starts_with("build.rs:");
        if starts_with_dir || is_build_rs {
            let file = tok
                .rsplit_once(':')
                .and_then(|(before, after)| {
                    if after.chars().all(|c| c.is_ascii_digit()) {
                        Some(before)
                    } else {
                        None
                    }
                })
                .unwrap_or(tok);
            let ok_ext = [".rs", ".md", ".yaml", ".toml"]
                .iter()
                .any(|ext| file.ends_with(ext));
            if ok_ext {
                out.push(JustifiesAnchor::Path(PathBuf::from(file)));
            }
        }
    }
    out
}

pub(crate) fn load_known_invariants(repo_root: &Path) -> Result<BTreeSet<String>, String> {
    let path = repo_root.join("traceability/invariants.yaml");
    let text = fs::read_to_string(&path)
        .map_err(|_| format!("cannot read {} to verify anchors", path.display()))?;
    #[derive(Deserialize)]
    struct InvRecord {
        id: String,
    }
    let records: Vec<InvRecord> =
        yaml_serde::from_str(&text).map_err(|e| format!("parse {}: {}", path.display(), e))?;
    Ok(records.into_iter().map(|r| r.id).collect())
}

pub(crate) fn resolve_anchor(
    anchor: &JustifiesAnchor,
    repo_root: &Path,
    known_invariants: &BTreeSet<String>,
) -> bool {
    match anchor {
        JustifiesAnchor::Invariant(id) => known_invariants.contains(id),
        JustifiesAnchor::Adr(n) => {
            let prefix = format!("ADR-{n:04}");
            adr_file_with_prefix_exists(repo_root, &prefix)
        }
        JustifiesAnchor::Path(rel) => resolve_repo_or_core_path(repo_root, rel).exists(),
    }
}
