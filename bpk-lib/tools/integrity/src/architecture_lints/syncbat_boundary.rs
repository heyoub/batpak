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
        token: "authority_required",
        reason: "authority claims are caller policy input, not substrate law",
    },
];

const SYNCBAT_LAYER_LEAKS: &[BoundaryTerm] = &[
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
        token: "authority_required",
        reason: "authority claims are caller policy input, not syncbat law",
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
        token: "authority_required",
        reason: "authority claims are caller policy input, not netbat law",
    },
    BoundaryTerm {
        token: "batpak::",
        reason: "netbat should expose syncbat, not bypass the runtime into batpak",
    },
];

// The single-threaded Linux confinement LAUNCHER (`crates/bvisor/launcher/<os>/`,
// kernel plan §10.8). It is a separate `[[bin]]` in the bvisor PACKAGE — a real-OS
// trust boundary that legitimately uses std/libc + the backend protocol types +
// `batpak::canonical` decode. It must NOT reach into the network layer (`netbat`):
// the launcher has NO network client (it execs and is replaced), so a `netbat`
// token is a layer leak. The host-wiring layer (syncbat) and the contract-layer
// names also have no business in the launcher. We do NOT forbid
// `bvisor`/`batpak` here: the launcher consumes the bvisor backend protocol and
// `batpak::canonical`, which are its sanctioned inputs.
const BVISOR_LAUNCHER_LAYER_LEAKS: &[BoundaryTerm] = &[
    BoundaryTerm {
        token: "netbat",
        reason: "the launcher has no network client — network access belongs nowhere in the confinement launcher",
    },
    BoundaryTerm {
        token: "Netbat",
        reason: "the launcher has no network client — network types belong nowhere in the confinement launcher",
    },
    BoundaryTerm {
        token: "syncbat",
        reason: "host wiring belongs in the bvisor::host feature module, not the confinement launcher",
    },
    BoundaryTerm {
        token: "Syncbat",
        reason: "host wiring belongs in the bvisor::host feature module, not the confinement launcher",
    },
    BoundaryTerm {
        token: "authority_required",
        reason: "authority claims are caller policy input — the launcher executes mechanisms, not meanings",
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
        token: "authority_required",
        reason:
            "authority claims are caller policy input — bvisor governs mechanisms, not meanings",
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

        if is_launcher_source(&relative(repo_root, path)) {
            // The launcher is single-threaded BY CONSTRUCTION (kernel plan §10.8):
            // it must NEVER create a thread. Combined with the no-async ban
            // (runtime-shape, above) this makes it structurally single-threaded —
            // it execs and is replaced, so there is no "after the boundary".
            check_no_thread_spawn_in_launcher(repo_root, path, source_cache)?;
        }
    }
    check_family_manifest_boundaries(repo_root)?;
    Ok(())
}

/// Whether `rel` is a tracked launcher Rust source: any `.rs` under
/// `crates/bvisor/launcher/`. This INCLUDES the launcher basement — the
/// no-thread-spawn rule has no basement carve-out (an OS basement may use raw
/// `unsafe` syscalls, but it still may not spawn a thread before the confinement
/// boundary).
fn is_launcher_source(rel: &str) -> bool {
    rel.ends_with(".rs") && rel.starts_with("crates/bvisor/launcher/")
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
    /// The single-threaded Linux confinement LAUNCHER (`crates/bvisor/launcher/<os>/`,
    /// kernel plan §10.8): a separate `[[bin]]` in the bvisor package that performs
    /// real-OS `unsafe` (memfd/fstat/exec). It is BORN inside the assurance machine
    /// — the layer scan, runtime-shape ban (minus its `sys.rs`/`sys/` basement), and
    /// the single-thread gate all apply to it.
    BvisorLauncher,
}

