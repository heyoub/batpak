use super::ensure;
use crate::repo_surface::core_path;
use crate::shared_checks::public_item_names;
use crate::source_cache::SourceCache;
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::Path;
use syn::{Item, UseTree};

pub(super) fn check(repo_root: &Path, source_cache: &mut SourceCache) -> Result<()> {
    let store_mod = parse_public_api_rust(source_cache, &core_path(repo_root, "src/store/mod.rs"))?;
    let prelude = parse_public_api_rust(source_cache, &core_path(repo_root, "src/prelude.rs"))?;
    let event_mod = parse_public_api_rust(source_cache, &core_path(repo_root, "src/event/mod.rs"))?;
    let event_sourcing =
        parse_public_api_rust(source_cache, &core_path(repo_root, "src/event/sourcing.rs"))?;
    let config = parse_public_api_rust(source_cache, &core_path(repo_root, "src/store/config.rs"))?;
    let config_types = parse_public_api_rust(
        source_cache,
        &core_path(repo_root, "src/store/config/types.rs"),
    )?;

    let store_exports = public_use_names(&store_mod);
    ensure(
        store_exports.contains("IndexTopology"),
        "src/store/mod.rs must re-export IndexTopology",
    )?;
    ensure(
        !store_exports.contains("IndexLayout"),
        "src/store/mod.rs still re-exports removed IndexLayout",
    )?;
    ensure(
        !store_exports.contains("ViewConfig"),
        "src/store/mod.rs still re-exports removed ViewConfig",
    )?;

    let prelude_exports = public_use_names(&prelude);
    for required in ["IndexTopology", "ReplayLane", "JsonValueInput"] {
        ensure(
            prelude_exports.contains(required),
            format!("src/prelude.rs must re-export {required}"),
        )?;
    }
    for banned in ["IndexLayout", "ViewConfig", "ProjectionMode", "ValueInput"] {
        ensure(
            !prelude_exports.contains(banned),
            format!("src/prelude.rs still exposes removed public name {banned}"),
        )?;
    }

    let event_exports = public_use_names(&event_mod);
    for required in ["ReplayLane", "JsonValueInput", "RawMsgpackInput"] {
        ensure(
            event_exports.contains(required),
            format!("src/event/mod.rs must re-export {required}"),
        )?;
    }
    for banned in ["ProjectionMode", "ValueInput"] {
        ensure(
            !event_exports.contains(banned),
            format!("src/event/mod.rs still exposes removed replay name {banned}"),
        )?;
    }

    let mut config_items = public_item_names(&config);
    config_items.extend(public_item_names(&config_types));
    ensure(
        config_items.contains("IndexTopology"),
        "store config surface must define or re-export IndexTopology as a live public type",
    )?;
    for banned in ["IndexLayout", "ViewConfig"] {
        ensure(
            !config_items.contains(banned),
            format!("store config surface still defines removed topology name {banned}"),
        )?;
    }

    let config_methods = public_impl_method_names(&config, "StoreConfig");
    ensure(
        config_methods.contains("with_index_topology"),
        "StoreConfig must expose with_index_topology",
    )?;
    for banned in ["with_index_layout", "with_views"] {
        ensure(
            !config_methods.contains(banned),
            format!("StoreConfig still exposes removed builder {banned}"),
        )?;
    }

    let replay_items = public_item_names(&event_sourcing);
    for required in ["ReplayLane", "JsonValueInput", "RawMsgpackInput"] {
        ensure(
            replay_items.contains(required),
            format!("src/event/sourcing.rs must define {required}"),
        )?;
    }
    for banned in ["ProjectionMode", "ValueInput"] {
        ensure(
            !replay_items.contains(banned),
            format!("src/event/sourcing.rs still defines removed replay type {banned}"),
        )?;
    }

    let topology_constructors = public_impl_method_names(&config_types, "IndexTopology");
    for required in [
        "aos",
        "scan",
        "entity_local",
        "tiled",
        "all",
        "with_soa",
        "with_entity_groups",
        "with_tiles64",
    ] {
        ensure(
            topology_constructors.contains(required),
            format!("IndexTopology must expose builder/constructor `{required}`"),
        )?;
    }
    let topology_public_fields = public_struct_field_names(&config_types, "IndexTopology");
    ensure(
        topology_public_fields.is_empty(),
        format!(
            "IndexTopology fields must stay private; found public fields {:?}",
            topology_public_fields
        ),
    )?;
    let topology_default = default_impl_return_target(&config_types, "IndexTopology");
    ensure(
        matches!(
            topology_default.as_deref(),
            Some("Self::aos") | Some("IndexTopology::aos")
        ),
        "IndexTopology::default() must delegate to aos() so overlays stay opt-in",
    )?;

    let replay_variants = public_enum_variant_names(&event_sourcing, "ReplayLane");
    ensure(
        replay_variants == BTreeSet::from(["RawMsgpack".to_string(), "Value".to_string()]),
        format!(
            "ReplayLane must contain exactly {{Value, RawMsgpack}}, found {:?}",
            replay_variants
        ),
    )?;
    ensure(
        impl_const_expr_target(&event_sourcing, "JsonValueInput", "MODE").as_deref()
            == Some("ReplayLane::Value"),
        "JsonValueInput::MODE must map to ReplayLane::Value",
    )?;
    ensure(
        impl_const_expr_target(&event_sourcing, "RawMsgpackInput", "MODE").as_deref()
            == Some("ReplayLane::RawMsgpack"),
        "RawMsgpackInput::MODE must map to ReplayLane::RawMsgpack",
    )?;

    Ok(())
}

