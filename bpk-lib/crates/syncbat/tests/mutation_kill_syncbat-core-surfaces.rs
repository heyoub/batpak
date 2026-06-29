//! PROVES: durable operation-status writes go through the store, not a no-op.
//! CATCHES: the diff-scoped surviving mutant on
//! `StoreOperationStatusSink::record_started -> Ok(())`, which would silently
//! drop the status append while still reporting success.
//! SEEDED: a tempfile-backed batpak store and a fixed operation name.

use std::sync::Arc;

use batpak::prelude::*;
use batpak::store::{Store, StoreConfig};
use syncbat::{operation_status_entity, StoreOperationStatusSink};

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

#[test]
fn record_started_appends_exactly_one_started_fact() {
    let (store, _dir) = test_store();
    let sink = StoreOperationStatusSink::new(Arc::clone(&store));

    sink.record_started("ping", "receipt.ping.v1")
        .expect("record_started should append");

    let entity = operation_status_entity("ping").expect("status entity");
    let hits = store.query(&Region::entity(entity.as_str()));
    assert_eq!(
        hits.len(),
        1,
        "record_started must append one fact through the store"
    );
}
