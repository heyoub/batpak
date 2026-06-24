use super::{ensure, relative};
use crate::source_cache::SourceCache;
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

struct BoundaryTerm {
    token: &'static str,
    reason: &'static str,
}

struct InternalPathTerm {
    module: &'static str,
    reason: &'static str,
}

const CORE_LAYER_LEAKS: &[BoundaryTerm] = &[
    BoundaryTerm {
        token: "syncbat",
        reason: "runtime layer name belongs outside batpak core",
    },
    BoundaryTerm {
        token: "Syncbat",
        reason: "runtime layer type names belong outside batpak core",
    },
    BoundaryTerm {
        token: "downstream-kit",
        reason: "contract layer names belong outside batpak core",
    },
    BoundaryTerm {
        token: "DownstreamKit",
        reason: "contract layer type names belong outside batpak core",
    },
    BoundaryTerm {
        token: "netbat",
        reason: "network layer names belong outside batpak core",
    },
    BoundaryTerm {
        token: "Netbat",
        reason: "network layer type names belong outside batpak core",
    },
    BoundaryTerm {
        token: "bvisor",
        reason: "boundary-supervisor layer names belong outside batpak core",
    },
    BoundaryTerm {
        token: "Bvisor",
        reason: "boundary-supervisor layer type names belong outside batpak core",
    },
    BoundaryTerm {
        token: "contract.external_v1",
        reason: "ExtProfile profile wire validation belongs outside batpak core",
    },
    BoundaryTerm {
        token: "authority_required",
        reason: "authority claims are caller policy input, not substrate law",
    },
    BoundaryTerm {
        token: "External-Profile",
        reason: "ExtProfile semantics stay outside batpak core",
    },
    BoundaryTerm {
        token: "ExternalProfile",
        reason: "ExtProfile profile types stay outside batpak core",
    },
];

const SYNCBAT_LAYER_LEAKS: &[BoundaryTerm] = &[
    BoundaryTerm {
        token: "downstream-kit",
        reason: "contract layer names belong outside syncbat",
    },
    BoundaryTerm {
        token: "DownstreamKit",
        reason: "contract layer type names belong outside syncbat",
    },
    BoundaryTerm {
        token: "netbat",
        reason: "network layer names belong outside syncbat",
    },
    BoundaryTerm {
        token: "Netbat",
        reason: "network layer type names belong outside syncbat",
    },
    BoundaryTerm {
        token: "bvisor",
        reason: "boundary-supervisor layer names belong outside syncbat",
    },
    BoundaryTerm {
        token: "Bvisor",
        reason: "boundary-supervisor layer type names belong outside syncbat",
    },
    BoundaryTerm {
        token: "contract.external_v1",
        reason: "ExtProfile profile wire validation belongs outside syncbat",
    },
    BoundaryTerm {
        token: "authority_required",
        reason: "authority claims are caller policy input, not syncbat law",
    },
    BoundaryTerm {
        token: "External-Profile",
        reason: "ExtProfile semantics stay outside syncbat",
    },
    BoundaryTerm {
        token: "ExternalProfile",
        reason: "ExtProfile profile types stay outside syncbat",
    },
];

const NETBAT_LAYER_LEAKS: &[BoundaryTerm] = &[
    BoundaryTerm {
        token: "bvisor",
        reason: "boundary-supervisor layer names belong outside netbat",
    },
    BoundaryTerm {
        token: "Bvisor",
        reason: "boundary-supervisor layer type names belong outside netbat",
    },
    BoundaryTerm {
        token: "contract.external_v1",
        reason: "ExtProfile profile wire validation belongs outside netbat",
    },
    BoundaryTerm {
        token: "authority_required",
        reason: "authority claims are caller policy input, not netbat law",
    },
    BoundaryTerm {
        token: "External-Profile",
        reason: "ExtProfile semantics stay outside netbat",
    },
    BoundaryTerm {
        token: "ExternalProfile",
        reason: "ExtProfile profile types stay outside netbat",
    },
    BoundaryTerm {
        token: "batpak::",
        reason: "netbat should expose syncbat, not bypass the runtime into batpak",
    },
];

