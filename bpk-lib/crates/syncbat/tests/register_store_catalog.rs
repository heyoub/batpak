//! PROVES: INV-SYNCBAT-REGISTER-CATALOG-DETERMINISTIC
//! CATCHES: malformed catalog rows, invalid lifecycle transitions, tombstone reuse, and nondeterministic rebuilds.
//! SEEDED: tempfile-backed batpak stores with fixed operation descriptors.

use std::sync::Arc;

use batpak::event::EventKind;
use batpak::prelude::*;
use batpak::store::{Store, StoreConfig};
use syncbat::register_store::RegisterOperationActionV1;
use syncbat::{
    rebuild_register_from_store, EffectClass, OperationDescriptor, Register,
    RegisterOperationRowV1, StoreRegisterCatalog, StoreRegisterCatalogError,
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

const ALPHA_V2: OperationDescriptor = OperationDescriptor::new(
    "alpha",
    EffectClass::Compute,
    "schema.alpha.input.v2",
    "schema.alpha.output.v1",
    "receipt.alpha.v1",
);

const CHARLIE: OperationDescriptor = OperationDescriptor::new(
    "charlie",
    EffectClass::Control,
    "schema.charlie.input.v1",
    "schema.charlie.output.v1",
    "receipt.charlie.v1",
);

const CHARLIE_V2: OperationDescriptor = OperationDescriptor::new(
    "charlie",
    EffectClass::Persist,
    "schema.charlie.input.v2",
    "schema.charlie.output.v1",
    "receipt.charlie.v1",
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
    let store = Arc::try_unwrap(store)
        .map_err(|_| ())
        .expect("expected test to release all Store references before close");
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
    let _ = catalog.persist_operation(&ALPHA).expect("persist alpha");
    let _ = catalog.persist_operation(&BRAVO).expect("persist bravo");
    let _ = catalog.delete_operation("alpha").expect("delete alpha");
    drop(catalog);
    close_store(store);

    let reopened = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )
    .expect("reopen store");
    let rebuilt = rebuild_register_from_store(&reopened, &register_coord()).expect("rebuild");

    assert!(!rebuilt.contains_operation("alpha"));
    assert_eq!(rebuilt.descriptor("bravo"), Some(&BRAVO));
    reopened.close().expect("close reopened store");
}

#[test]
fn rebuild_filters_exact_coordinate_and_ignores_identical_duplicates() {
    let (store, _dir) = test_store();
    let catalog = StoreRegisterCatalog::new(Arc::clone(&store), register_coord());
    let other = StoreRegisterCatalog::new(Arc::clone(&store), other_coord());
    let _ = catalog.persist_operation(&ALPHA).expect("persist alpha");
    let _ = catalog
        .persist_operation(&ALPHA)
        .expect("persist duplicate alpha");
    let _ = other
        .persist_operation(&BRAVO)
        .expect("persist other bravo");
    let _ = other.delete_operation("bravo").expect("delete other bravo");

    let rebuilt = rebuild_register_from_store(store.as_ref(), &register_coord()).expect("rebuild");

    assert_eq!(rebuilt.len(), 1);
    assert_eq!(rebuilt.descriptor("alpha"), Some(&ALPHA));
    assert!(!rebuilt.contains_operation("bravo"));

    drop(catalog);
    drop(other);
    close_store(store);
}

#[test]
fn rebuild_applies_explicit_update_for_active_operation() {
    let (store, _dir) = test_store();
    let catalog = StoreRegisterCatalog::new(Arc::clone(&store), register_coord());
    let _ = catalog.persist_operation(&ALPHA).expect("persist alpha");
    let _ = catalog.update_operation(&ALPHA_V2).expect("update alpha");

    let rebuilt = rebuild_register_from_store(store.as_ref(), &register_coord()).expect("rebuild");

    assert_eq!(rebuilt.len(), 1);
    assert_eq!(rebuilt.descriptor("alpha"), Some(&ALPHA_V2));

    drop(catalog);
    close_store(store);
}

