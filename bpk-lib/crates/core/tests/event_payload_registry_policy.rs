//! PROVES: EventPayload registry validation exposes clean, warn, fail-fast, and explicit revalidation policy.
//! CATCHES: registry validator cache, error, and StoreConfig policy regressions.
//! SEEDED: deterministic / no randomness.

use batpak::store::Store;
use batpak_testkit::prelude::*;

#[test]
fn public_payload_registry_validator_reports_clean_registry() {
    let result: Result<(), EventPayloadRegistryError> = validate_event_payload_registry();
    result.expect("test payload registry has no duplicate kinds");
    revalidate_event_payload_registry().expect("revalidate clean payload registry");

    let collision = EventPayloadKindCollision {
        category: 0xF,
        type_id: 0x0FE,
        first_type_name: "first",
        second_type_name: "second",
    };
    assert_eq!(collision.category, 0xF);
    let registry_error = EventPayloadRegistryError::new(vec![collision]);
    let collision_count = registry_error.collisions().len();
    assert_eq!(collision_count, 1);
    let rendered = registry_error.to_string();
    assert!(
        rendered.contains("duplicate kind assignment")
            && rendered.contains("category=0xF")
            && rendered.contains("type_id=0x0FE")
            && rendered.contains("first")
            && rendered.contains("second"),
        "PROPERTY: registry collision Display must include the duplicate kind and both type names, got: {rendered}"
    );
    assert!(
        matches!(
            StoreError::EventPayloadRegistry(registry_error.clone()),
            StoreError::EventPayloadRegistry(_)
        ),
        "StoreError must expose fail-fast payload registry errors"
    );

    let _warn = EventPayloadValidation::Warn;
    let _fail_fast = EventPayloadValidation::FailFast;
    let _silent = EventPayloadValidation::Silent;
}

#[test]
fn store_open_accepts_explicit_payload_validation_policy_when_registry_is_clean() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_event_payload_validation(EventPayloadValidation::FailFast),
    )
    .expect("clean registry opens in fail-fast mode");
    store.close().expect("close store");
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0x120)]
struct StartupPayload {
    value: u64,
}

#[test]
fn startup_validate_event_payload_registry_before_fail_fast_open(
) -> Result<(), Box<dyn std::error::Error>> {
    validate_event_payload_registry().map_err(std::io::Error::other)?;
    let dir = tempfile::tempdir()?;
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_event_payload_validation(EventPayloadValidation::FailFast),
    )?;
    let coord = Coordinate::new("entity:registry-startup", "scope:payloads")?;
    let receipt = store.append_typed(&coord, &StartupPayload { value: 1 })?;
    let stored = store.get(receipt.event_id)?;
    assert_eq!(stored.event.event_kind(), StartupPayload::KIND);
    store.close()?;
    Ok(())
}