// The PURE `bvisor` contract crate (crates/bvisor/src/ EXCEPT src/host/). It
// depends on the batpak generic substrate API ONLY: host wiring (syncbat),
// transport (netbat), and contract-layer names live in the feature-gated
// `bvisor::host` module (kernel plan §11 — there is NO `bvisor-host` crate),
// never in the pure `contract/` + `backend/`.
const BVISOR_LAYER_LEAKS: &[BoundaryTerm] = &[
    BoundaryTerm {
        token: "syncbat",
        reason: "runtime host wiring belongs in the bvisor::host feature module, not the pure bvisor contract",
    },
    BoundaryTerm {
        token: "Syncbat",
        reason: "runtime host wiring belongs in the bvisor::host feature module, not the pure bvisor contract",
    },
    BoundaryTerm {
        token: "netbat",
        reason: "transport belongs in the bvisor::host feature module, not the pure bvisor contract",
    },
    BoundaryTerm {
        token: "Netbat",
        reason: "transport belongs in the bvisor::host feature module, not the pure bvisor contract",
    },
    BoundaryTerm {
        token: "downstream-kit",
        reason: "contract layer names belong outside bvisor",
    },
    BoundaryTerm {
        token: "DownstreamKit",
        reason: "contract layer type names belong outside bvisor",
    },
    BoundaryTerm {
        token: "authority_required",
        reason:
            "authority claims are caller policy input — bvisor governs mechanisms, not meanings",
    },
    BoundaryTerm {
        token: "External-Profile",
        reason: "ExtProfile semantics stay outside bvisor",
    },
    BoundaryTerm {
        token: "ExternalProfile",
        reason: "ExtProfile profile types stay outside bvisor",
    },
];

const FAMILY_INTERNAL_BATPAK_PATHS: &[InternalPathTerm] = &[
    InternalPathTerm {
        module: "write",
        reason: "family layers must use batpak's public substrate API, not store write internals",
    },
    InternalPathTerm {
        module: "segment",
        reason: "family layers must use batpak's public substrate API, not segment internals",
    },
    InternalPathTerm {
        module: "index",
        reason: "family layers must use batpak's public substrate API, not index internals",
    },
    InternalPathTerm {
        module: "cold_start",
        reason: "family layers must use batpak's public substrate API, not cold-start internals",
    },
    InternalPathTerm {
        module: "platform",
        reason: "family layers must use batpak's public substrate API, not platform internals",
    },
    InternalPathTerm {
        module: "projection",
        reason: "family layers must use batpak's public substrate API, not projection internals",
    },
    InternalPathTerm {
        module: "delivery",
        reason: "family layers must use batpak's public substrate API, not delivery internals",
    },
    InternalPathTerm {
        module: "ancestry",
        reason: "family layers must use batpak's public substrate API, not ancestry internals",
    },
];

const ASYNC_RUNTIME_DEPS: &[&str] = &[
    "tokio",
    "async-std",
    "smol",
    "glommio",
    "monoio",
    "async-executor",
];