#[test]
fn persist_operation_rejects_implicit_replacement_put() {
    let (store, _dir) = test_store();
    let catalog = StoreRegisterCatalog::new(Arc::clone(&store), register_coord());
    let _ = catalog.persist_operation(&ALPHA).expect("persist alpha");

    let err = catalog
        .persist_operation(&ALPHA_V2)
        .map(|_| ())
        .expect_err("expected implicit put replacement rejection");

    assert!(matches!(
        err,
        StoreRegisterCatalogError::CatalogConflict {
            ref name,
            ref action,
            ..
        } if name == "alpha" && action == RegisterOperationActionV1::Put.as_str()
    ));

    drop(catalog);
    close_store(store);
}

#[test]
fn update_operation_rejects_missing_operation() {
    let (store, _dir) = test_store();
    let catalog = StoreRegisterCatalog::new(Arc::clone(&store), register_coord());

    let err = catalog
        .update_operation(&ALPHA_V2)
        .map(|_| ())
        .expect_err("expected update-before-put rejection");

    assert!(matches!(
        err,
        StoreRegisterCatalogError::CatalogConflict {
            ref name,
            ref action,
            ..
        } if name == "alpha" && action == RegisterOperationActionV1::Update.as_str()
    ));

    drop(catalog);
    close_store(store);
}

#[test]
fn rebuild_applies_delete_tombstone_and_idempotent_duplicate() {
    let (store, _dir) = test_store();
    let catalog = StoreRegisterCatalog::new(Arc::clone(&store), register_coord());
    let _ = catalog.persist_operation(&ALPHA).expect("persist alpha");
    let _ = catalog.delete_operation("alpha").expect("delete alpha");
    let _ = store
        .append_typed(&register_coord(), &RegisterOperationRowV1::delete("alpha"))
        .expect("append duplicate delete row");

    let rebuilt = rebuild_register_from_store(store.as_ref(), &register_coord()).expect("rebuild");

    assert!(rebuilt.is_empty());
    assert!(!rebuilt.contains_operation("alpha"));

    drop(catalog);
    close_store(store);
}

#[test]
fn rebuild_rejects_put_after_tombstone() {
    let (store, _dir) = test_store();
    let catalog = StoreRegisterCatalog::new(Arc::clone(&store), register_coord());
    let _ = catalog.persist_operation(&ALPHA).expect("persist alpha");
    let _ = catalog.delete_operation("alpha").expect("delete alpha");
    let _ = store
        .append_typed(
            &register_coord(),
            &RegisterOperationRowV1::from_descriptor(&ALPHA),
        )
        .expect("append put after tombstone");

    let err = rebuild_register_from_store(store.as_ref(), &register_coord())
        .map(|_| ())
        .expect_err("expected tombstone conflict");

    assert!(matches!(
        err,
        StoreRegisterCatalogError::CatalogConflict {
            ref name,
            ref action,
            ..
        } if name == "alpha" && action == RegisterOperationActionV1::Put.as_str()
    ));

    drop(catalog);
    close_store(store);
}

#[test]
fn rebuild_rejects_put_row_with_supersedes_field() {
    let (store, _dir) = test_store();
    let mut row = RegisterOperationRowV1::from_descriptor(&ALPHA);
    row.supersedes = Some("old.alpha".to_owned());
    let _ = store
        .append_typed(&register_coord(), &row)
        .expect("append malformed put");

    let err = rebuild_register_from_store(store.as_ref(), &register_coord())
        .map(|_| ())
        .expect_err("expected malformed put row");

    assert!(matches!(
        err,
        StoreRegisterCatalogError::InvalidLifecycleRow {
            ref name,
            ref action,
            ..
        } if name == "alpha" && action == RegisterOperationActionV1::Put.as_str()
    ));
    close_store(store);
}

#[test]
fn rebuild_rejects_delete_before_put() {
    let (store, _dir) = test_store();
    let _ = store
        .append_typed(&register_coord(), &RegisterOperationRowV1::delete("alpha"))
        .expect("append delete before put");

    let err = rebuild_register_from_store(store.as_ref(), &register_coord())
        .map(|_| ())
        .expect_err("expected delete-before-put conflict");

    assert!(matches!(
        err,
        StoreRegisterCatalogError::CatalogConflict {
            ref name,
            ref action,
            ..
        } if name == "alpha" && action == RegisterOperationActionV1::Delete.as_str()
    ));

    close_store(store);
}

