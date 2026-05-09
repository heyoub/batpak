use super::{ensure, relative};
use anyhow::{Context, Result};
use quote::ToTokens;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::{self, Visit};
use syn::{Attribute, Expr, ExprCall, ExprMethodCall, Item, UseTree};

pub(super) fn check(repo_root: &Path, tracked_files: &[PathBuf]) -> Result<()> {
    let banned_calls = [
        ("File::create_new", "exclusive file creation"),
        (".sync_all(", "file or directory sync_all"),
        (".sync_data(", "file sync_data"),
        ("memmap2::Mmap::map", "direct mmap mapping"),
        ("Mmap::map", "direct mmap mapping"),
        ("MmapOptions::map", "direct mmap options mapping"),
        (".custom_flags(", "platform open flags"),
        ("libc::O_NOFOLLOW", "Unix symlink-leaf open policy"),
        ("std::os::unix::fs::FileExt", "Unix positional read import"),
        (".read_at(", "Unix positional read call"),
        ("cfg!(unix", "runtime target cfg"),
        ("cfg!(windows", "runtime target cfg"),
        ("cfg!(target_family", "runtime target cfg"),
    ];
    let banned_target_cfgs = [
        "#[cfg(unix)]",
        "#[cfg(windows)]",
        "#[cfg(not(unix))]",
        "target_family = \"unix\"",
        "target_family = \"windows\"",
        "target_os = \"windows\"",
        "target_os = \"linux\"",
        "target_os = \"macos\"",
    ];

    for path in tracked_files {
        let rel = relative(repo_root, path);
        if !rel.starts_with("src/store/")
            || rel.starts_with("src/store/platform/")
            || path.extension().and_then(|ext| ext.to_str()) != Some("rs")
        {
            continue;
        }

        let content =
            fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        if let Ok(file) = syn::parse_file(&content) {
            for violation in target_cfg_attr_violations(&file) {
                ensure(
                    false,
                    format!(
                        "structural-check: target cfg `{violation}` in store runtime code must live under src/store/platform; found in {rel}"
                    ),
                )?;
            }
            for violation in store_platform_contact_syn_violations(&file) {
                ensure(
                    false,
                    format!(
                        "structural-check: target-sensitive {violation} must route through src/store/platform; found in {rel}"
                    ),
                )?;
            }
        }
        for (line_index, line) in content.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with("//!")
            {
                continue;
            }
            let line = strip_string_literals(line);
            for (needle, description) in banned_calls {
                ensure(
                    !line.contains(needle),
                    format!(
                        "structural-check: target-sensitive {description} must route through src/store/platform; found `{needle}` in {rel}:{}",
                        line_index + 1
                    ),
                )?;
            }
            for needle in banned_target_cfgs {
                ensure(
                    !line.contains(needle),
                    format!(
                        "structural-check: target cfg `{needle}` in store runtime code must live under src/store/platform; found in {rel}:{}",
                        line_index + 1
                    ),
                )?;
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct PlatformContactAliases {
    file: BTreeSet<String>,
    mmap: BTreeSet<String>,
    mmap_options: BTreeSet<String>,
}

fn store_platform_contact_syn_violations(file: &syn::File) -> Vec<String> {
    let aliases = collect_platform_contact_aliases(file);
    let mut visitor = PlatformContactVisitor {
        aliases,
        violations: Vec::new(),
    };
    visitor.visit_file(file);
    visitor.violations
}

fn collect_platform_contact_aliases(file: &syn::File) -> PlatformContactAliases {
    let mut aliases = PlatformContactAliases::default();
    aliases.file.insert("File".to_owned());
    aliases.mmap.insert("Mmap".to_owned());
    aliases.mmap_options.insert("MmapOptions".to_owned());
    for item in &file.items {
        let Item::Use(item) = item else {
            continue;
        };
        collect_use_tree_aliases(&item.tree, &mut Vec::new(), &mut aliases);
    }
    aliases
}

fn collect_use_tree_aliases(
    tree: &UseTree,
    prefix: &mut Vec<String>,
    aliases: &mut PlatformContactAliases,
) {
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            collect_use_tree_aliases(&path.tree, prefix, aliases);
            let _ = prefix.pop();
        }
        UseTree::Name(name) => {
            let mut full = prefix.clone();
            full.push(name.ident.to_string());
            record_platform_contact_alias(&full, name.ident.to_string(), aliases);
        }
        UseTree::Rename(rename) => {
            let mut full = prefix.clone();
            full.push(rename.ident.to_string());
            record_platform_contact_alias(&full, rename.rename.to_string(), aliases);
        }
        UseTree::Group(group) => {
            for tree in &group.items {
                collect_use_tree_aliases(tree, prefix, aliases);
            }
        }
        UseTree::Glob(_) => {}
    }
}

fn record_platform_contact_alias(
    full_path: &[String],
    alias: String,
    aliases: &mut PlatformContactAliases,
) {
    if path_ends_with(full_path, &["std", "fs", "File"]) {
        aliases.file.insert(alias);
    } else if path_ends_with(full_path, &["memmap2", "Mmap"]) {
        aliases.mmap.insert(alias);
    } else if path_ends_with(full_path, &["memmap2", "MmapOptions"]) {
        aliases.mmap_options.insert(alias);
    }
}

fn path_ends_with(path: &[String], suffix: &[&str]) -> bool {
    path.len() >= suffix.len()
        && path[path.len() - suffix.len()..]
            .iter()
            .map(String::as_str)
            .eq(suffix.iter().copied())
}

struct PlatformContactVisitor {
    aliases: PlatformContactAliases,
    violations: Vec<String>,
}

impl PlatformContactVisitor {
    fn call_path_violation(&self, path: &syn::Path) -> Option<&'static str> {
        let segments = path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>();
        let (owner, method) = match segments.as_slice() {
            [owner, method] => (owner.as_str(), method.as_str()),
            [.., owner, method] => (owner.as_str(), method.as_str()),
            _ => return None,
        };
        if method == "create_new"
            && (self.aliases.file.contains(owner)
                || path_ends_with(&segments, &["std", "fs", "File", "create_new"]))
        {
            return Some("exclusive file creation");
        }
        if method == "map"
            && (self.aliases.mmap.contains(owner)
                || path_ends_with(&segments, &["memmap2", "Mmap", "map"]))
        {
            return Some("direct mmap mapping");
        }
        if method == "map"
            && (self.aliases.mmap_options.contains(owner)
                || path_ends_with(&segments, &["memmap2", "MmapOptions", "map"]))
        {
            return Some("direct mmap options mapping");
        }
        None
    }

    fn receiver_is_mmap_options_new(&self, receiver: &Expr) -> bool {
        let Expr::Call(ExprCall { func, .. }) = receiver else {
            return false;
        };
        let Expr::Path(path) = func.as_ref() else {
            return false;
        };
        let segments = path
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>();
        let (owner, method) = match segments.as_slice() {
            [owner, method] => (owner.as_str(), method.as_str()),
            [.., owner, method] => (owner.as_str(), method.as_str()),
            _ => return false,
        };
        method == "new"
            && (self.aliases.mmap_options.contains(owner)
                || path_ends_with(&segments, &["memmap2", "MmapOptions", "new"]))
    }
}

