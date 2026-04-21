// justifies: INV-TEST-PANIC-AS-ASSERTION; this public-input cleanup harness uses panic! as a deliberate assertion style for exact variant checking.
#![allow(clippy::panic)]
//! PROVES: public runtime input that used to rely on `expect()` now surfaces
//! structured errors: `EventKind::try_custom` rejects invalid kind namespaces
//! without panicking, and negative custom-clock input returns `StoreError`
//! instead of tearing down append/batch paths.
//! CATCHES: panic-only validation drift on `EventKind` construction or custom
//! clock execution paths that would otherwise crash the writer on public input.
//! SEEDED: deterministic / no randomness.

use batpak::prelude::*;
use std::sync::Arc;

#[test]
fn event_kind_try_custom_rejects_invalid_public_input() {
    let valid = EventKind::try_custom(0xF, 0x123).expect("valid custom kind");
    assert_eq!(valid, EventKind::custom(0xF, 0x123));
    assert_eq!(valid.category(), 0xF);
    assert_eq!(valid.type_id(), 0x123);

    assert!(matches!(
        EventKind::try_custom(0x10, 1),
        Err(batpak::event::kind::EventKindError::CategoryOutOfRange { category: 0x10 })
    ));
    assert!(matches!(
        EventKind::try_custom(0x0, 1),
        Err(batpak::event::kind::EventKindError::ReservedSystemCategory)
    ));
    assert!(matches!(
        EventKind::try_custom(0xD, 1),
        Err(batpak::event::kind::EventKindError::ReservedEffectCategory)
    ));
    assert!(matches!(
        EventKind::try_custom(0xF, 0x1000),
        Err(batpak::event::kind::EventKindError::TypeIdOutOfRange { type_id: 0x1000 })
    ));
}

#[test]
fn negative_custom_clock_surfaces_store_error_in_append_and_batch_paths() {
    let coord = Coordinate::new("entity:negative-clock", "scope:test").expect("coord");

    let append_dir = tempfile::tempdir().expect("append temp dir");
    let append_clock: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(|| -1);
    let append_store =
        Store::open(StoreConfig::new(append_dir.path()).with_clock(Some(append_clock)))
            .expect("open append store");

    let append_err = match append_store.append(
        &coord,
        EventKind::DATA,
        &serde_json::json!({"append": true}),
    ) {
        Ok(_) => panic!("append should reject a negative custom clock"),
        Err(err) => err,
    };
    assert!(
        matches!(
            append_err,
            StoreError::InvalidClock {
                timestamp_us: -1,
                ..
            }
        ),
        "negative custom clock must surface StoreError::InvalidClock on append, got {append_err:?}"
    );
    assert!(
        append_store.query(&Region::all()).is_empty(),
        "append failure from invalid clock must not publish any visible event"
    );
    append_store.close().expect("close append store");

    let batch_dir = tempfile::tempdir().expect("batch temp dir");
    let batch_clock: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(|| -1);
    let batch_store = Store::open(StoreConfig::new(batch_dir.path()).with_clock(Some(batch_clock)))
        .expect("open batch store");

    let batch_err = match batch_store.append_batch(vec![BatchAppendItem::new(
        coord.clone(),
        EventKind::DATA,
        &serde_json::json!({"batch": true}),
        AppendOptions::default(),
        CausationRef::None,
    )
    .expect("batch item")])
    {
        Ok(_) => panic!("batch append should reject a negative custom clock"),
        Err(err) => err,
    };

    match batch_err {
        StoreError::BatchFailed { item_index, source } => {
            assert_eq!(item_index, 0, "negative clock should fail the first batch item envelope");
            assert!(
                matches!(
                    *source,
                    StoreError::InvalidClock {
                        timestamp_us: -1,
                        ..
                    }
                ),
                "negative custom clock must surface InvalidClock inside BatchFailed, got {source:?}"
            );
        }
        other => panic!(
            "negative custom clock must surface BatchFailed{{source=InvalidClock}} on batch append, got {other:?}"
        ),
    }

    assert!(
        batch_store.query(&Region::all()).is_empty(),
        "batch failure from invalid clock must not publish any visible event"
    );
    batch_store.close().expect("close batch store");
}