#[test]
fn rebuild_applies_supersession_and_idempotent_duplicate() {
    let (store, _dir) = test_store();
    let catalog = StoreRegisterCatalog::new(Arc::clone(&store), register_coord());
    let _ = catalog.persist_operation(&ALPHA).expect("persist alpha");
    let _ = catalog
        .supersede_operation("alpha", &CHARLIE)
        .expect("supersede alpha");
    let _ = store
        .append_typed(
            &register_coord(),
            &RegisterOperationRowV1::supersede("alpha", &CHARLIE),
        )
        .expect("append duplicate supersede row");

    let rebuilt = rebuild_register_from_store(store.as_ref(), &register_coord()).expect("rebuild");

    assert_eq!(rebuilt.len(), 1);
    assert!(!rebuilt.contains_operation("alpha"));
    assert_eq!(rebuilt.descriptor("charlie"), Some(&CHARLIE));

    drop(catalog);
    close_store(store);
}

#[test]
fn delete_operation_rejects_after_supersession() {
    let (store, _dir) = test_store();
    let catalog = StoreRegisterCatalog::new(Arc::clone(&store), register_coord());
    let _ = catalog.persist_operation(&ALPHA).expect("persist alpha");
    let _ = catalog
        .supersede_operation("alpha", &CHARLIE)
        .expect("supersede alpha");
    let err = catalog
        .delete_operation("alpha")
        .map(|_| ())
        .expect_err("expected delete-after-supersede rejection");

    assert!(matches!(
        err,
        StoreRegisterCatalogError::CatalogConflict {
            ref name,
            ref action,
            ..
        } if name == "alpha" && action == RegisterOperationActionV1::Delete.as_str()
    ));

    drop(catalog);
    close_store(store);
}

#[test]
fn rebuild_rejects_supersession_from_missing_source() {
    let (store, _dir) = test_store();
    let catalog = StoreRegisterCatalog::new(Arc::clone(&store), register_coord());

    let err = catalog
        .supersede_operation("alpha", &CHARLIE)
        .map(|_| ())
        .expect_err("expected missing-source conflict");

    assert!(matches!(
        err,
        StoreRegisterCatalogError::CatalogConflict {
            ref name,
            ref action,
            ..
        } if name == "alpha" && action == RegisterOperationActionV1::Supersede.as_str()
    ));

    drop(catalog);
    close_store(store);
}

#[test]
fn rebuild_rejects_supersession_after_delete_without_matching_replacement() {
    let (store, _dir) = test_store();
    let catalog = StoreRegisterCatalog::new(Arc::clone(&store), register_coord());
    let _ = catalog.persist_operation(&ALPHA).expect("persist alpha");
    let _ = catalog.delete_operation("alpha").expect("delete alpha");

    let err = catalog
        .supersede_operation("alpha", &CHARLIE)
        .map(|_| ())
        .expect_err("expected supersede-after-delete conflict");

    assert!(matches!(
        err,
        StoreRegisterCatalogError::CatalogConflict {
            ref name,
            ref action,
            ..
        } if name == "alpha" && action == RegisterOperationActionV1::Supersede.as_str()
    ));

    drop(catalog);
    close_store(store);
}

#[test]
fn rebuild_rejects_supersede_row_missing_supersedes_name() {
    let (store, _dir) = test_store();
    let mut row = RegisterOperationRowV1::from_descriptor(&CHARLIE);
    row.action = RegisterOperationActionV1::Supersede.as_str().to_owned();
    let _ = store
        .append_typed(&register_coord(), &row)
        .expect("append malformed supersede");

    let err = rebuild_register_from_store(store.as_ref(), &register_coord())
        .map(|_| ())
        .expect_err("expected malformed supersede row");

    assert!(matches!(
        err,
        StoreRegisterCatalogError::InvalidLifecycleRow {
            ref name,
            ref action,
            ..
        } if name == "charlie" && action == RegisterOperationActionV1::Supersede.as_str()
    ));
    close_store(store);
}

