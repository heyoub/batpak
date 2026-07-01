//! PROVES: an `Emit` op's declared receipt emission carries opaque evidence into
//!         the runtime's single banked invocation receipt (Option B). The emitted
//!         payload rides the `Ctx` receipt-emit handle into the invocation's
//!         `ReceiptMetadata`, is drained into the banked receipt's LOCAL drawer,
//!         and survives round-trip through a real `StoreReceiptSink`.
//! CATCHES: regressing `ReceiptEmitHandle::emit_receipt` back to a decorative
//!          axis that mediates the emission but stamps nothing, so an
//!          `emits_receipt` declaration could contribute no evidence and the
//!          emitted payload would never reach the banked receipt.
//! SEEDED: a tempfile-backed batpak store + `StoreReceiptSink`; an emit-only
//!          effect backend that mediates the emission. `StoreEffectBackend`
//!          deliberately keeps `emit_receipt` fail-closed (a Core-level
//!          receipt-sink concern), so a dedicated emit-capable backend stands in
//!          for the host layer that mediates the emission in production.
//!
//! RED-BEFORE: the assertion decodes the PERSISTED receipt envelope back off disk
//! (`Store::read_raw`) and reads its LOCAL drawer. Drop the stamp-after-perform
//! step in `ReceiptEmitHandle::emit_receipt` (the `emit_meta.local.insert` line)
//! and the op still completes but the drawer entry is absent, so
//! `local_extensions.get(..)` is `None` and this fails. Confirmed by deleting
//! that insert locally: `banked receipt must carry the emitted payload` fails.

use std::sync::Arc;

use batpak::event::EventKind;
use batpak::prelude::*;
use batpak::store::{Store, StoreConfig};
use syncbat::{
    Core, Ctx, EffectBackend, EffectClass, EffectError, Handler, HandlerError, HandlerResult,
    OperationDescriptor, OperationEffectRow, ReceiptEnvelope, StoreReceiptSink,
};

const RECEIPT_KIND: &str = "receipt.audit.v1";
const SCHEMA_IN: &str = "schema.audit.input.v1";
const SCHEMA_OUT: &str = "schema.audit.output.v1";
const EMIT_PAYLOAD: &[u8] = b"emit-evidence-v1";

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

fn receipt_coord() -> Coordinate {
    Coordinate::new("syncbat:receipt", "scope:test").expect("receipt coordinate")
}

fn close_store(store: Arc<Store>) {
    Store::sync(store.as_ref()).expect("sync store before close");
    let store = Arc::try_unwrap(store)
        .map_err(|_| ())
        .expect("expected test to release all Store references before close");
    drop(store);
}

/// Mirror of the runtime-owned LOCAL drawer key stamped by
/// `ReceiptEmitHandle::emit_receipt` (`effect.rs`).
fn emit_local_key(receipt_kind: &str) -> String {
    format!("syncbat.emit_receipt.{receipt_kind}")
}

fn emit_descriptor() -> OperationDescriptor {
    OperationDescriptor::new(
        "audit.emit",
        EffectClass::Emit,
        SCHEMA_IN,
        SCHEMA_OUT,
        RECEIPT_KIND,
    )
    .with_effect_row(OperationEffectRow::new().emits_receipt(RECEIPT_KIND))
}

/// Backs only the receipt-emit axis so the op can complete. Appends and the
/// other axes fall through to the trait's fail-closed defaults — this op never
/// touches them.
struct EmitBackend;

impl EffectBackend for EmitBackend {
    fn append_event(&mut self, _kind: EventKind, _payload: &[u8]) -> Result<(), EffectError> {
        Err(EffectError::new("append not supported by emit backend"))
    }

    fn emit_receipt(&mut self, _receipt_kind: &str) -> Result<(), EffectError> {
        Ok(())
    }
}

struct EmitHandler;

impl Handler for EmitHandler {
    fn handle(&mut self, input: &[u8], cx: &mut Ctx<'_>) -> HandlerResult {
        cx.receipt_emit_handle()
            .emit_receipt(RECEIPT_KIND, EMIT_PAYLOAD.to_vec())
            .map_err(|error| HandlerError::failed(error.message().to_owned()))?;
        Ok(input.to_vec())
    }
}

#[test]
fn emitted_payload_reaches_the_banked_receipt_local_drawer() {
    let (store, _dir) = test_store();
    let sink = StoreReceiptSink::new(Arc::clone(&store), receipt_coord());

    let mut builder = Core::builder();
    builder
        .register(emit_descriptor(), EmitHandler)
        .expect("register");
    builder.effect_backend(EmitBackend);
    builder.receipt_sink(sink);
    let mut core = builder.build().expect("core builds");

    // (a) the declared receipt emit completes end to end.
    let result = core
        .invoke("audit.emit", b"hello".to_vec())
        .expect("declared receipt emit must complete");
    assert_eq!(result.output(), b"hello");

    // (b) the runtime banked exactly one receipt through the real store sink;
    // decode its PERSISTED envelope back off disk and confirm the emitted payload
    // landed in the LOCAL drawer under the runtime-owned key.
    let fields = result
        .recorded_receipt()
        .expect("banked receipt")
        .batpak_receipt
        .clone()
        .expect("batpak receipt fields");

    let stored = store
        .read_raw(fields.event_id)
        .expect("stored receipt event");
    let persisted: ReceiptEnvelope =
        batpak::canonical::from_bytes(&stored.event.payload).expect("decode persisted receipt");
    assert_eq!(
        persisted
            .local_extensions
            .get(&emit_local_key(RECEIPT_KIND))
            .map(Vec::as_slice),
        Some(EMIT_PAYLOAD),
        "banked receipt must carry the emitted payload in its local drawer"
    );

    drop(core);
    close_store(store);
}
