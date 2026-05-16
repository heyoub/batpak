#![allow(clippy::panic)]

use std::sync::Arc;

use batpak::event::EventKind;
use batpak::prelude::*;
use batpak::store::{Store, StoreConfig};
use syncbat::{
    rebuild_register_from_store, EffectClass, OperationDescriptor, Register,
    RegisterOperationPutV1, StoreRegisterCatalog, StoreRegisterCatalogError,
};

const ALPHA: OperationDescriptor = OperationDescriptor::new(
    "alpha",
    EffectClass::Inspect,
    "schema.alpha.input.v1",
    "schema.alpha.output.v1",
    "receipt.alpha.v1",
);

const BRAVO: OperationDescriptor = OperationDescriptor::new(
    "bravo",
    EffectClass::Emit,
    "schema.bravo.input.v1",
    "schema.bravo.output.v1",
    "receipt.bravo.v1",
);

#[derive(serde::Serialize, serde::Deserialize)]
struct OtherRow {
    name: String,
}

impl EventPayload for OtherRow {
    const KIND: EventKind = syncbat::SYNCBAT_REGISTER_EVENT_KIND;
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

fn register_coord() -> Coordinate {
    Coordinate::new("syncbat:register", "scope:catalog").expect("register coordinate")
}

fn other_coord() -> Coordinate {
    Coordinate::new("syncbat:register-other", "scope:catalog").expect("other coordinate")
}

fn close_store(store: Arc<Store>) {
    let store = match Arc::try_unwrap(store) {
        Ok(store) => store,
        Err(_) => panic!("expected test to release all Store references before close"),
    };
    store.close().expect("close store");
}

#[test]
fn store_register_catalog_persists_and_rebuilds_deterministic_register() {
    let (store, _dir) = test_store();
    let register = Register::from_operations([BRAVO.clone(), ALPHA.clone()]).expect("register");
    let catalog = StoreRegisterCatalog::new(Arc::clone(&store), register_coord());

    let receipts = catalog
        .persist_register(&register)
        .expect("persist register");
    let rebuilt = rebuild_register_from_store(store.as_ref(), &register_coord()).expect("rebuild");

    assert_eq!(receipts.len(), 2);
    assert_eq!(rebuilt.names().collect::<Vec<_>>(), vec!["alpha", "bravo"]);
    assert_eq!(rebuilt.descriptor("alpha"), Some(&ALPHA));
    assert_eq!(rebuilt.descriptor("bravo"), Some(&BRAVO));

    drop(catalog);
    close_store(store);
}

#[test]
fn rebuilt_register_survives_store_reopen() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )
    .expect("open store");
    let store = Arc::new(store);
    let catalog = StoreRegisterCatalog::new(Arc::clone(&store), register_coord());
    catalog.persist_operation(&ALPHA).expect("persist alpha");
    drop(catalog);
    close_store(store);

    let reopened = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )
    .expect("reopen store");
    let rebuilt = rebuild_register_from_store(&reopened, &register_coord()).expect("rebuild");

    assert_eq!(rebuilt.descriptor("alpha"), Some(&ALPHA));
    reopened.close().expect("close reopened store");
}

#[test]
fn rebuild_filters_exact_coordinate_and_ignores_identical_duplicates() {
    let (store, _dir) = test_store();
    let catalog = StoreRegisterCatalog::new(Arc::clone(&store), register_coord());
    let other = StoreRegisterCatalog::new(Arc::clone(&store), other_coord());
    catalog.persist_operation(&ALPHA).expect("persist alpha");
    catalog
        .persist_operation(&ALPHA)
        .expect("persist duplicate alpha");
    other
        .persist_operation(&BRAVO)
        .expect("persist other bravo");

    let rebuilt = rebuild_register_from_store(store.as_ref(), &register_coord()).expect("rebuild");

    assert_eq!(rebuilt.len(), 1);
    assert_eq!(rebuilt.descriptor("alpha"), Some(&ALPHA));
    assert!(!rebuilt.contains_operation("bravo"));

    drop(catalog);
    drop(other);
    close_store(store);
}

#[test]
fn rebuild_rejects_conflicting_duplicate_name() {
    let (store, _dir) = test_store();
    let catalog = StoreRegisterCatalog::new(Arc::clone(&store), register_coord());
    let conflict = OperationDescriptor::new(
        "alpha",
        EffectClass::Compute,
        "schema.alpha.input.v2",
        "schema.alpha.output.v1",
        "receipt.alpha.v1",
    );
    catalog.persist_operation(&ALPHA).expect("persist alpha");
    catalog
        .persist_operation(&conflict)
        .expect("persist conflict");

    let err = match rebuild_register_from_store(store.as_ref(), &register_coord()) {
        Ok(_) => panic!("expected conflict"),
        Err(error) => error,
    };

    assert!(matches!(
        err,
        StoreRegisterCatalogError::CatalogConflict { ref name } if name == "alpha"
    ));

    drop(catalog);
    close_store(store);
}

#[test]
fn rebuild_rejects_malformed_catalog_payload() {
    let (store, _dir) = test_store();
    store
        .append_typed(
            &register_coord(),
            &OtherRow {
                name: "not a register row".to_owned(),
            },
        )
        .expect("append malformed row");

    let err = match rebuild_register_from_store(store.as_ref(), &register_coord()) {
        Ok(_) => panic!("expected decode failure"),
        Err(error) => error,
    };

    assert!(matches!(err, StoreRegisterCatalogError::Decode(_)));
    close_store(store);
}

#[test]
fn register_row_round_trips_descriptor_fields() {
    let descriptor = OperationDescriptor::owned(
        "owned.alpha",
        EffectClass::Persist,
        "schema.owned.alpha.input.v1",
        "schema.owned.alpha.output.v1",
        "receipt.owned.alpha.v1",
    )
    .with_owned_title("Owned Alpha");

    let row = RegisterOperationPutV1::from_descriptor(&descriptor);

    assert_eq!(row.schema_version, 1);
    assert_eq!(row.action, "put");
    assert_eq!(row.name, "owned.alpha");
    assert_eq!(row.title.as_deref(), Some("Owned Alpha"));
    assert_eq!(row.effect, "persist");
}
