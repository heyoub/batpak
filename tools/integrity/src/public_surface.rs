use crate::repo_surface::{ensure, load_yaml, relative, rust_files};
use crate::shared_checks::{ast_references_name, public_item_names};
use anyhow::{Context, Result};
use serde::Deserialize;
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
    let allowlist: Vec<AllowlistEntry> =
        load_yaml(&repo_root.join("traceability/pub_item_allowlist.yaml"))?;
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
                witness.path.starts_with("tests/"),
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
            let abs = repo_root.join(&witness.path);
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

    let test_files: Vec<PathBuf> = rust_files(&repo_root.join("tests"));
    let mut parsed_tests: Vec<(PathBuf, syn::File)> = Vec::with_capacity(test_files.len());
    for path in test_files {
        let content = fs::read_to_string(&path)
            .with_context(|| format!("read {}", relative(repo_root, &path)))?;
        let file = syn::parse_file(&content)
            .with_context(|| format!("parse {}", relative(repo_root, &path)))?;
        parsed_tests.push((path, file));
    }

    for path in rust_files(&repo_root.join("src")) {
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
