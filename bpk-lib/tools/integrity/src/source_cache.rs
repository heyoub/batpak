use anyhow::{anyhow, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

enum ParsedRust {
    Ok(Rc<syn::File>),
    Err(String),
}

/// One cached Rust source file keyed by repo-relative path.
pub(crate) struct CachedSource {
    pub(crate) relative_path: PathBuf,
    pub(crate) text: Arc<str>,
    parsed_rust: Option<ParsedRust>,
}

/// Shared repository source cache for integrity checks.
///
/// Integrity checks should fold source files once, then project different
/// rule views from the cached text/AST. This keeps detector behavior aligned
/// and makes repeated structural runs cheaper to reason about.
pub(crate) struct SourceCache {
    root: PathBuf,
    files: BTreeMap<PathBuf, CachedSource>,
}

impl SourceCache {
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let root = root.canonicalize().unwrap_or(root);
        Self {
            root,
            files: BTreeMap::new(),
        }
    }

    pub(crate) fn rust_file(&mut self, relative_path: impl AsRef<Path>) -> Result<&CachedSource> {
        let key = normalize_relative_path(relative_path.as_ref());
        if !self.files.contains_key(&key) {
            let absolute = self.root.join(&key);
            let text = fs::read_to_string(&absolute)
                .with_context(|| format!("read {}", display_relative(&key)))?
                .into();
            self.files.insert(
                key.clone(),
                CachedSource {
                    relative_path: key.clone(),
                    text,
                    parsed_rust: None,
                },
            );
        }
        self.files
            .get(&key)
            .ok_or_else(|| anyhow!("source cache failed to retain {}", display_relative(&key)))
    }

    /// Walk a repo-relative directory, preload every `.rs` file, and return
    /// their normalized paths in deterministic sorted order.
    pub(crate) fn rust_files_under(
        &mut self,
        relative_dir: impl AsRef<Path>,
    ) -> Result<Vec<PathBuf>> {
        let dir = self.root.join(relative_dir.as_ref());
        if !dir.is_dir() {
            return Ok(Vec::new());
        }

        let mut relative_paths = BTreeSet::new();
        for entry in walkdir::WalkDir::new(&dir)
            .into_iter()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_file())
        {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let relative = path
                .strip_prefix(&self.root)
                .with_context(|| format!("strip repo root from {}", path.display()))?;
            relative_paths.insert(normalize_relative_path(relative));
        }

        let paths: Vec<PathBuf> = relative_paths.into_iter().collect();
        for relative in &paths {
            self.rust_file(relative)?;
        }
        Ok(paths)
    }

    pub(crate) fn read_to_string(&mut self, path: &Path) -> Result<Arc<str>> {
        let relative = self.relative_key(path)?;
        Ok(Arc::clone(&self.rust_file(&relative)?.text))
    }

    pub(crate) fn parse_rust(&mut self, path: &Path) -> Result<Rc<syn::File>> {
        let relative = self.relative_key(path)?;
        self.parse_rust_relative(&relative)
    }

    pub(crate) fn parse_rust_if_valid(&mut self, path: &Path) -> Result<Option<Rc<syn::File>>> {
        let relative = self.relative_key(path)?;
        Ok(match self.parse_rust_relative_result(&relative)? {
            ParsedRust::Ok(file) => Some(file),
            ParsedRust::Err(_) => None,
        })
    }

    fn parse_rust_relative(&mut self, relative: &Path) -> Result<Rc<syn::File>> {
        match self.parse_rust_relative_result(relative)? {
            ParsedRust::Ok(file) => Ok(file),
            ParsedRust::Err(error) => Err(anyhow!(
                "parse Rust source {}: {error}",
                display_relative(relative)
            )),
        }
    }

    fn parse_rust_relative_result(&mut self, relative: &Path) -> Result<ParsedRust> {
        let key = normalize_relative_path(relative);
        self.rust_file(&key)?;
        let cached = self
            .files
            .get_mut(&key)
            .ok_or_else(|| anyhow!("source cache failed to retain {}", display_relative(&key)))?;
        if cached.parsed_rust.is_none() {
            cached.parsed_rust = Some(match syn::parse_file(&cached.text) {
                Ok(file) => ParsedRust::Ok(Rc::new(file)),
                Err(error) => ParsedRust::Err(error.to_string()),
            });
        }
        match cached.parsed_rust.as_ref() {
            Some(ParsedRust::Ok(file)) => Ok(ParsedRust::Ok(Rc::clone(file))),
            Some(ParsedRust::Err(error)) => Ok(ParsedRust::Err(error.clone())),
            None => Err(anyhow!(
                "source cache failed to retain parse result {}",
                display_relative(&key)
            )),
        }
    }

    fn relative_key(&self, path: &Path) -> Result<PathBuf> {
        let canonical_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let path = canonical_path.as_path();

        if let Ok(relative) = path.strip_prefix(&self.root) {
            return Ok(normalize_relative_path(relative));
        }
        if let Some(parent) = self.root.parent() {
            if let Ok(relative) = path.strip_prefix(parent) {
                let relative = normalize_relative_path(relative);
                if let Some(root_name) = self.root.file_name() {
                    if let Ok(stripped) = relative.strip_prefix(root_name) {
                        return Ok(normalize_relative_path(stripped));
                    }
                }
                return Ok(relative);
            }
        }
        Err(anyhow!("resolve repo-relative path for {}", path.display()))
    }
}

