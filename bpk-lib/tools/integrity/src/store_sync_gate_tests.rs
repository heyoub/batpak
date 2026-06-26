//! D11 STORE_SYNC_ONLY red fixtures — each of the six evasions the OLD
//! `contents.contains("async fn")` grep missed must BITE. Collect-and-assert
//! (no `panic!` even in tests). Four are AST-structural (over `scan_file`); two
//! are dep-graph (a renamed-tokio and a target-specific async runtime in the
//! store's production graph, over the shared scanner). The green half proves the
//! live store tree is sync and that flume (recv_async) is not flagged.

use super::{scan_file, AsyncKind};

fn scan(src: &str) -> Vec<super::AsyncViolation> {
    let file = syn::parse_file(src).expect("fixture parses as Rust");
    scan_file(&file)
}

fn has_kind(src: &str, kind: AsyncKind) -> bool {
    scan(src).iter().any(|v| v.kind == kind)
}

/// THE D11 RED FIXTURE (GateNegativePath): every async evasion bites. Named by
/// the gate registry. Asserts each planted shape is flagged by `scan_file` (the
/// 4 AST shapes) and by the shared dep-graph scanner (the 2 dep shapes).
#[test]
fn every_async_store_evasion_is_rejected() {
    let mut failures: Vec<String> = Vec::new();

    // (1) public async method — the only shape the old grep caught.
    if !has_kind(
        "struct Store; impl Store { pub async fn append(&self) {} }",
        AsyncKind::AsyncFn,
    ) {
        failures.push("public async method not flagged".into());
    }

    // (2) impl-Future return — NO literal `async fn`, grep blind.
    if !has_kind(
        "use std::future::Future; fn query() -> impl Future<Output = ()> { async {} }",
        AsyncKind::ImplFutureReturn,
    ) {
        failures.push("impl-Future return not flagged".into());
    }

    // (3) boxed-future return type — grep blind.
    if !has_kind(
        "use std::{pin::Pin, future::Future}; \
         fn boxed() -> Pin<Box<dyn Future<Output = ()>>> { Box::pin(async {}) }",
        AsyncKind::BoxedFutureType,
    ) {
        failures.push("boxed-future return not flagged".into());
    }

    // (3b) boxed-future TYPE ALIAS — grep blind.
    if !has_kind(
        "use std::{pin::Pin, future::Future}; \
         type Fut = Pin<Box<dyn Future<Output = ()>>>;",
        AsyncKind::BoxedFutureType,
    ) {
        failures.push("boxed-future type alias not flagged".into());
    }

    // (3c) NAMED `futures::BoxFuture` alias return — the §5 D11 "boxed-future alias"
    //      evasion: carries NO Pin/Box/Future idents in its own tree, so the
    //      structural shape-probe alone is blind to it.
    if !has_kind(
        "use futures::future::BoxFuture; \
         fn boxed() -> BoxFuture<'static, ()> { Box::pin(async {}) }",
        AsyncKind::BoxedFutureType,
    ) {
        failures.push("named BoxFuture-alias return not flagged".into());
    }

    // (3d) NAMED `BoxFuture` TYPE ALIAS.
    if !has_kind(
        "use futures::future::LocalBoxFuture; type Cb = LocalBoxFuture<'static, ()>;",
        AsyncKind::BoxedFutureType,
    ) {
        failures.push("named BoxFuture type alias not flagged".into());
    }

    // (4) async-trait impl — grep blind (the attribute expands to boxed futures).
    if !has_kind(
        "#[async_trait] impl Subscriber for Store { async fn on_event(&self) {} }",
        AsyncKind::AsyncTraitAttr,
    ) {
        failures.push("async_trait impl not flagged".into());
    }

    // (4c) a no-`.await` `async ||` closure — an async producer the `.await` hook
    //      alone would miss.
    if !has_kind(
        "fn make() { let _f = async || { 1 }; }",
        AsyncKind::AwaitOrAsyncBlock,
    ) {
        failures.push("no-await async closure not flagged".into());
    }

    // (4b) a stray `.await` inside an otherwise-sync fn body — grep blind.
    if !has_kind(
        "fn drive(f: impl core::future::Future) { let _ = async { f.await }; }",
        AsyncKind::AwaitOrAsyncBlock,
    ) {
        failures.push(".await / async block not flagged".into());
    }

    // (5) renamed-tokio in the store's production graph (dep-graph half).
    let renamed = r#"{
      "packages": [
        {"id":"ws+batpak@0.1.0","name":"batpak"},
        {"id":"registry+x#tokio@1.0.0","name":"tokio"}
      ],
      "workspace_members": ["ws+batpak@0.1.0"],
      "resolve": {"root": null, "nodes": [
        {"id":"ws+batpak@0.1.0","deps":[
          {"name":"store_rt","pkg":"registry+x#tokio@1.0.0","dep_kinds":[{"kind":null,"target":null}]}
        ]},
        {"id":"registry+x#tokio@1.0.0","deps":[]}
      ]}
    }"#;
    assert_dep_rejected(renamed, "renamed-tokio under store", &mut failures);

    // (6) target-specific async runtime in the store's production graph.
    let target_specific = r#"{
      "packages": [
        {"id":"ws+batpak@0.1.0","name":"batpak"},
        {"id":"registry+x#async-std@1.0.0","name":"async-std"}
      ],
      "workspace_members": ["ws+batpak@0.1.0"],
      "resolve": {"root": null, "nodes": [
        {"id":"ws+batpak@0.1.0","deps":[
          {"name":"async-std","pkg":"registry+x#async-std@1.0.0","dep_kinds":[{"kind":null,"target":"cfg(unix)"}]}
        ]},
        {"id":"registry+x#async-std@1.0.0","deps":[]}
      ]}
    }"#;
    assert_dep_rejected(
        target_specific,
        "target-specific async-runtime under store",
        &mut failures,
    );

    assert!(failures.is_empty(), "{failures:?}");
}