pub(super) fn check(
    repo_root: &Path,
    tracked_files: &[PathBuf],
    source_cache: &mut SourceCache,
) -> Result<()> {
    for path in tracked_files {
        let layer = match source_layer(repo_root, path) {
            Some(layer) => layer,
            None => continue,
        };
        let content = source_cache
            .read_to_string(path)
            .with_context(|| format!("read {}", path.display()))?;

        let semantic_content = semantic_content(&content);

        for term in forbidden_layer_terms(layer, &semantic_content) {
            ensure(
                false,
                format!(
                    "{} layer leak in {}: `{}` ({})",
                    layer.label(),
                    relative(repo_root, path),
                    term.token,
                    term.reason
                ),
            )?;
        }

        if layer.checks_internal_batpak_paths() {
            for term in family_internal_batpak_paths(&semantic_content) {
                ensure(
                    false,
                    format!(
                        "{} batpak internal dependency in {}: `batpak::store::{}` ({})",
                        layer.label(),
                        relative(repo_root, path),
                        term.module,
                        term.reason
                    ),
                )?;
            }
        }

        if checks_runtime_shape(repo_root, path) {
            check_no_async_or_unsafe_runtime_source(repo_root, path, source_cache)?;
        }
    }
    check_family_manifest_boundaries(repo_root)?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceLayer {
    Core,
    Syncbat,
    Netbat,
    /// The PURE `bvisor` contract crate (`crates/bvisor/src/` EXCEPT `src/host/`).
    /// The feature-gated `bvisor::host` module may use syncbat/hostbat and owns
    /// the host wiring — it maps to `None` (kernel plan §11: no `bvisor-host` crate).
    Bvisor,
}

impl SourceLayer {
    fn label(self) -> &'static str {
        match self {
            SourceLayer::Core => "batpak core",
            SourceLayer::Syncbat => "syncbat",
            SourceLayer::Netbat => "netbat",
            SourceLayer::Bvisor => "bvisor (pure contract)",
        }
    }

    fn checks_internal_batpak_paths(self) -> bool {
        matches!(
            self,
            SourceLayer::Syncbat | SourceLayer::Netbat | SourceLayer::Bvisor
        )
    }
}

fn source_layer(repo_root: &Path, path: &Path) -> Option<SourceLayer> {
    let rel = relative(repo_root, path);
    if !rel.ends_with(".rs") {
        return None;
    }
    if rel.starts_with("crates/core/src/") {
        return Some(SourceLayer::Core);
    }
    if rel.starts_with("crates/syncbat/src/") {
        return Some(SourceLayer::Syncbat);
    }
    if rel.starts_with("crates/netbat/src/") {
        return Some(SourceLayer::Netbat);
    }
    // The pure contract only. The feature-gated `bvisor::host` module
    // (`crates/bvisor/src/host/`) is the sanctioned host wiring (kernel plan §11:
    // NO `bvisor-host` crate; host wiring lives here). It legitimately uses
    // syncbat + hostbat, so it is exempt from the pure-contract layer exactly as
    // the old `bvisor-host` crate was — `contract/` and `backend/` stay pure.
    if rel.starts_with("crates/bvisor/src/host/") {
        return None;
    }
    if rel.starts_with("crates/bvisor/src/") {
        return Some(SourceLayer::Bvisor);
    }
    None
}

fn checks_runtime_shape(repo_root: &Path, path: &Path) -> bool {
    let rel = relative(repo_root, path);
    rel.starts_with("crates/syncbat/src/")
        || rel.starts_with("crates/netbat/src/")
        || rel.starts_with("crates/bvisor/src/")
}

fn check_no_async_or_unsafe_runtime_source(
    repo_root: &Path,
    path: &Path,
    source_cache: &mut SourceCache,
) -> Result<()> {
    let parsed = source_cache
        .parse_rust(path)
        .with_context(|| format!("parse {}", relative(repo_root, path)))?;
    let mut visitor = RuntimeShapeVisitor::default();
    visitor.visit_file(&parsed);
    if visitor.findings.is_empty() {
        return Ok(());
    }
    ensure(
        false,
        format!(
            "runtime layer source must stay sync-first and safe Rust in {}: {}",
            relative(repo_root, path),
            visitor.findings.join(", ")
        ),
    )
}

#[derive(Default)]
struct RuntimeShapeVisitor {
    findings: Vec<String>,
}

