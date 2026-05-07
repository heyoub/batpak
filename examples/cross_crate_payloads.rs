//! # cross_crate_payloads
//!
//! **Teaches:** validating typed payload kind allocation when an application
//! composes payload types from multiple crates.
//!
//! Run:
//! `cargo run --example cross_crate_payloads`

use batpak::prelude::*;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0x120)]
struct LocalPayload {
    value: u64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // In a real application, call this once during startup after linking all
    // payload crates. `Store::open` also warns once per process by default when
    // duplicate kind registrations are linked.
    validate_event_payload_registry()?;

    let dir = tempfile::tempdir()?;
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_event_payload_validation(EventPayloadValidation::FailFast),
    )?;
    let coord = Coordinate::new("entity:example", "scope:payloads")?;
    let receipt = store.append_typed(&coord, &LocalPayload { value: 1 })?;
    let stored = store.get(receipt.event_id)?;
    assert_eq!(stored.event.event_kind(), LocalPayload::KIND);
    store.close()?;
    Ok(())
}