fn normalize_relative_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                normalized.pop();
            }
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    PathBuf::from(normalized.to_string_lossy().replace('\\', "/"))
}

fn display_relative(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;
    use std::sync::Arc;

    fn write_temp_rust(dir: &Path, relative: &str, contents: &str) {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, contents).expect("write fixture");
    }

    #[test]
    fn cache_hit_avoids_reread() {
        let root = std::env::temp_dir().join(format!(
            "batpak-integrity-source-cache-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create temp root");
        write_temp_rust(&root, "a.rs", "fn once() {}\n");

        let mut cache = SourceCache::new(&root);
        let first_text = Arc::clone(&cache.rust_file("a.rs").expect("first load").text);
        let second_text = Arc::clone(&cache.rust_file("a.rs").expect("second load").text);
        assert!(Arc::ptr_eq(&first_text, &second_text));

        let parsed_a = cache.parse_rust(&root.join("a.rs")).expect("parse a");
        let parsed_b = cache.parse_rust(&root.join("a.rs")).expect("parse b");
        assert!(Rc::ptr_eq(&parsed_a, &parsed_b));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rust_files_under_is_sorted() {
        let root = std::env::temp_dir().join(format!(
            "batpak-integrity-source-cache-walk-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create temp root");
        write_temp_rust(&root, "src/z.rs", "fn z() {}\n");
        write_temp_rust(&root, "src/nested/a.rs", "fn a() {}\n");
        write_temp_rust(&root, "src/m.rs", "fn m() {}\n");

        let mut cache = SourceCache::new(&root);
        let paths = cache.rust_files_under("src").expect("walk src");
        let displayed: Vec<_> = paths.iter().map(|path| display_relative(path)).collect();
        assert_eq!(displayed, vec!["src/m.rs", "src/nested/a.rs", "src/z.rs"]);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parse_error_includes_relative_path() {
        let root = std::env::temp_dir().join(format!(
            "batpak-integrity-source-cache-parse-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create temp root");
        write_temp_rust(&root, "bad/syntax.rs", "fn broken( {}\n");

        let mut cache = SourceCache::new(&root);
        let err = cache
            .parse_rust(&root.join("bad/syntax.rs"))
            .err()
            .expect("invalid syntax must fail");
        let message = err.to_string();
        assert!(
            message.contains("bad/syntax.rs"),
            "expected relative path in error, got: {message}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn project_root_absolute_path_keeps_repo_relative_key() {
        let parent = std::env::temp_dir().join(format!(
            "batpak-integrity-source-cache-project-root-{}",
            std::process::id()
        ));
        let root = parent.join("bpk-lib");
        let _ = fs::remove_dir_all(&parent);
        fs::create_dir_all(&root).expect("create temp root");
        write_temp_rust(&root, "tools/integrity/src/lib.rs", "fn ok() {}\n");

        let mut cache = SourceCache::new(&root);
        let absolute = root.join("tools/integrity/src/lib.rs");
        let parsed = cache.parse_rust(&absolute).expect("parse absolute path");
        assert_eq!(parsed.items.len(), 1);

        let cached = cache
            .rust_file("tools/integrity/src/lib.rs")
            .expect("repo-relative cache key");
        assert_eq!(
            display_relative(&cached.relative_path),
            "tools/integrity/src/lib.rs"
        );
        assert!(!cache
            .files
            .contains_key(Path::new("bpk-lib/tools/integrity/src/lib.rs")));

        let _ = fs::remove_dir_all(&parent);
    }
}