impl SourceLayer {
    fn label(self) -> &'static str {
        match self {
            SourceLayer::Core => "batpak core",
            SourceLayer::Syncbat => "syncbat",
            SourceLayer::Netbat => "netbat",
            SourceLayer::Bvisor => "bvisor (pure contract)",
            SourceLayer::BvisorLauncher => "bvisor launcher",
        }
    }

    fn checks_internal_batpak_paths(self) -> bool {
        matches!(
            self,
            SourceLayer::Syncbat
                | SourceLayer::Netbat
                | SourceLayer::Bvisor
                | SourceLayer::BvisorLauncher
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
    // The confinement launcher (`crates/bvisor/launcher/<os>/`, kernel plan §10.8).
    // It is a separate `[[bin]]` in the bvisor package, NOT under `src/`, so it was
    // previously INVISIBLE to this check loop (`source_layer == None ⇒ continue`),
    // which would let launcher unsafe/async escape the gate. Mapping it here brings
    // it inside the assurance machine: layer scan + runtime-shape ban (minus the
    // basement) + the single-thread gate all bite.
    if rel.starts_with("crates/bvisor/launcher/") {
        return Some(SourceLayer::BvisorLauncher);
    }
    None
}

fn checks_runtime_shape(repo_root: &Path, path: &Path) -> bool {
    let rel = relative(repo_root, path);
    if is_unsafe_basement(&rel) {
        // The SANCTIONED unsafe basement (kernel plan §10.8 quarantine): the ONLY
        // place real OS-backend `unsafe` is allowed to live. Every unsafe block
        // here is reconciled against `traceability/unsafe_ledger.yaml` by
        // `unsafe_ledger::check` (fail-closed), NOT by the blanket runtime-shape
        // ban — so this file is exempt from the sync-first/safe-Rust visitor.
        //
        // The exemption is DELIBERATELY narrow: only `backend/<os>/sys.rs` (or a
        // `backend/<os>/sys/` dir). The SAFE `backend/<os>/mod.rs` orchestration
        // and ALL of `contract/` stay fully runtime-shape-checked.
        return false;
    }
    rel.starts_with("crates/syncbat/src/")
        || rel.starts_with("crates/netbat/src/")
        || rel.starts_with("crates/bvisor/src/")
        // The confinement launcher (kernel plan §10.8): async + unsafe-outside-basement
        // are banned there too. Its `sys.rs`/`sys/` basement is exempted by the
        // `is_unsafe_basement` early-return above, exactly like the backend basement.
        || rel.starts_with("crates/bvisor/launcher/")
}

/// Whether `rel` is a sanctioned unsafe-basement file. Two sanctioned basement
/// roots (kernel plan §10.8), same shape (`/sys.rs` or under a `/sys/` dir):
///   - the backend basement: `crates/bvisor/src/backend/<os>/sys.rs` or `sys/`;
///   - the launcher basement: `crates/bvisor/launcher/<os>/sys.rs` or `sys/`.
///
/// NOTHING else is exempt — the safe `mod.rs`/`main.rs` orchestration stays
/// runtime-shape-checked, and each basement's `unsafe` is reconciled by the
/// ledger gate. KEPT IN LOCKSTEP with `unsafe_ledger::is_basement`.
pub(crate) fn is_unsafe_basement(rel: &str) -> bool {
    let backend_basement = rel.contains("crates/bvisor/src/backend/");
    let launcher_basement = rel.contains("crates/bvisor/launcher/");
    (backend_basement || launcher_basement) && (rel.ends_with("/sys.rs") || rel.contains("/sys/"))
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

/// The launcher single-thread structural gate (kernel plan §10.8). Scan every
/// `.rs` under `crates/bvisor/launcher/` with syn and FAIL CLOSED on any thread
/// creation. AST over raw text is deliberate: thread spawns are CALL expressions
/// whose meaning lives in the call target's path tail (`thread::spawn`,
/// `Builder::…spawn`, `thread::scope`, the family `Spawn::spawn`); a syn scan
/// matches the real call sites while ignoring identically-spelled tokens in
/// comments, strings, and doc examples — exactly how the runtime-shape and
/// unsafe-ledger gates already operate. A raw token scan would either miss
/// method-call forms (`builder.spawn(...)`) or false-positive on prose.
fn check_no_thread_spawn_in_launcher(
    repo_root: &Path,
    path: &Path,
    source_cache: &mut SourceCache,
) -> Result<()> {
    let parsed = source_cache
        .parse_rust(path)
        .with_context(|| format!("parse {}", relative(repo_root, path)))?;
    let mut visitor = ThreadSpawnVisitor::default();
    visitor.visit_file(&parsed);
    if visitor.findings.is_empty() {
        return Ok(());
    }
    ensure(
        false,
        format!(
            "the confinement launcher is single-threaded by construction and MUST NOT create a \
             thread (kernel plan §10.8) in {}: {} [GAUNT-LAUNCHER-SINGLE-THREAD]",
            relative(repo_root, path),
            visitor.findings.join(", ")
        ),
    )
}

/// A syn visitor that records every thread-creation call site in a launcher file.
///
/// Matches, by the call target's path/method tail:
///   - `std::thread::spawn` / `thread::spawn`     (free function),
///   - `std::thread::scope` / `thread::scope`     (scoped threads),
///   - `Builder::…spawn` / `…spawn_scoped`        (`thread::Builder` methods),
///   - the family `Spawn` trait's `.spawn(...)`   (method-call form).
///
/// The `Builder` and `Spawn` cases are method calls, so they are caught by
/// `visit_expr_method_call` on the `spawn`/`spawn_scoped` method name; the free
/// `thread::spawn`/`thread::scope` cases are path calls caught on the path tail.
#[derive(Default)]
struct ThreadSpawnVisitor {
    findings: Vec<String>,
}

impl<'ast> Visit<'ast> for ThreadSpawnVisitor {
    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = node.func.as_ref() {
            if let Some(segments) = path_tail_two(&path.path) {
                let (head, tail) = segments;
                // `thread::spawn(...)` / `std::thread::spawn(...)` and the scoped
                // form `thread::scope(...)`. Require the `thread` module qualifier
                // so a project-local free fn named `spawn` is not swept in.
                if head == "thread" && (tail == "spawn" || tail == "scope") {
                    self.findings
                        .push(format!("thread-creating call `thread::{tail}`"));
                }
            }
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let method = node.method.to_string();
        // `Builder::new().spawn(...)` / `.spawn_scoped(...)` and the family
        // `Spawn` trait's `.spawn(...)`. Any `.spawn`/`.spawn_scoped` method call
        // in launcher source is a thread creation — fail closed.
        if method == "spawn" || method == "spawn_scoped" {
            self.findings
                .push(format!("thread-creating method call `.{method}()`"));
        }
        syn::visit::visit_expr_method_call(self, node);
    }
}

/// The last two `::`-segments of a path, as `(head, tail)`. For a single-segment
/// path returns `None` (no qualifier to anchor on).
fn path_tail_two(path: &syn::Path) -> Option<(String, String)> {
    let len = path.segments.len();
    if len < 2 {
        return None;
    }
    let head = path.segments[len - 2].ident.to_string();
    let tail = path.segments[len - 1].ident.to_string();
    Some((head, tail))
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
            forbidden_stack_deps: &["syncbat", "netbat", "bvisor"],
        },
        ManifestRule {
            label: "syncbat",
            rel: "crates/syncbat/Cargo.toml",
            forbidden_stack_deps: &["netbat", "bvisor"],
        },
        ManifestRule {
            label: "netbat",
            rel: "crates/netbat/Cargo.toml",
            forbidden_stack_deps: &["batpak", "bvisor"],
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
            forbidden_stack_deps: &["netbat"],
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
        SourceLayer::BvisorLauncher => BVISOR_LAUNCHER_LAYER_LEAKS,
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
        check_signature, checks_runtime_shape, dependency_names, family_internal_batpak_paths,
        forbidden_layer_terms, is_launcher_source, is_unsafe_basement, semantic_content,
        source_layer, RuntimeShapeVisitor, SourceLayer, ThreadSpawnVisitor,
    };
    use std::path::Path;
    use syn::visit::Visit;

    /// Run the launcher single-thread visitor over a synthetic file and return its
    /// findings (mirrors how the gate parses + visits a real launcher source).
    fn thread_spawn_findings(file: &syn::File) -> Vec<String> {
        let mut visitor = ThreadSpawnVisitor::default();
        visitor.visit_file(file);
        visitor.findings
    }

    /// Run the runtime-shape visitor over a synthetic file and return its findings.
    fn runtime_shape_findings(file: &syn::File) -> Vec<String> {
        let mut visitor = RuntimeShapeVisitor::default();
        visitor.visit_file(file);
        visitor.findings
    }

    fn tokens(leaks: &[&'static super::BoundaryTerm]) -> Vec<&'static str> {
        leaks.iter().map(|leak| leak.token).collect()
    }

    fn path_modules(leaks: &[&'static super::InternalPathTerm]) -> Vec<&'static str> {
        leaks.iter().map(|leak| leak.module).collect()
    }

    #[test]
    fn detects_core_layer_leaks() {
        let content = "pub struct SyncbatCore;\nuse netbat::Client;\n";
        let tokens = tokens(&forbidden_layer_terms(SourceLayer::Core, content));
        assert!(tokens.contains(&"Syncbat"));
        assert!(tokens.contains(&"netbat"));
    }

    #[test]
    fn detects_syncbat_layer_leaks() {
        let content = "pub struct BvisorRuntime;\nconst CLAIM: &str = \"authority_required\";\n";
        let tokens = tokens(&forbidden_layer_terms(SourceLayer::Syncbat, content));
        assert!(tokens.contains(&"Bvisor"));
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
        let content = "//! This layer does not depend on netbat.\n/** Nor Bvisor. */\n/*! Nor bvisor. */\npub struct Plain;\n";
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
    fn runtime_shape_covers_safe_orchestration_but_exempts_unsafe_basement() {
        let root = Path::new("/repo");

        // The SAFE backend orchestration MUST stay runtime-shape-checked: a stray
        // `unsafe`/`async` in `mod.rs` is a real finding, never silently allowed.
        let mod_rs = Path::new("/repo/crates/bvisor/src/backend/linux/mod.rs");
        assert!(
            checks_runtime_shape(root, mod_rs),
            "backend/linux/mod.rs must stay runtime-shape-checked"
        );
        assert!(!is_unsafe_basement(
            "crates/bvisor/src/backend/linux/mod.rs"
        ));

        // The SANCTIONED unsafe basement is the ONLY exempt file.
        let sys_rs = Path::new("/repo/crates/bvisor/src/backend/linux/sys.rs");
        assert!(
            !checks_runtime_shape(root, sys_rs),
            "backend/linux/sys.rs is the sanctioned unsafe basement (exempt)"
        );
        assert!(is_unsafe_basement("crates/bvisor/src/backend/linux/sys.rs"));
        assert!(is_unsafe_basement(
            "crates/bvisor/src/backend/windows/sys.rs"
        ));
        assert!(is_unsafe_basement(
            "crates/bvisor/src/backend/linux/sys/raw.rs"
        ));

        // The exemption is narrow: `contract/` stays fully checked, and a
        // confusingly-named non-basement file is NOT exempt.
        assert!(checks_runtime_shape(
            root,
            Path::new("/repo/crates/bvisor/src/contract/registry.rs")
        ));
        assert!(!is_unsafe_basement("crates/bvisor/src/contract/sys.rs"));
        assert!(!is_unsafe_basement(
            "crates/bvisor/src/backend/linux/system.rs"
        ));
    }

    #[test]
    fn launcher_is_visible_to_the_check_loop() {
        // BEFORE this gap was closed the launcher mapped to None and the check loop
        // `continue`-skipped it — unsafe/async would have escaped. It must now map
        // to its own layer so the loop processes it.
        let root = Path::new("/repo");
        assert_eq!(
            source_layer(
                root,
                Path::new("/repo/crates/bvisor/launcher/linux/main.rs")
            ),
            Some(SourceLayer::BvisorLauncher)
        );
        assert_eq!(SourceLayer::BvisorLauncher.label(), "bvisor launcher");
        assert!(is_launcher_source("crates/bvisor/launcher/linux/main.rs"));
        assert!(is_launcher_source("crates/bvisor/launcher/linux/sys.rs"));
        assert!(!is_launcher_source(
            "crates/bvisor/src/backend/linux/mod.rs"
        ));
        assert!(!is_launcher_source("crates/bvisor/launcher/linux/notes.md"));
    }

    #[test]
    fn launcher_legit_imports_are_not_layer_or_path_leaks() {
        // The launcher legitimately consumes the bvisor backend protocol types and
        // `batpak::canonical` decode. Neither may trip a layer-term leak nor a
        // batpak-internal-path leak.
        let content = "use bvisor::backend::linux::protocol::LinuxLaunchPlanV1;\n\
                       use batpak::canonical::decode;\n\
                       use std::os::unix::io::RawFd;\n\
                       extern crate libc;\n";
        assert!(
            forbidden_layer_terms(SourceLayer::BvisorLauncher, content).is_empty(),
            "protocol + canonical + std/libc are the launcher's sanctioned inputs"
        );
        assert!(family_internal_batpak_paths(content).is_empty());
    }

    #[test]
    fn launcher_rejects_netbat_and_host_wiring_leaks() {
        // No network client (it execs and is replaced) and no host wiring.
        let content = "use netbat::serve_tcp_listener;\nuse syncbat::Core;\n";
        let tokens = tokens(&forbidden_layer_terms(SourceLayer::BvisorLauncher, content));
        assert!(tokens.contains(&"netbat"), "launcher must reject netbat");
        assert!(tokens.contains(&"syncbat"), "launcher must reject syncbat");
    }

    #[test]
    fn launcher_basement_is_exempt_but_main_is_runtime_shape_checked() {
        let root = Path::new("/repo");

        // The launcher `main.rs` orchestration IS runtime-shape-checked: a stray
        // `async`/`unsafe` there is a real finding.
        let main_rs = Path::new("/repo/crates/bvisor/launcher/linux/main.rs");
        assert!(
            checks_runtime_shape(root, main_rs),
            "launcher/linux/main.rs must stay runtime-shape-checked"
        );
        assert!(!is_unsafe_basement("crates/bvisor/launcher/linux/main.rs"));

        // The launcher `sys.rs` (and `sys/` dir) is the sanctioned unsafe basement,
        // exactly like the backend basement — exempt from the runtime-shape ban.
        let sys_rs = Path::new("/repo/crates/bvisor/launcher/linux/sys.rs");
        assert!(
            !checks_runtime_shape(root, sys_rs),
            "launcher/linux/sys.rs is the sanctioned unsafe basement (exempt)"
        );
        assert!(is_unsafe_basement("crates/bvisor/launcher/linux/sys.rs"));
        assert!(is_unsafe_basement(
            "crates/bvisor/launcher/linux/sys/raw.rs"
        ));
    }

    #[test]
    fn launcher_async_fn_in_main_is_caught() {
        // RED fixture: an `async fn` planted in the launcher main.rs (non-basement)
        // is caught by the runtime-shape visitor.
        let parsed: syn::File = syn::parse_quote! {
            async fn coordinate() {}
        };
        let findings = runtime_shape_findings(&parsed);
        assert!(
            findings.iter().any(|f| f == "async fn `coordinate`"),
            "an async fn in the launcher must be caught, got {findings:?}"
        );
    }

    #[test]
    fn launcher_unsafe_block_in_main_is_caught() {
        // RED fixture: an `unsafe {}` block planted in the launcher main.rs
        // (NON-basement) is caught by the runtime-shape visitor. Only the launcher
        // `sys.rs`/`sys/` basement is exempt — main.rs is not.
        let parsed: syn::File = syn::parse_quote! {
            fn coordinate() {
                unsafe { libc::exit(0) };
            }
        };
        let findings = runtime_shape_findings(&parsed);
        assert!(
            findings.iter().any(|f| f == "unsafe block"),
            "an unsafe block in the launcher main.rs must be caught, got {findings:?}"
        );
    }

    #[test]
    fn launcher_thread_spawn_is_caught_by_single_thread_gate() {
        // RED fixture: `std::thread::spawn(...)` anywhere under the launcher is
        // caught by the single-thread gate (the SPECIFIC thread::spawn finding).
        let parsed: syn::File = syn::parse_quote! {
            fn coordinate() {
                std::thread::spawn(|| {});
            }
        };
        let findings = thread_spawn_findings(&parsed);
        assert!(
            findings
                .iter()
                .any(|f| f == "thread-creating call `thread::spawn`"),
            "std::thread::spawn must be caught, got {findings:?}"
        );
    }

    #[test]
    fn launcher_thread_builder_and_scope_and_spawn_trait_are_caught() {
        // The single-thread gate also catches the Builder method form, scoped
        // threads, the `spawn_scoped` method, and the family `Spawn` trait spawn.
        let builder: syn::File = syn::parse_quote! {
            fn a() { let _ = std::thread::Builder::new().spawn(|| {}); }
        };
        assert!(thread_spawn_findings(&builder)
            .iter()
            .any(|f| f == "thread-creating method call `.spawn()`"));

        let scope: syn::File = syn::parse_quote! {
            fn b() { std::thread::scope(|s| { let _ = s; }); }
        };
        assert!(thread_spawn_findings(&scope)
            .iter()
            .any(|f| f == "thread-creating call `thread::scope`"));

        let scoped: syn::File = syn::parse_quote! {
            fn c() { std::thread::scope(|s| { let _ = s.spawn(|| {}); }); }
        };
        let scoped_findings = thread_spawn_findings(&scoped);
        assert!(scoped_findings
            .iter()
            .any(|f| f == "thread-creating method call `.spawn()`"));

        let trait_spawn: syn::File = syn::parse_quote! {
            fn d(spawner: &S) { let _ = spawner.spawn(job); }
        };
        assert!(thread_spawn_findings(&trait_spawn)
            .iter()
            .any(|f| f == "thread-creating method call `.spawn()`"));
    }

    #[test]
    fn launcher_single_thread_gate_ignores_unrelated_calls() {
        // A plain sync launcher coordinator with no thread creation is clean — the
        // gate must not false-positive on ordinary calls (incl. a local fn named
        // `spawn` with no `thread::` qualifier, or `process::exit`).
        let parsed: syn::File = syn::parse_quote! {
            fn coordinate() {
                let plan = decode(bytes);
                fstat(fd);
                std::process::exit(0);
            }
        };
        assert!(
            thread_spawn_findings(&parsed).is_empty(),
            "ordinary launcher calls must not trip the single-thread gate"
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
