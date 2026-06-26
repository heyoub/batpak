//! D11 STORE_SYNC_ONLY — the STRUCTURAL (AST) half.
//!
//! INV-STORE-SYNC-ONLY says the store's public surface is SYNC; any async lives
//! in the caller's executor over `flume` channels. The old build.rs check was a
//! `contents.contains("async fn")` SUBSTRING grep over store files — it missed
//! every shape that is async WITHOUT the literal `async fn`:
//!
//! * a `-> impl Future<..>` return,
//! * a boxed `-> Pin<Box<dyn Future<..>>>` return or a `type` alias of one,
//! * an `#[async_trait]` impl (the proc-macro expands to boxed futures),
//! * a stray `.await` / `async {}` block in a sync fn body.
//!
//! This gate parses the production store code with `syn` and flags each shape
//! structurally. It is scoped to PRODUCTION store code: `#[cfg(test)]` modules
//! and `*_tests.rs` sibling files are excluded (test helpers may use whatever).
//!
//! flume is a synchronous channel library; `recv_async` is flume's own method
//! name and is NOT an `.await` — the AST walker only flags real `Expr::Await`
//! nodes and real `async`/Future syntax, so flume usage is never flagged.

use crate::repo_surface::{core_src_root, relative, rust_files};
use crate::source_cache::SourceCache;
use anyhow::{anyhow, bail, Result};
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
use syn::visit::Visit;

/// One structural async violation found in production store code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AsyncViolation {
    pub line: usize,
    pub kind: AsyncKind,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AsyncKind {
    /// `async fn` (public or not) in production store code.
    AsyncFn,
    /// A function returning `impl Future` / `impl Stream`.
    ImplFutureReturn,
    /// A function or `type` alias returning/aliasing `Pin<Box<dyn Future>>`.
    BoxedFutureType,
    /// An `#[async_trait]` attribute (its expansion is boxed futures).
    AsyncTraitAttr,
    /// An `.await` expression or an `async {}` block in a fn body.
    AwaitOrAsyncBlock,
}

impl AsyncKind {
    fn label(self) -> &'static str {
        match self {
            AsyncKind::AsyncFn => "async fn",
            AsyncKind::ImplFutureReturn => "-> impl Future return",
            AsyncKind::BoxedFutureType => "Pin<Box<dyn Future>> type",
            AsyncKind::AsyncTraitAttr => "#[async_trait]",
            AsyncKind::AwaitOrAsyncBlock => ".await / async block",
        }
    }
}

/// The production store source files: every `*.rs` under `crates/core/src/store`
/// EXCEPT `*_tests.rs` siblings (which are test islands by convention).
pub(crate) fn store_production_files(repo_root: &Path) -> Vec<PathBuf> {
    let store_root = core_src_root(repo_root).join("store");
    rust_files(&store_root)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !name.ends_with("_tests.rs"))
        })
        .collect()
}

/// The gate: parse every production store file and fail on the first structural
/// async shape. Routes through the shared [`SourceCache`] like the sibling
/// structural lints.
pub(crate) fn check(repo_root: &Path, source_cache: &mut SourceCache) -> Result<()> {
    for path in store_production_files(repo_root) {
        let rel = relative(repo_root, &path);
        let file = source_cache
            .parse_rust(&path)
            .map_err(|err| anyhow!("parse store-sync gate target {rel}: {err}"))?;
        let violations = scan_file(&file);
        if let Some(v) = violations.first() {
            bail!(
                "structural-check (INV-STORE-SYNC-ONLY): async store surface in {rel}:{} — {} ({}).\n\
                 The store API is SYNC. Async callers use flume's recv_async() or spawn_blocking()\n\
                 in their OWN executor; production store code must not be async (no async fn, no\n\
                 impl-Future/boxed-Future return, no #[async_trait], no .await/async block).\n\
                 See ADR-0001, INV-STORE-SYNC-ONLY, store/delivery/subscription.rs.",
                v.line,
                v.kind.label(),
                v.detail,
            );
        }
    }
    Ok(())
}

/// PURE: collect every structural async violation in a parsed file, skipping
/// `#[cfg(test)]` modules. Testable in isolation by the red fixtures.
pub(crate) fn scan_file(file: &syn::File) -> Vec<AsyncViolation> {
    let mut visitor = AsyncShapeVisitor::default();
    visitor.visit_file(file);
    visitor.violations.sort_by_key(|v| v.line);
    visitor.violations
}

#[derive(Default)]
struct AsyncShapeVisitor {
    violations: Vec<AsyncViolation>,
}

impl AsyncShapeVisitor {
    fn record(&mut self, span: proc_macro2::Span, kind: AsyncKind, detail: impl Into<String>) {
        self.violations.push(AsyncViolation {
            line: span.start().line,
            kind,
            detail: detail.into(),
        });
    }

    fn check_attrs_for_async_trait(&mut self, attrs: &[syn::Attribute]) {
        for attr in attrs {
            if attr
                .path()
                .segments
                .last()
                .is_some_and(|seg| seg.ident == "async_trait")
            {
                self.record(
                    attr.span(),
                    AsyncKind::AsyncTraitAttr,
                    "async_trait attribute",
                );
            }
        }
    }

    fn check_signature(&mut self, sig: &syn::Signature, attrs: &[syn::Attribute]) {
        self.check_attrs_for_async_trait(attrs);
        if sig.asyncness.is_some() {
            self.record(sig.span(), AsyncKind::AsyncFn, format!("fn {}", sig.ident));
        }
        if let syn::ReturnType::Type(_, ty) = &sig.output {
            self.check_return_type(ty, &sig.ident.to_string());
        }
    }

