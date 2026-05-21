use super::{ensure, relative};
use anyhow::{Context, Result};
use quote::ToTokens;
use std::collections::{BTreeMap, BTreeSet};
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
        if !(rel.starts_with("crates/core/src/store/") || rel.starts_with("src/store/"))
            || rel.starts_with("crates/core/src/store/platform/")
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
    check_direct_fs_contact_ratchet(repo_root, tracked_files)?;
    Ok(())
}

fn check_direct_fs_contact_ratchet(repo_root: &Path, tracked_files: &[PathBuf]) -> Result<()> {
    const DIRECT_FS_NEEDLES: &[&str] = &[
        "std::fs::read_dir",
        "std::fs::metadata",
        "std::fs::read(",
        "std::fs::write(",
        "std::fs::remove_file",
        "std::fs::remove_dir_all",
        "std::fs::rename",
        "std::fs::copy",
        "std::fs::create_dir_all",
        "std::fs::canonicalize",
        "std::fs::File::open",
        "File::open",
        "NamedTempFile::new_in",
    ];
    const ALLOWED_DIRECT_FS_CONTACTS: &[(&str, &str, usize)] = &[
        (
            "crates/core/src/store/cold_start/checkpoint/format.rs",
            "std::fs::read(",
            1,
        ),
        (
            "crates/core/src/store/cold_start/checkpoint/test_support.rs",
            "std::fs::write(",
            2,
        ),
        (
            "crates/core/src/store/cold_start/checkpoint/tests.rs",
            "std::fs::read(",
            3,
        ),
        (
            "crates/core/src/store/cold_start/checkpoint/tests.rs",
            "std::fs::write(",
            5,
        ),
        ("crates/core/src/store/cold_start/mmap.rs", "File::open", 1),
        (
            "crates/core/src/store/cold_start/mmap.rs",
            "std::fs::write(",
            2,
        ),
        (
            "crates/core/src/store/cold_start/mod.rs",
            "std::fs::metadata",
            2,
        ),
        (
            "crates/core/src/store/cold_start/mod.rs",
            "std::fs::read_dir",
            1,
        ),
        (
            "crates/core/src/store/cold_start/rebuild.rs",
            "std::fs::write(",
            8,
        ),
        (
            "crates/core/src/store/cold_start/rebuild/topology.rs",
            "std::fs::read_dir",
            1,
        ),
        (
            "crates/core/src/store/cold_start/rebuild/topology.rs",
            "std::fs::read(",
            1,
        ),
        (
            "crates/core/src/store/cold_start/rebuild/topology.rs",
            "std::fs::remove_file",
            1,
        ),
        (
            "crates/core/src/store/compaction_report.rs",
            "std::fs::read(",
            1,
        ),
        (
            "crates/core/src/store/delivery/cursor/checkpoint.rs",
            "NamedTempFile::new_in",
            1,
        ),
        (
            "crates/core/src/store/delivery/cursor/checkpoint.rs",
            "std::fs::create_dir_all",
            1,
        ),
        (
            "crates/core/src/store/delivery/cursor/checkpoint.rs",
            "std::fs::read(",
            1,
        ),
        (
            "crates/core/src/store/dir_lock.rs",
            "std::fs::canonicalize",
            1,
        ),
        (
            "crates/core/src/store/hidden_ranges.rs",
            "NamedTempFile::new_in",
            1,
        ),
        (
            "crates/core/src/store/hidden_ranges.rs",
            "std::fs::read(",
            1,
        ),
        (
            "crates/core/src/store/hidden_ranges.rs",
            "std::fs::remove_file",
            1,
        ),
        ("crates/core/src/store/lifecycle.rs", "std::fs::copy", 1),
        (
            "crates/core/src/store/lifecycle.rs",
            "std::fs::create_dir_all",
            1,
        ),
        ("crates/core/src/store/lifecycle.rs", "std::fs::metadata", 1),
        ("crates/core/src/store/lifecycle.rs", "std::fs::read_dir", 3),
        (
            "crates/core/src/store/lifecycle.rs",
            "std::fs::remove_dir_all",
            1,
        ),
        (
            "crates/core/src/store/lifecycle.rs",
            "std::fs::remove_file",
            6,
        ),
        ("crates/core/src/store/lifecycle.rs", "std::fs::rename", 2),
        ("crates/core/src/store/open.rs", "std::fs::canonicalize", 1),
        (
            "crates/core/src/store/open.rs",
            "std::fs::create_dir_all",
            1,
        ),
        ("crates/core/src/store/open.rs", "std::fs::write(", 2),
        (
            "crates/core/src/store/projection/mod.rs",
            "std::fs::create_dir_all",
            2,
        ),
        (
            "crates/core/src/store/projection/mod.rs",
            "std::fs::metadata",
            1,
        ),
        (
            "crates/core/src/store/projection/mod.rs",
            "std::fs::read_dir",
            2,
        ),
        (
            "crates/core/src/store/projection/mod.rs",
            "std::fs::read(",
            1,
        ),
        (
            "crates/core/src/store/projection/mod.rs",
            "std::fs::remove_file",
            2,
        ),
        (
            "crates/core/src/store/segment/mod.rs",
            "std::fs::File::open",
            1,
        ),
        (
            "crates/core/src/store/segment/scan/full_scan.rs",
            "File::open",
            1,
        ),
        ("crates/core/src/store/segment/scan/mod.rs", "File::open", 1),
        (
            "crates/core/src/store/segment/scan/mod.rs",
            "std::fs::write(",
            1,
        ),
        (
            "crates/core/src/store/segment/scan/point_read.rs",
            "File::open",
            1,
        ),
        (
            "crates/core/src/store/segment/scan/recovery.rs",
            "File::open",
            1,
        ),
        (
            "crates/core/src/store/segment/scan/recovery.rs",
            "std::fs::write(",
            1,
        ),
        (
            "crates/core/src/store/segment/scan/recovery/sidx_fast_path.rs",
            "std::fs::File::open",
            1,
        ),
        (
            "crates/core/src/store/segment/scan/recovery/sidx_fast_path.rs",
            "std::fs::metadata",
            1,
        ),
        (
            "crates/core/src/store/segment/sidx.rs",
            "std::fs::File::open",
            1,
        ),
        (
            "crates/core/src/store/store_resource_report.rs",
            "std::fs::canonicalize",
            2,
        ),
        (
            "crates/core/src/store/write/writer.rs",
            "std::fs::create_dir_all",
            1,
        ),
        (
            "crates/core/src/store/write/writer/runtime.rs",
            "std::fs::read_dir",
            1,
        ),
    ];

    let mut observed: BTreeMap<(String, &'static str), usize> = BTreeMap::new();
    for path in tracked_files {
        let rel = relative(repo_root, path);
        if !(rel.starts_with("crates/core/src/store/") || rel.starts_with("src/store/"))
            || rel.starts_with("crates/core/src/store/platform/")
            || rel.starts_with("src/store/platform/")
            || path.extension().and_then(|ext| ext.to_str()) != Some("rs")
        {
            continue;
        }
        let content =
            fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        for line in content.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with("//!")
            {
                continue;
            }
            let stripped = strip_string_literals(line);
            for needle in DIRECT_FS_NEEDLES {
                if stripped.contains(needle) {
                    *observed.entry((rel.clone(), *needle)).or_default() += 1;
                    break;
                }
            }
        }
    }

    for ((rel, needle), count) in observed {
        let allowed = ALLOWED_DIRECT_FS_CONTACTS
            .iter()
            .find_map(|(allowed_rel, allowed_needle, allowed_count)| {
                (*allowed_rel == rel && *allowed_needle == needle).then_some(*allowed_count)
            })
            .unwrap_or(0);
        ensure(
            count <= allowed,
            format!(
                "structural-check: direct filesystem contact ratchet exceeded in {rel}: `{needle}` observed {count}, allowed {allowed}. Route new store machine-contact through src/store/platform or deliberately shrink/adjust the ratchet."
            ),
        )?;
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
        if let Item::Fn(item) = item {
            collect_attrs(&item.attrs, &mut violations);
        } else if let Item::Mod(item) = item {
            collect_attrs(&item.attrs, &mut violations);
        } else if let Item::Struct(item) = item {
            collect_attrs(&item.attrs, &mut violations);
        } else if let Item::Enum(item) = item {
            collect_attrs(&item.attrs, &mut violations);
        } else if let Item::Impl(item) = item {
            collect_attrs(&item.attrs, &mut violations);
        } else if let Item::Use(item) = item {
            collect_attrs(&item.attrs, &mut violations);
        } else if let Item::Const(item) = item {
            collect_attrs(&item.attrs, &mut violations);
        } else if let Item::Type(item) = item {
            collect_attrs(&item.attrs, &mut violations);
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