#[test]
fn rebuild_rejects_same_name_supersede_row() {
    let (store, _dir) = test_store();
    let row = RegisterOperationRowV1::supersede("alpha", &ALPHA);
    let _ = store
        .append_typed(&register_coord(), &row)
        .expect("append malformed same-name supersede");

    let err = rebuild_register_from_store(store.as_ref(), &register_coord())
        .map(|_| ())
        .expect_err("expected same-name supersede rejection");

    assert!(matches!(
        err,
        StoreRegisterCatalogError::InvalidLifecycleRow {
            ref name,
            ref action,
            ..
        } if name == "alpha" && action == RegisterOperationActionV1::Supersede.as_str()
    ));
    close_store(store);
}

#[test]
fn supersede_operation_rejects_tombstoned_replacement_name() {
    let (store, _dir) = test_store();
    let catalog = StoreRegisterCatalog::new(Arc::clone(&store), register_coord());
    let _ = catalog
        .persist_operation(&CHARLIE)
        .expect("persist charlie");
    let _ = catalog.delete_operation("charlie").expect("delete charlie");
    let _ = catalog.persist_operation(&ALPHA).expect("persist alpha");

    let err = catalog
        .supersede_operation("alpha", &CHARLIE)
        .map(|_| ())
        .expect_err("expected tombstoned replacement conflict");

    assert!(matches!(
        err,
        StoreRegisterCatalogError::CatalogConflict {
            ref name,
            ref action,
            ..
        } if name == "charlie" && action == RegisterOperationActionV1::Supersede.as_str()
    ));

    drop(catalog);
    close_store(store);
}

#[test]
fn supersede_operation_rejects_active_replacement_with_different_fields() {
    let (store, _dir) = test_store();
    let catalog = StoreRegisterCatalog::new(Arc::clone(&store), register_coord());
    let _ = catalog.persist_operation(&ALPHA).expect("persist alpha");
    let _ = catalog
        .persist_operation(&CHARLIE)
        .expect("persist charlie");

    let err = catalog
        .supersede_operation("alpha", &CHARLIE_V2)
        .map(|_| ())
        .expect_err("expected replacement conflict");

    assert!(matches!(
        err,
        StoreRegisterCatalogError::CatalogConflict {
            ref name,
            ref action,
            ..
        } if name == "charlie" && action == RegisterOperationActionV1::Supersede.as_str()
    ));

    drop(catalog);
    close_store(store);
}

#[test]
fn rebuild_rejects_malformed_catalog_payload() {
    let (store, _dir) = test_store();
    let _ = store
        .append_typed(
            &register_coord(),
            &OtherRow {
                name: "not a register row".to_owned(),
            },
        )
        .expect("append malformed row");

    let err = rebuild_register_from_store(store.as_ref(), &register_coord())
        .map(|_| ())
        .expect_err("expected decode failure");

    assert!(matches!(err, StoreRegisterCatalogError::Decode(_)));
    close_store(store);
}

