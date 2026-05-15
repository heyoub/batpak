use proc_macro2::Span;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::Token;

/// An anchor extracted from a structured `// justifies:` comment body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JustifiesAnchor {
    Invariant(String),
    Adr(u32),
    Path(PathBuf),
}

/// Extract the prose body after `// justifies:` from a single source line.
pub(crate) fn justification_body(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if let Some(idx) = trimmed.find("// justifies:") {
        return Some(trimmed[idx + "// justifies:".len()..].trim().to_string());
    }
    if trimmed.starts_with("//") {
        let stripped = trimmed.trim_start_matches('/').trim();
        if let Some(body) = stripped.strip_prefix("justifies:") {
            return Some(body.trim().to_string());
        }
    }
    None
}

/// Extract resolvable anchors from a justification body. Anchors are
/// `INV-<NAME>`, `ADR-NNNN`, and repo-relative paths (`src/...`, `tests/...`,
/// `examples/...`, etc. — ending in `.rs`, `.md`, `.yaml`, or `.toml`, with an
/// optional `:line` suffix).
pub(crate) fn extract_anchors(body: &str) -> Vec<JustifiesAnchor> {
    let mut out = Vec::new();
    for tok in body.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
        let tok =
            tok.trim_matches(|c: char| c == '(' || c == ')' || c == '\'' || c == '"' || c == '.');
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
            "crates/core/examples/",
            "crates/core/benches/",
            "crates/core/fixtures/",
            "crates/macros/",
            "crates/macros-support/",
            "benches/",
            "tools/",
            "fixtures/",
            "docs/",
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
    let text = fs::read_to_string(&path).map_err(|_| {
        format!(
            "cannot read {} to verify justifies: anchors",
            path.display()
        )
    })?;
    #[derive(Deserialize)]
    struct InvRecord {
        id: String,
    }
    let records: Vec<InvRecord> =
        yaml_serde::from_str(&text).map_err(|e| format!("parse {}: {}", path.display(), e))?;
    Ok(records.into_iter().map(|r| r.id).collect())
}

fn resolve_anchor(
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

fn adr_file_with_prefix_exists(repo_root: &Path, prefix: &str) -> bool {
    adr_search_dirs(repo_root).into_iter().any(|dir| {
        fs::read_dir(&dir)
            .ok()
            .map(|it| {
                it.flatten().any(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with(prefix))
                })
            })
            .unwrap_or(false)
    })
}

fn adr_search_dirs(repo_root: &Path) -> [PathBuf; 2] {
    [repo_root.join("docs"), repo_root.join("docs/adr")]
}

fn resolve_repo_or_core_path(repo_root: &Path, rel: &Path) -> PathBuf {
    let direct = repo_root.join(rel);
    if direct.exists() {
        return direct;
    }
    if is_primary_crate_relative_path(rel) {
        return repo_root.join("crates/core").join(rel);
    }
    direct
}

fn is_primary_crate_relative_path(rel: &Path) -> bool {
    let rel = rel.to_string_lossy().replace('\\', "/");
    rel == "build.rs"
        || rel.starts_with("build.rs:")
        || rel.starts_with("src/")
        || rel.starts_with("tests/")
        || rel.starts_with("examples/")
        || rel.starts_with("benches/")
        || rel.starts_with("fixtures/")
}