impl<'ast> Visit<'ast> for RuntimeShapeVisitor {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        check_signature(&node.sig, &mut self.findings);
        syn::visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        check_signature(&node.sig, &mut self.findings);
        syn::visit::visit_impl_item_fn(self, node);
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        check_signature(&node.sig, &mut self.findings);
        syn::visit::visit_trait_item_fn(self, node);
    }

    fn visit_expr_unsafe(&mut self, node: &'ast syn::ExprUnsafe) {
        self.findings.push("unsafe block".to_owned());
        syn::visit::visit_expr_unsafe(self, node);
    }
}

fn check_signature(sig: &syn::Signature, findings: &mut Vec<String>) {
    if sig.asyncness.is_some() {
        findings.push(format!("async fn `{}`", sig.ident));
    }
    if sig.unsafety.is_some() {
        findings.push(format!("unsafe fn `{}`", sig.ident));
    }
}

fn check_family_manifest_boundaries(repo_root: &Path) -> Result<()> {
    let manifests = [
        ManifestRule {
            label: "batpak core",
            rel: "crates/core/Cargo.toml",
            forbidden_stack_deps: &["syncbat", "downstream-kit", "netbat", "bvisor", "extprofile", "downstream-frontend"],
        },
        ManifestRule {
            label: "syncbat",
            rel: "crates/syncbat/Cargo.toml",
            forbidden_stack_deps: &["downstream-kit", "netbat", "bvisor", "extprofile", "downstream-frontend"],
        },
        ManifestRule {
            label: "netbat",
            rel: "crates/netbat/Cargo.toml",
            forbidden_stack_deps: &["batpak", "downstream-kit", "bvisor", "extprofile", "downstream-frontend"],
        },
        // The PURE bvisor contract depends on the batpak generic substrate ONLY.
        // The `host` feature legitimately adds OPTIONAL syncbat + hostbat deps for
        // the `bvisor::host` module (kernel plan §11 — no `bvisor-host` crate), so
        // `syncbat` is NOT forbidden at the manifest level; the source-layer scan
        // still forbids syncbat USAGE in the pure `contract/` + `backend/` files.
        // `netbat` stays forbidden until a host transport slice adds it.
        ManifestRule {
            label: "bvisor (pure contract)",
            rel: "crates/bvisor/Cargo.toml",
            forbidden_stack_deps: &["netbat", "downstream-kit", "extprofile", "downstream-frontend"],
        },
    ];

    for rule in manifests {
        let path = repo_root.join(rule.rel);
        if !path.exists() {
            continue;
        }
        let content = fs::read_to_string(&path).with_context(|| format!("read {}", rule.rel))?;
        let all_deps = dependency_names(&content, true);
        for dep in rule.forbidden_stack_deps {
            ensure(
                !all_deps.contains(*dep),
                format!(
                    "{} must not declare upward stack dependency `{dep}` in {}",
                    rule.label, rule.rel
                ),
            )?;
        }

        let production_deps = dependency_names(&content, false);
        for dep in ASYNC_RUNTIME_DEPS {
            ensure(
                !production_deps.contains(*dep),
                format!(
                    "{} must not declare async runtime dependency `{dep}` in {}",
                    rule.label, rule.rel
                ),
            )?;
        }
    }

    Ok(())
}

struct ManifestRule {
    label: &'static str,
    rel: &'static str,
    forbidden_stack_deps: &'static [&'static str],
}

fn dependency_names(content: &str, include_dev: bool) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut in_dependency_table = false;

    for line in content.lines() {
        let Some(line) = line.split('#').next() else {
            continue;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('[') {
            in_dependency_table = dependency_table_allows(trimmed, include_dev);
            continue;
        }
        if !in_dependency_table {
            continue;
        }
        let Some((name, _)) = trimmed.split_once('=') else {
            continue;
        };
        let name = name.trim().trim_matches('"').trim_matches('\'');
        if !name.is_empty() {
            names.insert(name.to_owned());
        }
    }

    names
}