fn assert_dep_rejected(json: &str, label: &str, failures: &mut Vec<String>) {
    use crate::no_runtime_gate::scanner::{model_from_metadata_json, scan_resolved_graph};
    match model_from_metadata_json(json, &["batpak"]) {
        Ok((nodes, _roots)) => {
            if scan_resolved_graph(&nodes, &["batpak"]).is_empty() {
                failures.push(format!("{label} NOT flagged by the store dep-graph scan"));
            }
        }
        Err(err) => failures.push(format!("{label} fixture failed to model: {err}")),
    }
}

/// The cfg(test) module surface is NOT scanned (test helpers may be async).
#[test]
fn cfg_test_modules_are_excluded() {
    let src = "#[cfg(test)] mod tests { async fn helper() {} }";
    assert!(
        scan(src).is_empty(),
        "a #[cfg(test)] async helper must not be flagged: {:?}",
        scan(src)
    );
}

/// flume's `recv_async()` is a plain sync METHOD CALL, not an `.await`, so a
/// store fn using it must scan clean — flume is the sanctioned escape hatch.
#[test]
fn flume_recv_async_method_call_is_not_an_await() {
    let src = "fn pump(rx: flume::Receiver<u8>) { let _ = rx.recv_async(); }";
    assert!(
        scan(src).is_empty(),
        "flume recv_async() is a sync call, must not be flagged: {:?}",
        scan(src)
    );
}

/// GREEN PATH over the LIVE tree: the real production store code is sync today.
#[test]
fn real_store_tree_is_sync() {
    use crate::repo_surface::repo_root;
    use crate::source_cache::SourceCache;
    let repo = repo_root().expect("repo root resolves");
    let mut cache = SourceCache::new(&repo);
    super::check(&repo, &mut cache).expect("the live production store code must be sync");
}
