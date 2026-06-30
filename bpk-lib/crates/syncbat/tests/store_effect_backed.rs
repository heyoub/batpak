//! PROVES: the store-backed effect backend backs the read axes end to end.
//! CATCHES: regressing `StoreEffectBackend::read_event` / `query_projection` to
//!          the trait's "not supported" stubs, which would make every Inspect op
//!          that declares an event read or projection query un-invokable; and
//!          a backend silently flipping the host-layer axes (`emit_receipt`,
//!          `use_host_control`) away from their typed fail-closed defaults.
//! SEEDED: tempfile-backed batpak stores and fixed operation descriptors.
//!
//! RED-BEFORE: before `StoreEffectBackend` overrode `read_event`/`query_projection`,
//! both fell through to `EffectBackend`'s default `EffectError` stubs, so the two
//! `*_succeeds_end_to_end` tests below failed (the handler mapped the stub error
//! to a `RuntimeError::Handler`). The `default_*_axis_stays_fail_closed` tests pin
//! that the unbacked default is still exactly that "not supported" error, i.e. the
//! shape these axes had before being wired — and the shape the FLAGGED axes keep.

use std::sync::Arc;

use batpak::event::EventKind;
use batpak::prelude::*;
use batpak::store::{Store, StoreConfig};
use syncbat::{
    Core, Ctx, EffectBackend, EffectClass, EffectError, Handler, HandlerError, HandlerResult,
    OperationDescriptor, OperationEffectRow, ReceiptEnvelope, ReceiptSink, ReceiptSinkError,
    RecordedReceipt, RuntimeError, StoreEffectBackend,
};

const KIND: EventKind = EventKind::custom(0xF, 1);
const EVENT_CATEGORY: &str = "cat.inventory.v1";
const PROJECTION_ID: &str = "proj.orders.v1";
const RECEIPT_KIND: &str = "receipt.audit.v1";
const SCHEMA_IN: &str = "schema.audit.input.v1";
const SCHEMA_OUT: &str = "schema.audit.output.v1";

fn test_store() -> (Arc<Store>, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )
    .expect("open store");
    (Arc::new(store), dir)
}

fn coord() -> Coordinate {
    Coordinate::new("audit:stream", "scope:test").expect("coordinate")
}

fn close_store(store: Arc<Store>) {
    Store::sync(store.as_ref()).expect("sync store before close");
    let store = Arc::try_unwrap(store)
        .map_err(|_| ())
        .expect("expected test to release all Store references before close");
    drop(store);
}

fn read_descriptor() -> OperationDescriptor {
    OperationDescriptor::new(
        "audit.read",
        EffectClass::Inspect,
        SCHEMA_IN,
        SCHEMA_OUT,
        RECEIPT_KIND,
    )
    .with_effect_row(OperationEffectRow::new().reads_event(EVENT_CATEGORY))
}

fn projection_descriptor() -> OperationDescriptor {
    OperationDescriptor::new(
        "audit.projection",
        EffectClass::Inspect,
        SCHEMA_IN,
        SCHEMA_OUT,
        RECEIPT_KIND,
    )
    .with_effect_row(OperationEffectRow::new().queries_projection(PROJECTION_ID))
}

struct ReadHandler;

impl Handler for ReadHandler {
    fn handle(&mut self, input: &[u8], cx: &mut Ctx<'_>) -> HandlerResult {
        cx.event_read_handle()
            .read_event(EVENT_CATEGORY)
            .map_err(|error| HandlerError::failed(error.message().to_owned()))?;
        Ok(input.to_vec())
    }
}

struct ProjectionHandler;

impl Handler for ProjectionHandler {
    fn handle(&mut self, input: &[u8], cx: &mut Ctx<'_>) -> HandlerResult {
        cx.projection_read_handle()
            .query_projection(PROJECTION_ID)
            .map_err(|error| HandlerError::failed(error.message().to_owned()))?;
        Ok(input.to_vec())
    }
}

/// A backend that only backs appends and leaves every other axis on the
/// `EffectBackend` trait defaults — the exact shape `StoreEffectBackend` had for
/// the read axes before they were wired.
struct AppendOnlyBackend;

impl EffectBackend for AppendOnlyBackend {
    fn append_event(&mut self, _kind: EventKind, _payload: &[u8]) -> Result<(), EffectError> {
        Ok(())
    }
}

/// Minimal receipt sink: the runtime requires one to bank the per-invocation
/// receipt, but these tests assert on the effect axes, not the receipt body.
struct NoopSink;

impl ReceiptSink for NoopSink {
    fn record_receipt(
        &self,
        envelope: &ReceiptEnvelope,
    ) -> Result<RecordedReceipt, ReceiptSinkError> {
        Ok(RecordedReceipt::new(envelope.clone()))
    }
}