fn dependency_table_allows(header: &str, include_dev: bool) -> bool {
    if header == "[dependencies]" || header == "[build-dependencies]" {
        return true;
    }
    if header == "[dev-dependencies]" {
        return include_dev;
    }
    if header.starts_with("[target.") && header.ends_with(".dependencies]") {
        return true;
    }
    if header.starts_with("[target.") && header.ends_with(".build-dependencies]") {
        return true;
    }
    include_dev && header.starts_with("[target.") && header.ends_with(".dev-dependencies]")
}

fn forbidden_layer_terms(layer: SourceLayer, content: &str) -> Vec<&'static BoundaryTerm> {
    let terms = match layer {
        SourceLayer::Core => CORE_LAYER_LEAKS,
        SourceLayer::Syncbat => SYNCBAT_LAYER_LEAKS,
        SourceLayer::Netbat => NETBAT_LAYER_LEAKS,
        SourceLayer::Bvisor => BVISOR_LAYER_LEAKS,
    };
    matching_terms(terms, content)
}

fn family_internal_batpak_paths(content: &str) -> Vec<&'static InternalPathTerm> {
    let compact = compact(content);
    FAMILY_INTERNAL_BATPAK_PATHS
        .iter()
        .filter(|term| {
            let direct = format!("batpak::store::{}", term.module);
            let grouped_crate = format!("batpak::{{store::{}", term.module);
            let nested_grouped_crate = format!("batpak::{{store::{{{}", term.module);
            compact.contains(&direct)
                || compact.contains(&grouped_crate)
                || compact.contains(&nested_grouped_crate)
                || grouped_path_contains(&compact, "batpak::store::{", term.module)
                || grouped_path_contains(&compact, "batpak::{store::{", term.module)
        })
        .collect()
}

