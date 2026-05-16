#![allow(clippy::panic)]

use std::sync::Arc;

use batpak::prelude::*;
use batpak::store::{AppendOptions, ExtensionKey, Store, StoreConfig};
use syncbat::{
    receipt_extension_key, receipt_extension_value, Core, Cx, EffectClass, Handler, HandlerResult,
    OperationDescriptor, ReceiptEnvelope, ReceiptOutcome, StoreReceiptSink,
    SYNCBAT_RECEIPT_EVENT_KIND,
};

const PING: OperationDescriptor = OperationDescriptor::new(
    "ping",
    EffectClass::Inspect,
    "schema.ping.input.v1",
    "schema.ping.output.v1",
    "receipt.ping.v1",
);

struct EchoHandler;

impl Handler for EchoHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut Cx<'_>) -> HandlerResult {
        Ok(input.to_vec())
    }
}

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

fn syncbat_key(field: &str) -> ExtensionKey {
    receipt_extension_key(field)
        .expect("syncbat extension key")
        .as_key()
        .clone()
}

fn close_store(store: Arc<Store>) {
    let store = match Arc::try_unwrap(store) {
        Ok(store) => store,
        Err(_) => panic!("expected test to release all Store references before close"),
    };
    store.close().expect("close store");
}

#[test]
fn store_receipt_sink_persists_envelope_and_signed_extensions() {
    let (store, _dir) = test_store();
    let input_hash = [1_u8; 32];
    let output_hash = [2_u8; 32];
    let envelope = ReceiptEnvelope::from_descriptor(
        "repo.patch",
        "receipt.repo_patch.v1",
        ReceiptOutcome::Completed,
    )
    .with_input_hash(input_hash)
    .with_output_hash(output_hash)
    .with_signed_extension("kit.ref", b"abc".to_vec())
    .with_local_extension("local.note", b"not signed".to_vec());
    let sink = StoreReceiptSink::new(Arc::clone(&store), receipt_coord()).with_options(
        AppendOptions::new().with_receipt_extension(
            receipt_extension_key("descriptor").expect("descriptor key"),
            receipt_extension_value(b"stale descriptor".to_vec()),
        ),
    );

    let recorded = sink.record_typed(&envelope).expect("record receipt");
    let fields = recorded
        .batpak_receipt
        .clone()
        .expect("batpak receipt fields");

    assert_eq!(recorded.envelope, envelope);
    assert_eq!(
        fields.extensions.get(&syncbat_key("descriptor")),
        Some(&b"repo.patch".to_vec())
    );
    assert_eq!(
        fields.extensions.get(&syncbat_key("kind")),
        Some(&b"receipt.repo_patch.v1".to_vec())
    );
    assert_eq!(
        fields.extensions.get(&syncbat_key("outcome")),
        Some(&b"completed".to_vec())
    );
    assert_eq!(
        fields.extensions.get(&syncbat_key("input")),
        Some(&input_hash.to_vec())
    );
    assert_eq!(
        fields.extensions.get(&syncbat_key("output")),
        Some(&output_hash.to_vec())
    );
    let signed_drawer = fields
        .extensions
        .get(&syncbat_key("signed"))
        .expect("signed drawer extension");
    assert!(signed_drawer.starts_with(b"syncbat.drawer.v1\0"));
    assert!(!fields.extensions.contains_key(&syncbat_key("local")));

    let hits = store.query(&Region::entity("syncbat:receipt"));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].event_id, fields.event_id);
    assert_eq!(hits[0].global_sequence, fields.sequence);
    assert_eq!(hits[0].disk_pos, fields.disk_pos);
    assert_eq!(hits[0].hash_chain.event_hash, fields.content_hash);
    assert_eq!(hits[0].receipt_extensions, fields.extensions);
    assert_eq!(hits[0].kind, SYNCBAT_RECEIPT_EVENT_KIND);

    let stored = store.get(fields.event_id).expect("stored receipt event");
    assert_eq!(stored.coordinate, receipt_coord());
    assert_eq!(stored.event.header.event_kind, SYNCBAT_RECEIPT_EVENT_KIND);
    assert_eq!(stored.event.header.content_hash, fields.content_hash);

    drop(sink);
    close_store(store);
}

#[test]
fn core_with_store_receipt_sink_banks_success_receipt_once() {
    let (store, _dir) = test_store();
    let sink = StoreReceiptSink::new(Arc::clone(&store), receipt_coord());
    let mut builder = Core::builder();
    builder.receipt_sink(sink);
    builder.register(PING, EchoHandler).expect("register");
    let mut core = builder.build().expect("core builds");

    let result = core.invoke("ping", b"hi".to_vec()).expect("invoke");

    assert_eq!(result.output(), b"hi");
    let recorded = result.recorded_receipt().expect("receipt recorded");
    assert_eq!(recorded.envelope.descriptor_name, "ping");
    assert_eq!(recorded.envelope.receipt_kind, "receipt.ping.v1");
    assert_eq!(recorded.envelope.outcome, ReceiptOutcome::Completed);
    let fields = recorded.batpak_receipt.as_ref().expect("batpak fields");
    assert_eq!(
        fields.extensions.get(&syncbat_key("descriptor")),
        Some(&b"ping".to_vec())
    );
    assert_eq!(
        fields.extensions.get(&syncbat_key("outcome")),
        Some(&b"completed".to_vec())
    );

    let hits = store.query(&Region::entity("syncbat:receipt"));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].event_id, fields.event_id);
    assert_eq!(hits[0].receipt_extensions, fields.extensions);

    drop(core);
    close_store(store);
}