#[test]
fn store_backed_read_event_succeeds_end_to_end() {
    let (store, _dir) = test_store();
    // Commit an event at the backend's coordinate so the mediated read resolves a
    // real index entry and reads its bytes back off disk (a non-vacuous read).
    let _seed = store
        .append(&coord(), KIND, &b"committed".to_vec())
        .expect("seed committed event");

    let backend = StoreEffectBackend::new(Arc::clone(&store), coord());
    let mut builder = Core::builder();
    builder
        .register(read_descriptor(), ReadHandler)
        .expect("register");
    builder.effect_backend(backend);
    builder.receipt_sink(NoopSink);
    let mut core = builder.build().expect("core builds");

    // RED-BEFORE: the default `read_event` stub returned `EffectError`, so this
    // invoke surfaced `RuntimeError::Handler` and `is_ok()` was false.
    let result = core
        .invoke("audit.read", b"hello".to_vec())
        .expect("store-backed read_event must succeed end to end");
    assert_eq!(result.output(), b"hello");

    drop(core);
    close_store(store);
}

#[test]
fn store_backed_query_projection_succeeds_end_to_end() {
    let (store, _dir) = test_store();
    let _seed = store
        .append(&coord(), KIND, &b"committed".to_vec())
        .expect("seed committed event");

    let backend = StoreEffectBackend::new(Arc::clone(&store), coord());
    let mut builder = Core::builder();
    builder
        .register(projection_descriptor(), ProjectionHandler)
        .expect("register");
    builder.effect_backend(backend);
    builder.receipt_sink(NoopSink);
    let mut core = builder.build().expect("core builds");

    // RED-BEFORE: the default `query_projection` stub returned `EffectError`.
    let result = core
        .invoke("audit.projection", b"hello".to_vec())
        .expect("store-backed query_projection must succeed end to end");
    assert_eq!(result.output(), b"hello");

    drop(core);
    close_store(store);
}

#[test]
fn store_backed_read_event_with_empty_stream_still_succeeds() {
    // No committed event at the coordinate: the mediated read resolves an empty
    // stream and must still succeed (a read over an empty stream is not an error).
    let (store, _dir) = test_store();
    let backend = StoreEffectBackend::new(Arc::clone(&store), coord());
    let mut builder = Core::builder();
    builder
        .register(read_descriptor(), ReadHandler)
        .expect("register");
    builder.effect_backend(backend);
    builder.receipt_sink(NoopSink);
    let mut core = builder.build().expect("core builds");

    let result = core
        .invoke("audit.read", b"hello".to_vec())
        .expect("read over an empty stream must still succeed");
    assert_eq!(result.output(), b"hello");

    drop(core);
    close_store(store);
}

#[test]
fn default_read_axis_stays_fail_closed() -> Result<(), Box<dyn std::error::Error>> {
    // Pin the unbacked default: an op whose read flows through a backend without a
    // real `read_event` impl fails closed with the typed "not supported" error.
    // This is precisely the RED state `StoreEffectBackend` left behind.
    let mut builder = Core::builder();
    builder
        .register(read_descriptor(), ReadHandler)
        .expect("register");
    builder.effect_backend(AppendOnlyBackend);
    builder.receipt_sink(NoopSink);
    let mut core = builder.build().expect("core builds");

    let error = match core.invoke("audit.read", b"hello".to_vec()) {
        Ok(_) => {
            return Err(
                std::io::Error::other("PROPERTY: an unbacked read axis must fail closed").into(),
            )
        }
        Err(error) => error,
    };
    assert!(
        matches!(
            error,
            RuntimeError::Handler { ref name, ref message, .. }
                if name == "audit.read" && message.contains("event reads are not supported")
        ),
        "unbacked read must surface the typed not-supported error; got {error:?}"
    );
    Ok(())
}

#[test]
fn default_query_axis_stays_fail_closed() -> Result<(), Box<dyn std::error::Error>> {
    let mut builder = Core::builder();
    builder
        .register(projection_descriptor(), ProjectionHandler)
        .expect("register");
    builder.effect_backend(AppendOnlyBackend);
    builder.receipt_sink(NoopSink);
    let mut core = builder.build().expect("core builds");

    let error = match core.invoke("audit.projection", b"hello".to_vec()) {
        Ok(_) => {
            return Err(
                std::io::Error::other("PROPERTY: an unbacked query axis must fail closed").into(),
            )
        }
        Err(error) => error,
    };
    assert!(
        matches!(
            error,
            RuntimeError::Handler { ref name, ref message, .. }
                if name == "audit.projection"
                    && message.contains("projection queries are not supported")
        ),
        "unbacked query must surface the typed not-supported error; got {error:?}"
    );
    Ok(())
}

#[test]
fn store_backend_keeps_host_layer_axes_fail_closed() {
    // FLAGGED axes: `StoreEffectBackend` deliberately does NOT back receipt
    // emission (a Core-level receipt-sink concern) or host control (a host-layer
    // concern). Both must keep the trait's typed fail-closed error so a store
    // backend can never silently succeed at an effect it has no authority for.
    let (store, _dir) = test_store();
    let mut backend = StoreEffectBackend::new(Arc::clone(&store), coord());

    let emit = backend
        .emit_receipt(RECEIPT_KIND)
        .expect_err("store backend must not back receipt emission");
    assert!(
        emit.message().contains("receipt emission is not supported"),
        "emit_receipt must stay fail-closed; got {emit:?}"
    );

    let host = backend
        .use_host_control()
        .expect_err("store backend must not back host control");
    assert!(
        host.message().contains("host controls are not supported"),
        "use_host_control must stay fail-closed; got {host:?}"
    );

    drop(backend);
    close_store(store);
}