impl<'ast> Visit<'ast> for PlatformContactVisitor {
    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let Expr::Path(path) = node.func.as_ref() {
            if let Some(violation) = self.call_path_violation(&path.path) {
                self.violations.push(violation.to_owned());
            }
        }
        visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        if node.method == "map" && self.receiver_is_mmap_options_new(&node.receiver) {
            self.violations
                .push("direct mmap options mapping".to_owned());
        }
        visit::visit_expr_method_call(self, node);
    }
}

fn target_cfg_attr_violations(file: &syn::File) -> Vec<String> {
    fn collect_attrs(attrs: &[Attribute], violations: &mut Vec<String>) {
        for attr in attrs {
            if !attr.path().is_ident("cfg") {
                continue;
            }
            let rendered = attr.meta.to_token_stream().to_string();
            if rendered.contains("unix")
                || rendered.contains("windows")
                || rendered.contains("target_family")
                || rendered.contains("target_os")
            {
                violations.push(rendered);
            }
        }
    }

    let mut violations = Vec::new();
    for item in &file.items {
        match item {
            Item::Fn(item) => collect_attrs(&item.attrs, &mut violations),
            Item::Mod(item) => collect_attrs(&item.attrs, &mut violations),
            Item::Struct(item) => collect_attrs(&item.attrs, &mut violations),
            Item::Enum(item) => collect_attrs(&item.attrs, &mut violations),
            Item::Impl(item) => collect_attrs(&item.attrs, &mut violations),
            Item::Use(item) => collect_attrs(&item.attrs, &mut violations),
            Item::Const(item) => collect_attrs(&item.attrs, &mut violations),
            Item::Type(item) => collect_attrs(&item.attrs, &mut violations),
            _ => {}
        }
    }
    violations
}

fn strip_string_literals(line: &str) -> String {
    let mut output = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    let mut in_string = false;
    while let Some(ch) = chars.next() {
        if in_string {
            if ch == '\\' {
                let _ = chars.next();
                output.push(' ');
                continue;
            }
            if ch == '"' {
                in_string = false;
            }
            output.push(' ');
        } else if ch == '"' {
            in_string = true;
            output.push(' ');
        } else {
            output.push(ch);
        }
    }
    output
}