fn parse_public_api_rust(
    source_cache: &mut SourceCache,
    path: &Path,
) -> Result<std::sync::Arc<syn::File>> {
    source_cache
        .parse_rust(path)
        .with_context(|| format!("parse {}", path.display()))
}

fn public_impl_method_names(file: &syn::File, type_name: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for item in &file.items {
        let Item::Impl(impl_block) = item else {
            continue;
        };
        if impl_block.trait_.is_some() {
            continue;
        }
        let is_target_impl = if let syn::Type::Path(tp) = impl_block.self_ty.as_ref() {
            tp.path
                .segments
                .last()
                .map(|segment| segment.ident == type_name)
                .unwrap_or(false)
        } else {
            false
        };
        if !is_target_impl {
            continue;
        }
        for impl_item in &impl_block.items {
            if let syn::ImplItem::Fn(method) = impl_item {
                if matches!(method.vis, syn::Visibility::Public(_)) {
                    names.insert(method.sig.ident.to_string());
                }
            }
        }
    }
    names
}

fn public_struct_field_names(file: &syn::File, struct_name: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for item in &file.items {
        let Item::Struct(item_struct) = item else {
            continue;
        };
        if item_struct.ident != struct_name {
            continue;
        }
        if !matches!(item_struct.vis, syn::Visibility::Public(_)) {
            continue;
        }
        if let syn::Fields::Named(fields) = &item_struct.fields {
            for field in &fields.named {
                if matches!(field.vis, syn::Visibility::Public(_)) {
                    if let Some(ident) = &field.ident {
                        names.insert(ident.to_string());
                    }
                }
            }
        }
    }
    names
}

fn public_enum_variant_names(file: &syn::File, enum_name: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for item in &file.items {
        let Item::Enum(item_enum) = item else {
            continue;
        };
        if item_enum.ident != enum_name || !matches!(item_enum.vis, syn::Visibility::Public(_)) {
            continue;
        }
        for variant in &item_enum.variants {
            names.insert(variant.ident.to_string());
        }
    }
    names
}

fn default_impl_return_target(file: &syn::File, type_name: &str) -> Option<String> {
    for item in &file.items {
        let Item::Impl(impl_block) = item else {
            continue;
        };
        let Some((_, trait_path, _)) = &impl_block.trait_ else {
            continue;
        };
        let is_default_impl = trait_path
            .segments
            .last()
            .map(|segment| segment.ident == "Default")
            .unwrap_or(false);
        if !is_default_impl || !self_ty_is(impl_block.self_ty.as_ref(), type_name) {
            continue;
        }
        for impl_item in &impl_block.items {
            let syn::ImplItem::Fn(method) = impl_item else {
                continue;
            };
            if method.sig.ident != "default" {
                continue;
            }
            if let Some(target) = trailing_expr_target(&method.block) {
                return Some(target);
            }
        }
    }
    None
}

fn impl_const_expr_target(file: &syn::File, type_name: &str, const_name: &str) -> Option<String> {
    for item in &file.items {
        let Item::Impl(impl_block) = item else {
            continue;
        };
        if impl_block.trait_.is_none() || !self_ty_is(impl_block.self_ty.as_ref(), type_name) {
            continue;
        }
        for impl_item in &impl_block.items {
            let syn::ImplItem::Const(item_const) = impl_item else {
                continue;
            };
            if item_const.ident == const_name {
                return expr_target(&item_const.expr);
            }
        }
    }
    None
}

fn self_ty_is(ty: &syn::Type, type_name: &str) -> bool {
    if let syn::Type::Path(tp) = ty {
        tp.path
            .segments
            .last()
            .map(|segment| segment.ident == type_name)
            .unwrap_or(false)
    } else {
        false
    }
}

fn trailing_expr_target(block: &syn::Block) -> Option<String> {
    let stmt = block.stmts.last()?;
    if let syn::Stmt::Expr(expr, _) = stmt {
        expr_target(expr)
    } else {
        None
    }
}

fn expr_target(expr: &syn::Expr) -> Option<String> {
    if let syn::Expr::Call(call) = expr {
        expr_target(&call.func)
    } else if let syn::Expr::Path(path) = expr {
        Some(path_to_string(&path.path))
    } else {
        None
    }
}

fn path_to_string(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

fn public_use_names(file: &syn::File) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for item in &file.items {
        if let Item::Use(item_use) = item {
            if matches!(item_use.vis, syn::Visibility::Public(_)) {
                collect_use_tree_names(&item_use.tree, &mut names);
            }
        }
    }
    names
}

fn collect_use_tree_names(tree: &UseTree, names: &mut BTreeSet<String>) {
    match tree {
        UseTree::Path(path) => collect_use_tree_names(&path.tree, names),
        UseTree::Name(name) => {
            names.insert(name.ident.to_string());
        }
        UseTree::Rename(rename) => {
            names.insert(rename.rename.to_string());
        }
        UseTree::Group(group) => {
            for item in &group.items {
                collect_use_tree_names(item, names);
            }
        }
        UseTree::Glob(_) => {}
    }
}