fn matching_terms(terms: &'static [BoundaryTerm], content: &str) -> Vec<&'static BoundaryTerm> {
    terms
        .iter()
        .filter(|term| content.contains(term.token))
        .collect()
}

fn semantic_content(content: &str) -> String {
    strip_block_comments(content)
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !(trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with("//!"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn grouped_path_contains(content: &str, prefix: &str, module: &str) -> bool {
    let mut rest = content;
    while let Some(start) = rest.find(prefix) {
        let group = &rest[start + prefix.len()..];
        let end = group.find('}').unwrap_or(group.len());
        let group = &group[..end];
        if group_entry_matches(group, module) {
            return true;
        }
        rest = &rest[start + prefix.len()..];
    }
    false
}

fn group_entry_matches(group: &str, module: &str) -> bool {
    let colon = format!("{module}::");
    let comma = format!("{module},");
    let brace = format!("{module}}}");
    group == module
        || group.starts_with(&colon)
        || group.starts_with(&comma)
        || group.ends_with(&brace)
        || group.contains(&format!(",{colon}"))
        || group.contains(&format!("{{{colon}"))
        || group.contains(&format!(",{comma}"))
        || group.contains(&format!("{{{comma}"))
}

fn strip_block_comments(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut chars = content.chars().peekable();
    let mut in_block = false;

    while let Some(ch) = chars.next() {
        if in_block {
            if ch == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block = false;
            }
            continue;
        }

        if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            in_block = true;
            continue;
        }

        out.push(ch);
    }

    out
}

fn compact(content: &str) -> String {
    content.chars().filter(|ch| !ch.is_whitespace()).collect()
}

#[cfg(test)]
mod tests {
    use super::{
        check_signature, dependency_names, family_internal_batpak_paths, forbidden_layer_terms,
        semantic_content, source_layer, RuntimeShapeVisitor, SourceLayer,
    };
    use std::path::Path;
    use syn::visit::Visit;

    fn tokens(leaks: &[&'static super::BoundaryTerm]) -> Vec<&'static str> {
        leaks.iter().map(|leak| leak.token).collect()
    }

    fn path_modules(leaks: &[&'static super::InternalPathTerm]) -> Vec<&'static str> {
        leaks.iter().map(|leak| leak.module).collect()
    }

    #[test]
    fn detects_core_layer_leaks() {
        let content = "pub struct SyncbatCore;\nconst PROFILE: &str = \"contract.external_v1\";\n";
        let tokens = tokens(&forbidden_layer_terms(SourceLayer::Core, content));
        assert!(tokens.contains(&"Syncbat"));
        assert!(tokens.contains(&"contract.external_v1"));
    }

    #[test]
    fn detects_syncbat_layer_leaks() {
        let content = "pub struct DownstreamKitRuntime;\nconst CLAIM: &str = \"authority_required\";\n";
        let tokens = tokens(&forbidden_layer_terms(SourceLayer::Syncbat, content));
        assert!(tokens.contains(&"DownstreamKit"));
        assert!(tokens.contains(&"authority_required"));
    }

    #[test]
    fn detects_netbat_layer_leaks() {
        let content = "let _ = batpak::Store::open;\nconst CLAIM: &str = \"authority_required\";\n";
        let tokens = tokens(&forbidden_layer_terms(SourceLayer::Netbat, content));
        assert!(tokens.contains(&"batpak::"));
        assert!(tokens.contains(&"authority_required"));
    }

    #[test]
    fn allows_public_substrate_terms() {
        let content = "Store AppendReceipt GateSet Pipeline opaque extension cargo";
        assert!(forbidden_layer_terms(SourceLayer::Core, content).is_empty());
        assert!(forbidden_layer_terms(SourceLayer::Syncbat, content).is_empty());
        assert!(forbidden_layer_terms(SourceLayer::Netbat, content).is_empty());
        assert!(forbidden_layer_terms(SourceLayer::Bvisor, content).is_empty());
    }

    #[test]
    fn detects_bvisor_referenced_from_lower_layers() {
        // The boundary-supervisor layer must not leak DOWN into core/syncbat/netbat.
        let content = "use bvisor::BoundaryPlan;\nlet _ = bvisor::BoundaryRunner::run;\n";
        for layer in [SourceLayer::Core, SourceLayer::Syncbat, SourceLayer::Netbat] {
            let tokens = tokens(&forbidden_layer_terms(layer, content));
            assert!(
                tokens.contains(&"bvisor"),
                "{layer:?} should reject `bvisor`"
            );
        }
    }

    #[test]
    fn detects_runtime_and_transport_leak_in_pure_bvisor() {
        // The PURE contract must not reach UP into host wiring (syncbat) or
        // transport (netbat) — those belong in bvisor-host.
        let content = "use syncbat::Core;\nlet _ = netbat::serve_tcp_listener;\n";
        let tokens = tokens(&forbidden_layer_terms(SourceLayer::Bvisor, content));
        assert!(tokens.contains(&"syncbat"));
        assert!(tokens.contains(&"netbat"));
    }

    #[test]
    fn allows_bvisor_public_batpak_paths() {
        // The pure contract legitimately consumes the batpak generic API.
        let content =
            "use batpak::{EventPayload, canonical};\nuse batpak::event::hash::compute_hash;\n";
        assert!(forbidden_layer_terms(SourceLayer::Bvisor, content).is_empty());
        assert!(family_internal_batpak_paths(content).is_empty());
    }

    #[test]
    fn allows_syncbat_public_batpak_paths() {
        let content = "use batpak::{AppendOptions, Store};\nuse batpak::prelude::*;\n";
        assert!(forbidden_layer_terms(SourceLayer::Syncbat, content).is_empty());
        assert!(family_internal_batpak_paths(content).is_empty());
    }

    #[test]
    fn rejects_syncbat_internal_batpak_paths() {
        let content = "use batpak::store::segment::FrameHeader;\n";
        let tokens = path_modules(&family_internal_batpak_paths(content));
        assert_eq!(tokens, vec!["segment"]);
    }

    #[test]
    fn rejects_syncbat_grouped_internal_batpak_paths() {
        let direct_group = "use batpak::store::{Store, segment::FrameHeader};\n";
        let crate_group = "use batpak::{store::index::IndexEntry};\n";
        let nested_group = "use batpak::{store::{Store, platform::Probe}};\n";

        assert_eq!(
            path_modules(&family_internal_batpak_paths(direct_group)),
            vec!["segment"]
        );
        assert_eq!(
            path_modules(&family_internal_batpak_paths(crate_group)),
            vec!["index"]
        );
        assert_eq!(
            path_modules(&family_internal_batpak_paths(nested_group)),
            vec!["platform"]
        );
    }

    #[test]
    fn ignores_comment_only_boundary_terms() {
        let content = "//! This layer does not implement External-Profile.\n/** Nor ExternalProfile. */\n/*! Nor contract.external_v1. */\npub struct Plain;\n";
        let semantic = semantic_content(content);
        assert!(forbidden_layer_terms(SourceLayer::Syncbat, &semantic).is_empty());
    }

    #[test]
    fn selects_only_production_rust_sources() {
        let root = Path::new("/repo");

        assert_eq!(
            source_layer(root, Path::new("/repo/crates/core/src/store/mod.rs")),
            Some(SourceLayer::Core)
        );
        assert_eq!(
            source_layer(root, Path::new("/repo/crates/syncbat/src/lib.rs")),
            Some(SourceLayer::Syncbat)
        );
        assert_eq!(
            source_layer(root, Path::new("/repo/crates/netbat/src/lib.rs")),
            Some(SourceLayer::Netbat)
        );
        assert_eq!(
            source_layer(root, Path::new("/repo/crates/bvisor/src/contract/plan.rs")),
            Some(SourceLayer::Bvisor)
        );
        // The `bvisor::host` feature module is NOT the pure layer — it owns the
        // syncbat/hostbat wiring (kernel plan §11), so it maps to None.
        assert_eq!(
            source_layer(root, Path::new("/repo/crates/bvisor/src/host/mod.rs")),
            None
        );
        assert_eq!(
            source_layer(
                root,
                Path::new("/repo/crates/core/tests/substrate_additions.rs")
            ),
            None
        );
        assert_eq!(
            source_layer(root, Path::new("/repo/crates/syncbat/examples/basic.rs")),
            None
        );
        assert_eq!(
            source_layer(root, Path::new("/repo/crates/syncbat/src/readme.md")),
            None
        );
    }

    #[test]
    fn dependency_names_respect_dependency_tables() {
        let manifest = r#"
[dependencies]
syncbat = { path = "../syncbat" }
tokio = "1"

[dev-dependencies]
netbat = { path = "../netbat" }

[target.'cfg(unix)'.dependencies]
smol = "2"
"#;

        let production = dependency_names(manifest, false);
        let all = dependency_names(manifest, true);

        assert!(production.contains("syncbat"));
        assert!(production.contains("tokio"));
        assert!(production.contains("smol"));
        assert!(!production.contains("netbat"));
        assert!(all.contains("netbat"));
    }

    #[test]
    fn runtime_shape_visitor_detects_async_and_unsafe_items() {
        let parsed: syn::File = syn::parse_quote! {
            async fn bad_async() {}
            unsafe fn bad_unsafe() {}
            fn bad_block() {
                unsafe {}
            }
        };
        let mut visitor = RuntimeShapeVisitor::default();
        visitor.visit_file(&parsed);

        assert!(visitor
            .findings
            .iter()
            .any(|finding| finding == "async fn `bad_async`"));
        assert!(visitor
            .findings
            .iter()
            .any(|finding| finding == "unsafe fn `bad_unsafe`"));
        assert!(visitor
            .findings
            .iter()
            .any(|finding| finding == "unsafe block"));
    }

    #[test]
    fn signature_check_allows_plain_sync_safe_functions() {
        let sig: syn::Signature = syn::parse_quote! {
            fn plain(input: &[u8]) -> Vec<u8>
        };
        let mut findings = Vec::new();
        check_signature(&sig, &mut findings);

        assert!(findings.is_empty());
    }
}
