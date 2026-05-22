use anyhow::{anyhow, Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

enum ParsedRust {
    Ok(Arc<syn::File>),
    Err(String),
}

struct CachedSource {
    text: Arc<str>,
    parsed_rust: Option<ParsedRust>,
}

/// Shared repository source cache for integrity checks.
///
/// Integrity checks should fold source files once, then project different
/// rule views from the cached text/AST. This keeps detector behavior aligned
/// and makes repeated structural runs cheaper to reason about.
pub(crate) struct SourceCache {
    files: BTreeMap<PathBuf, CachedSource>,
}

impl SourceCache {
    pub(crate) fn new() -> Self {
        Self {
            files: BTreeMap::new(),
        }
    }

    pub(crate) fn read_to_string(&mut self, path: &Path) -> Result<Arc<str>> {
        self.ensure_source(path)
            .map(|source| Arc::clone(&source.text))
    }

    pub(crate) fn parse_rust(&mut self, path: &Path) -> Result<Arc<syn::File>> {
        let source = self.parsed_rust(path)?;
        match source {
            ParsedRust::Ok(file) => Ok(Arc::clone(file)),
            ParsedRust::Err(error) => Err(anyhow!("parse Rust source {}: {error}", path.display())),
        }
    }

    pub(crate) fn parse_rust_if_valid(&mut self, path: &Path) -> Result<Option<Arc<syn::File>>> {
        Ok(match self.parsed_rust(path)? {
            ParsedRust::Ok(file) => Some(Arc::clone(file)),
            ParsedRust::Err(_) => None,
        })
    }

    fn parsed_rust(&mut self, path: &Path) -> Result<&ParsedRust> {
        let source = self.ensure_source(path)?;
        if source.parsed_rust.is_none() {
            source.parsed_rust = Some(match syn::parse_file(&source.text) {
                Ok(file) => ParsedRust::Ok(Arc::new(file)),
                Err(error) => ParsedRust::Err(error.to_string()),
            });
        }

        source.parsed_rust.as_ref().ok_or_else(|| {
            anyhow!(
                "source cache failed to retain parse result {}",
                path.display()
            )
        })
    }

    fn ensure_source(&mut self, path: &Path) -> Result<&mut CachedSource> {
        if !self.files.contains_key(path) {
            let text = fs::read_to_string(path)
                .with_context(|| format!("read {}", path.display()))?
                .into();
            self.files.insert(
                path.to_path_buf(),
                CachedSource {
                    text,
                    parsed_rust: None,
                },
            );
        }
        self.files
            .get_mut(path)
            .ok_or_else(|| anyhow!("source cache failed to retain {}", path.display()))
    }
}

pub(crate) fn path_segments(path: &syn::Path) -> Vec<String> {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect()
}