    fn check_return_type(&mut self, ty: &syn::Type, fn_name: &str) {
        if type_is_impl_future(ty) {
            self.record(
                ty.span(),
                AsyncKind::ImplFutureReturn,
                format!("fn {fn_name} returns impl Future/Stream"),
            );
        }
        if type_is_boxed_future(ty) {
            self.record(
                ty.span(),
                AsyncKind::BoxedFutureType,
                format!("fn {fn_name} returns a boxed Future"),
            );
        }
    }
}

impl<'ast> Visit<'ast> for AsyncShapeVisitor {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        // `#[cfg(test)]` modules are test islands, not the production store
        // surface — skip them entirely (do not descend).
        if module_is_cfg_test(&node.attrs) {
            return;
        }
        syn::visit::visit_item_mod(self, node);
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.check_signature(&node.sig, &node.attrs);
        syn::visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.check_signature(&node.sig, &node.attrs);
        syn::visit::visit_impl_item_fn(self, node);
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        self.check_signature(&node.sig, &node.attrs);
        syn::visit::visit_trait_item_fn(self, node);
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        self.check_attrs_for_async_trait(&node.attrs);
        syn::visit::visit_item_impl(self, node);
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        self.check_attrs_for_async_trait(&node.attrs);
        syn::visit::visit_item_trait(self, node);
    }

    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        if type_is_boxed_future(&node.ty) || type_is_impl_future(&node.ty) {
            self.record(
                node.span(),
                AsyncKind::BoxedFutureType,
                format!("type alias {} is a Future type", node.ident),
            );
        }
        syn::visit::visit_item_type(self, node);
    }

    fn visit_expr_await(&mut self, node: &'ast syn::ExprAwait) {
        self.record(
            node.span(),
            AsyncKind::AwaitOrAsyncBlock,
            ".await expression",
        );
        syn::visit::visit_expr_await(self, node);
    }

    fn visit_expr_async(&mut self, node: &'ast syn::ExprAsync) {
        self.record(node.span(), AsyncKind::AwaitOrAsyncBlock, "async block");
        syn::visit::visit_expr_async(self, node);
    }

    fn visit_expr_closure(&mut self, node: &'ast syn::ExprClosure) {
        // An `async ||` closure is an async producer even with no `.await` in its
        // body (the await hook only catches the `.await`-ing case).
        if node.asyncness.is_some() {
            self.record(node.span(), AsyncKind::AwaitOrAsyncBlock, "async closure");
        }
        syn::visit::visit_expr_closure(self, node);
    }
}

/// True for `impl Future<..>` / `impl Stream<..>` (and `impl ... + Future`).
fn type_is_impl_future(ty: &syn::Type) -> bool {
    let syn::Type::ImplTrait(impl_trait) = ty else {
        return false;
    };
    impl_trait.bounds.iter().any(bound_is_future_like)
}

fn bound_is_future_like(bound: &syn::TypeParamBound) -> bool {
    let syn::TypeParamBound::Trait(trait_bound) = bound else {
        return false;
    };
    trait_bound
        .path
        .segments
        .last()
        .is_some_and(|seg| seg.ident == "Future" || seg.ident == "Stream")
}

/// True for a type whose syntax tree mentions `Future` or `Stream` inside a
/// `Pin`/`Box`/`dyn` shape — i.e. a boxed/pinned future like
/// `Pin<Box<dyn Future<Output = T>>>` or `Box<dyn Stream<..>>` — OR a NAMED
/// `futures`-crate alias of one (`BoxFuture` / `LocalBoxFuture` / `BoxStream` /
/// `LocalBoxStream`), which expands to exactly that shape but carries none of the
/// `Pin`/`Box`/`Future` idents in its own syntax tree (so the structural probe alone
/// would be blind to it — the §5 D11 "boxed-future alias" evasion).
fn type_is_boxed_future(ty: &syn::Type) -> bool {
    let mut visitor = BoxedFutureProbe::default();
    visitor.visit_type(ty);
    (visitor.has_pin_or_box && visitor.has_future) || visitor.has_named_box_future
}

/// The `futures`/`futures-util` named boxed-future aliases. Each is definitionally
/// `Pin<Box<dyn Future/Stream + ..>>`, so a `-> BoxFuture<..>` return or a
/// `type Cb = BoxFuture<..>` alias is a boxed future the structural shape-probe
/// cannot see by `Pin`/`Box`/`Future` idents alone.
const NAMED_BOX_FUTURE_ALIASES: &[&str] =
    &["BoxFuture", "LocalBoxFuture", "BoxStream", "LocalBoxStream"];

#[derive(Default)]
struct BoxedFutureProbe {
    has_pin_or_box: bool,
    has_future: bool,
    has_named_box_future: bool,
}

impl<'ast> Visit<'ast> for BoxedFutureProbe {
    fn visit_path_segment(&mut self, seg: &'ast syn::PathSegment) {
        let ident = &seg.ident;
        if ident == "Pin" || ident == "Box" {
            self.has_pin_or_box = true;
        }
        if ident == "Future" || ident == "Stream" {
            self.has_future = true;
        }
        if NAMED_BOX_FUTURE_ALIASES.iter().any(|alias| ident == alias) {
            self.has_named_box_future = true;
        }
        syn::visit::visit_path_segment(self, seg);
    }
}

/// True when `attrs` carries `#[cfg(test)]`.
fn module_is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("cfg") {
            return false;
        }
        let mut found = false;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("test") {
                found = true;
            }
            Ok(())
        });
        found
    })
}

#[cfg(test)]
#[path = "store_sync_gate_tests.rs"]
mod store_sync_gate_tests;