#[test]
fn rebuild_rejects_malformed_lifecycle_row_shape() {
    let (store, _dir) = test_store();
    let mut malformed_delete = RegisterOperationRowV1::from_descriptor(&ALPHA);
    malformed_delete.action = RegisterOperationActionV1::Delete.as_str().to_owned();
    let _ = store
        .append_typed(&register_coord(), &malformed_delete)
        .expect("append malformed delete");

    let err = rebuild_register_from_store(store.as_ref(), &register_coord())
        .map(|_| ())
        .expect_err("expected malformed lifecycle row");

    assert!(matches!(
        err,
        StoreRegisterCatalogError::InvalidLifecycleRow {
            ref name,
            ref action,
            ..
        } if name == "alpha" && action == RegisterOperationActionV1::Delete.as_str()
    ));
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

    let row = RegisterOperationRowV1::from_descriptor(&descriptor);

    assert_eq!(row.schema_version, 1);
    assert_eq!(row.action, "put");
    assert_eq!(row.name, "owned.alpha");
    assert_eq!(row.supersedes, None);
    assert_eq!(row.title.as_deref(), Some("Owned Alpha"));
    assert_eq!(row.effect, "persist");

    let update = RegisterOperationRowV1::update(&descriptor);
    assert_eq!(update.action, RegisterOperationActionV1::Update.as_str());
    assert_eq!(update.name, "owned.alpha");
    assert!(update.supersedes.is_none());

    let delete = RegisterOperationRowV1::delete("owned.alpha");
    assert_eq!(delete.action, RegisterOperationActionV1::Delete.as_str());
    assert!(delete.supersedes.is_none());
    assert!(delete.effect.is_empty());

    let supersede = RegisterOperationRowV1::supersede("owned.alpha", &descriptor);
    assert_eq!(
        supersede.action,
        RegisterOperationActionV1::Supersede.as_str()
    );
    assert_eq!(supersede.supersedes.as_deref(), Some("owned.alpha"));
}

#[test]
fn persist_operation_is_idempotent_for_same_descriptor() {
    let (store, _dir) = test_store();
    let catalog = StoreRegisterCatalog::new(Arc::clone(&store), register_coord());
    let _ = catalog.persist_operation(&ALPHA).expect("persist alpha");
    let _ = catalog
        .persist_operation(&ALPHA)
        .expect("idempotent persist succeeds");

    let register = rebuild_register_from_store(&store, &register_coord()).expect("rebuild");
    assert_eq!(register.descriptor("alpha"), Some(&ALPHA));

    drop(catalog);
    close_store(store);
}

#[test]
fn update_operation_rejects_tombstoned_operation() {
    let (store, _dir) = test_store();
    let catalog = StoreRegisterCatalog::new(Arc::clone(&store), register_coord());
    let _ = catalog.persist_operation(&ALPHA).expect("persist alpha");
    let _ = catalog.delete_operation("alpha").expect("delete alpha");

    let err = catalog
        .update_operation(&ALPHA_V2)
        .map(|_| ())
        .expect_err("expected update on tombstoned operation rejection");

    assert!(matches!(
        err,
        StoreRegisterCatalogError::CatalogConflict {
            ref name,
            ref action,
            ..
        } if name == "alpha" && action == RegisterOperationActionV1::Update.as_str()
    ));

    drop(catalog);
    close_store(store);
}

#[test]
fn delete_operation_rejects_already_deleted_operation() {
    let (store, _dir) = test_store();
    let catalog = StoreRegisterCatalog::new(Arc::clone(&store), register_coord());
    let _ = catalog.persist_operation(&ALPHA).expect("persist alpha");
    let _ = catalog.delete_operation("alpha").expect("delete alpha");

    let err = catalog
        .delete_operation("alpha")
        .map(|_| ())
        .expect_err("expected delete-on-deleted rejection");

    assert!(matches!(
        err,
        StoreRegisterCatalogError::CatalogConflict {
            ref name,
            ref action,
            ..
        } if name == "alpha" && action == RegisterOperationActionV1::Delete.as_str()
    ));

    drop(catalog);
    close_store(store);
}

#[test]
fn supersede_operation_rejects_idempotent_duplicate() {
    let (store, _dir) = test_store();
    let catalog = StoreRegisterCatalog::new(Arc::clone(&store), register_coord());
    let _ = catalog.persist_operation(&ALPHA).expect("persist alpha");
    let _ = catalog
        .supersede_operation("alpha", &CHARLIE)
        .expect("supersede alpha");

    let err = catalog
        .supersede_operation("alpha", &CHARLIE)
        .map(|_| ())
        .expect_err("expected duplicate supersede rejection");

    assert!(matches!(
        err,
        StoreRegisterCatalogError::CatalogConflict {
            ref name,
            ref action,
            ..
        } if name == "alpha" && action == RegisterOperationActionV1::Supersede.as_str()
    ));

    drop(catalog);
    close_store(store);
}