/// Parse a single source line and return true if it carries a structured
/// justification comment with (a) >= 5 words of prose and (b) >= 1
/// anchor that resolves against the current repo. See INV-ALLOW-IS-DESIGN.
pub(crate) fn line_carries_justification(
    line: &str,
    repo_root: &Path,
    known_invariants: &BTreeSet<String>,
) -> bool {
    let Some(body) = justification_body(line) else {
        return false;
    };
    if body.split_whitespace().count() < 5 {
        return false;
    }
    extract_anchors(&body)
        .iter()
        .any(|anchor| resolve_anchor(anchor, repo_root, known_invariants))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeadCodeSilencerSite {
    pub line: usize,
    pub column: usize,
    pub rendered: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct DeadCodeSilencerAllowlistEntry {
    pub path: String,
    pub reason: String,
    pub adr: String,
}

/// Collect every attribute site that silences `dead_code`, either directly,
/// transitively via the `unused` lint group, or through a `cfg_attr(...)`
/// wrapper. This is AST-based, so multi-line attributes are caught as well.
pub(crate) fn collect_dead_code_silencer_sites(
    source: &str,
) -> Result<Vec<DeadCodeSilencerSite>, String> {
    let file = syn::parse_file(source).map_err(|err| format!("parse Rust source: {err}"))?;
    let mut collector = DeadCodeSilencerCollector::new(source);
    collector.visit_file(&file);
    Ok(collector.sites)
}

pub(crate) fn load_dead_code_silencer_allowlist(
    repo_root: &Path,
) -> Result<BTreeSet<String>, String> {
    let path = repo_root.join("traceability/dead_code_silencer_allowlist.yaml");
    let text =
        fs::read_to_string(&path).map_err(|err| format!("read {}: {err}", path.display()))?;
    let entries: Vec<DeadCodeSilencerAllowlistEntry> =
        yaml_serde::from_str(&text).map_err(|err| format!("parse {}: {err}", path.display()))?;
    let mut allowed_sites = BTreeSet::new();
    for entry in entries {
        let site = entry.path.trim();
        if site.is_empty() {
            return Err(format!(
                "{} entry has empty `path`; each allowlist site must name `repo/file.rs:line`",
                path.display()
            ));
        }
        if entry.reason.trim().is_empty() {
            return Err(format!(
                "{} entry `{}` must include a non-empty `reason`",
                path.display(),
                site
            ));
        }
        let adr = entry.adr.trim();
        if adr.is_empty() {
            return Err(format!(
                "{} entry `{}` must include a non-empty `adr`",
                path.display(),
                site
            ));
        }
        if !adr_exists(repo_root, adr) {
            return Err(format!(
                "{} entry `{}` cites unknown ADR `{}`",
                path.display(),
                site,
                adr
            ));
        }
        let (rel_path, _line) = parse_allowlisted_site(site).ok_or_else(|| {
            format!(
                "{} entry `{}` must use `repo/file.rs:line` with a positive line number",
                path.display(),
                site
            )
        })?;
        let abs = resolve_repo_or_core_path(repo_root, Path::new(rel_path));
        if !abs.exists() {
            return Err(format!(
                "{} entry `{}` points at missing file `{}`",
                path.display(),
                site,
                rel_path
            ));
        }
        if !allowed_sites.insert(site.to_string()) {
            return Err(format!(
                "{} contains duplicate dead-code silencer allowlist site `{}`",
                path.display(),
                site
            ));
        }
    }
    Ok(allowed_sites)
}

fn parse_allowlisted_site(site: &str) -> Option<(&str, usize)> {
    let (path, line) = site.rsplit_once(':')?;
    let line = line.parse::<usize>().ok()?;
    if path.trim().is_empty() || line == 0 {
        return None;
    }
    Some((path, line))
}

fn adr_exists(repo_root: &Path, adr: &str) -> bool {
    let Some(digits) = adr.strip_prefix("ADR-") else {
        return false;
    };
    if digits.len() != 4 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let prefix = format!("ADR-{digits}");
    adr_file_with_prefix_exists(repo_root, &prefix)
}

struct DeadCodeSilencerCollector<'a> {
    lines: Vec<&'a str>,
    sites: Vec<DeadCodeSilencerSite>,
}

impl<'a> DeadCodeSilencerCollector<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            lines: source.lines().collect(),
            sites: Vec::new(),
        }
    }

    fn render_excerpt(&self, span: Span) -> String {
        let start = span.start();
        let end = span.end();
        if start.line == 0 || end.line == 0 {
            return "<attribute>".to_string();
        }
        let start_idx = start.line.saturating_sub(1);
        let end_idx = end.line.saturating_sub(1);
        if start_idx >= self.lines.len() || end_idx >= self.lines.len() || start_idx > end_idx {
            return "<attribute>".to_string();
        }
        self.lines[start_idx..=end_idx]
            .iter()
            .map(|line| line.trim())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl Visit<'_> for DeadCodeSilencerCollector<'_> {
    fn visit_attribute(&mut self, attribute: &syn::Attribute) {
        if meta_silences_dead_code(&attribute.meta) {
            let start = attribute.span().start();
            self.sites.push(DeadCodeSilencerSite {
                line: start.line,
                column: start.column + 1,
                rendered: self.render_excerpt(attribute.span()),
            });
        }
        syn::visit::visit_attribute(self, attribute);
    }
}

fn meta_silences_dead_code(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::List(list) if list.path.is_ident("allow") || list.path.is_ident("expect") => {
            parse_nested_meta_list(list)
                .map(|nested| nested.iter().any(lint_item_silences_dead_code))
                .unwrap_or(false)
        }
        syn::Meta::List(list) if list.path.is_ident("cfg_attr") => parse_nested_meta_list(list)
            .map(|nested| nested.iter().skip(1).any(meta_silences_dead_code))
            .unwrap_or(false),
        syn::Meta::Path(_) | syn::Meta::NameValue(_) | syn::Meta::List(_) => false,
    }
}

fn parse_nested_meta_list(list: &syn::MetaList) -> Option<Punctuated<syn::Meta, Token![,]>> {
    Punctuated::<syn::Meta, Token![,]>::parse_terminated
        .parse2(list.tokens.clone())
        .ok()
}

fn lint_item_silences_dead_code(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::Path(path) => path_silences_dead_code(path),
        syn::Meta::List(_) => meta_silences_dead_code(meta),
        syn::Meta::NameValue(value) => path_silences_dead_code(&value.path),
    }
}

fn path_silences_dead_code(path: &syn::Path) -> bool {
    path.is_ident("dead_code") || (path.is_ident("unused") && path.segments.len() == 1)
}

/// Walk a parsed Rust file and return true if any real path-position expression
/// or type references `name`. References inside comments and string literals
/// are ignored; only AST path positions count.
pub(crate) fn ast_references_name(file: &syn::File, name: &str) -> bool {
    struct Walker<'a> {
        needle: &'a str,
        found: bool,
    }
    impl Walker<'_> {
        fn path_matches(&self, path: &syn::Path) -> bool {
            path.segments
                .iter()
                .any(|segment| segment.ident == self.needle)
        }

        fn token_stream_mentions(&self, tokens: &proc_macro2::TokenStream) -> bool {
            tokens.clone().into_iter().any(|token| match token {
                proc_macro2::TokenTree::Ident(ident) => ident == self.needle,
                proc_macro2::TokenTree::Group(group) => self.token_stream_mentions(&group.stream()),
                proc_macro2::TokenTree::Punct(_) | proc_macro2::TokenTree::Literal(_) => false,
            })
        }
    }
    impl<'a, 'ast> Visit<'ast> for Walker<'a> {
        fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
            if self.found {
                return;
            }
            let meta_mentions = match &attr.meta {
                syn::Meta::Path(path) => self.path_matches(path),
                syn::Meta::List(list) => self.token_stream_mentions(&list.tokens),
                syn::Meta::NameValue(_) => false,
            };
            if self.path_matches(attr.path()) || meta_mentions {
                self.found = true;
                return;
            }
            syn::visit::visit_attribute(self, attr);
        }

        fn visit_path(&mut self, path: &'ast syn::Path) {
            if self.found {
                return;
            }
            if self.path_matches(path) {
                self.found = true;
                return;
            }
            syn::visit::visit_path(self, path);
        }

        fn visit_expr_struct(&mut self, expr: &'ast syn::ExprStruct) {
            if self.found {
                return;
            }
            if self.path_matches(&expr.path) {
                self.found = true;
                return;
            }
            syn::visit::visit_expr_struct(self, expr);
        }

        fn visit_expr_path(&mut self, expr: &'ast syn::ExprPath) {
            if self.found {
                return;
            }
            if self.path_matches(&expr.path) {
                self.found = true;
                return;
            }
            syn::visit::visit_expr_path(self, expr);
        }

        fn visit_item_use(&mut self, _node: &'ast syn::ItemUse) {
            // Import-only references do not prove behavioral coverage. The
            // caller wants an expression, type, pattern, method call, or macro
            // path that actually consumes the public item.
        }

        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            if self.found {
                return;
            }
            if call.method == self.needle {
                self.found = true;
                return;
            }
            syn::visit::visit_expr_method_call(self, call);
        }

        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            if self.found {
                return;
            }
            if self.path_matches(&mac.path) || self.token_stream_mentions(&mac.tokens) {
                self.found = true;
                return;
            }
            syn::visit::visit_macro(self, mac);
        }

        fn visit_field(&mut self, field: &'ast syn::Field) {
            if self.found {
                return;
            }
            syn::visit::visit_type(self, &field.ty);
        }
    }

    let mut walker = Walker {
        needle: name,
        found: false,
    };
    walker.visit_file(file);
    walker.found
}

pub(crate) fn public_item_names(file: &syn::File) -> BTreeSet<String> {
    let mut collector = PublicItemCollector::default();
    collector.visit_file(file);
    collector.names
}

#[derive(Default)]
struct PublicItemCollector {
    names: BTreeSet<String>,
}

impl PublicItemCollector {
    fn record_visibility(
        &mut self,
        vis: &syn::Visibility,
        attrs: &[syn::Attribute],
        name: impl Into<String>,
    ) {
        if matches!(vis, syn::Visibility::Public(_)) && !has_doc_hidden(attrs) {
            self.names.insert(name.into());
        }
    }

    fn record_use_tree(&mut self, tree: &syn::UseTree) {
        match tree {
            syn::UseTree::Name(name) => {
                self.names.insert(name.ident.to_string());
            }
            syn::UseTree::Rename(rename) => {
                self.names.insert(rename.rename.to_string());
            }
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    self.record_use_tree(item);
                }
            }
            syn::UseTree::Path(path) => self.record_use_tree(&path.tree),
            syn::UseTree::Glob(_) => {}
        }
    }
}

impl Visit<'_> for PublicItemCollector {
    fn visit_item_fn(&mut self, node: &syn::ItemFn) {
        self.record_visibility(&node.vis, &node.attrs, node.sig.ident.to_string());
        syn::visit::visit_item_fn(self, node);
    }

    fn visit_item_struct(&mut self, node: &syn::ItemStruct) {
        self.record_visibility(&node.vis, &node.attrs, node.ident.to_string());
        syn::visit::visit_item_struct(self, node);
    }

    fn visit_item_enum(&mut self, node: &syn::ItemEnum) {
        self.record_visibility(&node.vis, &node.attrs, node.ident.to_string());
        syn::visit::visit_item_enum(self, node);
    }

    fn visit_item_trait(&mut self, node: &syn::ItemTrait) {
        self.record_visibility(&node.vis, &node.attrs, node.ident.to_string());
        syn::visit::visit_item_trait(self, node);
    }

    fn visit_item_type(&mut self, node: &syn::ItemType) {
        self.record_visibility(&node.vis, &node.attrs, node.ident.to_string());
        syn::visit::visit_item_type(self, node);
    }

    fn visit_item_const(&mut self, node: &syn::ItemConst) {
        self.record_visibility(&node.vis, &node.attrs, node.ident.to_string());
        syn::visit::visit_item_const(self, node);
    }

    fn visit_item_mod(&mut self, node: &syn::ItemMod) {
        // Public modules are namespace/ownership containers. Their exported
        // functions, types, constants, traits, and explicit `pub use` symbols
        // carry the behavioral surface and are checked directly; requiring a
        // test to name every namespace would turn this detector into import
        // style enforcement instead of orphan-infrastructure detection.
        syn::visit::visit_item_mod(self, node);
    }

    fn visit_item_use(&mut self, node: &syn::ItemUse) {
        if matches!(node.vis, syn::Visibility::Public(_)) && !has_doc_hidden(&node.attrs) {
            self.record_use_tree(&node.tree);
        }
        syn::visit::visit_item_use(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &syn::ImplItemFn) {
        self.record_visibility(&node.vis, &node.attrs, node.sig.ident.to_string());
        syn::visit::visit_impl_item_fn(self, node);
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
    use super::collect_dead_code_silencer_sites;

    #[test]
    fn banned_forms_are_caught_even_when_wrapped_across_lines() {
        let source = r#"
#![allow(dead_code)]
#[expect(dead_code)]
#[allow(dead_code, unused_imports)]
#[allow(unused)]
#[cfg_attr(not(test), allow(dead_code))]
#[cfg_attr(
    all(not(test), feature = "bench"),
    expect(unused)
)]
fn example() {}
"#;
        let sites = collect_dead_code_silencer_sites(source).expect("parse banned forms");
        let rendered = sites
            .iter()
            .map(|site| site.rendered.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            sites.len(),
            6,
            "every banned attribute shape must be caught"
        );
        assert!(
            rendered
                .iter()
                .any(|attr| attr.contains("#![allow(dead_code)]")),
            "crate-inner dead_code allow must be reported"
        );
        assert!(
            rendered
                .iter()
                .any(|attr| attr.contains("#[allow(unused)]")),
            "unused lint group must be treated as a dead_code silencer"
        );
        assert!(
            rendered
                .iter()
                .any(|attr| attr.contains("#[cfg_attr(") && attr.contains("expect(unused)")),
            "multi-line cfg_attr wrappers must be caught by the AST walker"
        );
    }

    #[test]
    fn sibling_unused_lints_pass_unharmed() {
        let source = r#"
#[allow(unused_imports)]
#[allow(unused_variables)]
#[expect(unused_mut)]
#[allow(unused_must_use)]
#[cfg_attr(not(test), allow(unused_imports))]
#[allow(clippy::unwrap_used)]
#[expect(clippy::panic)]
#[cfg_attr(not(test), deny(clippy::expect_used))]
fn example() {}
"#;
        let sites = collect_dead_code_silencer_sites(source).expect("parse allowed forms");
        assert!(
            sites.is_empty(),
            "sibling unused_* lints and non-dead_code attributes must stay allowed"
        );
    }

    #[test]
    fn ast_reference_detection_ignores_bare_imports_but_accepts_type_use() {
        let import_only = syn::parse_file(
            r#"
use batpak::ImportantType;

fn unrelated() {}
"#,
        )
        .expect("parse import-only fixture");
        assert!(
            !super::ast_references_name(&import_only, "ImportantType"),
            "bare use trees must not satisfy public-item coverage"
        );

        let typed_use = syn::parse_file(
            r#"
use batpak::ImportantType;

fn takes_value(value: ImportantType) {
    let _ = value;
}
"#,
        )
        .expect("parse type-use fixture");
        assert!(
            super::ast_references_name(&typed_use, "ImportantType"),
            "type positions still count as real public-item uses"
        );

        let struct_literal = syn::parse_file(
            r#"
use batpak::ImportantType;

fn constructs() {
    let _ = ImportantType { value: 1 };
}
"#,
        )
        .expect("parse struct-literal fixture");
        assert!(
            super::ast_references_name(&struct_literal, "ImportantType"),
            "constructor and struct-literal positions are real public-item uses"
        );

        let bare_function_call = syn::parse_file(
            r#"
use batpak::important_function;

fn calls() {
    important_function();
}
"#,
        )
        .expect("parse bare function-call fixture");
        assert!(
            super::ast_references_name(&bare_function_call, "important_function"),
            "bare function calls imported into scope are real public-item uses"
        );

        let macro_body_reference = syn::parse_file(
            r#"
use batpak::ImportantType;

fn checks(value: ImportantType) {
    assert!(matches!(value, ImportantType::Ready));
}
"#,
        )
        .expect("parse macro-body fixture");
        assert!(
            super::ast_references_name(&macro_body_reference, "Ready"),
            "macro token bodies are real Rust positions, while string literals remain ignored"
        );

        let derive_attribute_reference = syn::parse_file(
            r#"
#[derive(Debug, ImportantDerive)]
struct UsesDerive;
"#,
        )
        .expect("parse derive-attribute fixture");
        assert!(
            super::ast_references_name(&derive_attribute_reference, "ImportantDerive"),
            "derive macro attributes are real public-item witnesses"
        );

        let config_propagation = syn::parse_file(
            r#"
fn fixture() {
    let _key = ClockKey {
        wall_ms: 1,
        clock: 2,
        uuid: 3,
    };
}
"#,
        )
        .expect("parse config_propagation fixture");
        assert!(
            super::ast_references_name(&config_propagation, "ClockKey"),
            "pub_item_allowlist witnesses may use struct-literal construction as behavioral coverage"
        );
    }
}
