//! PROVES: the open-time upcast-chain completeness scan is NON-vacuous — it
//! passes (default `Store::open` succeeds) when the linked registry is healthy.
//! A `#[batpak(version = 2)]` kind WITH a registered `1 -> 2` step has a
//! complete chain and opens fine, and a plain `version = 1` kind has no chain
//! obligation and opens fine.
//! CATCHES: an over-eager scan that refuses to open a correctly-versioned store
//! (false positive), or one that ignores registered steps entirely.
//! SEEDED: deterministic / no randomness.
//!
//! Kept in its OWN test binary: the link-time registries are binary-global, so
//! a clean default open can only be witnessed where NO incomplete chain is
//! linked (its sibling fixture seeds the incomplete-chain failure case).

use batpak::event::{Upcast, UpcastError};
use batpak::register_upcast;
use batpak_testkit::prelude::*;
use serde::{Deserialize, Serialize};

/// A `version = 2` payload WITH a registered `1 -> 2` step: a complete chain.
#[derive(Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xE, type_id = 0x7B3, version = 2)]
struct CompleteV2 {
    total: u64,
}

/// The `1 -> 2` migration. The open-time scan only checks the hop is REGISTERED,
/// so an identity transform is sufficient to make the chain complete.
struct CompleteV1ToV2;

impl Upcast for CompleteV1ToV2 {
    const KIND: EventKind = CompleteV2::KIND;
    const FROM_VERSION: u16 = 1;

    fn upcast(value: rmpv::Value) -> Result<rmpv::Value, UpcastError> {
        Ok(value)
    }
}

register_upcast!(CompleteV1ToV2);

/// A plain `version = 1` payload: no upcast obligation, must never be flagged.
#[derive(Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xE, type_id = 0x7B4)]
struct PlainV1 {
    note: u64,
}

#[test]
fn default_open_succeeds_when_every_versioned_chain_is_complete() {
    // Precondition (non-vacuous): the v2 kind's `1 -> 2` step is actually linked
    // via inventory, and the plain kind really is version 1. If either were
    // false the "opens fine" result below would not prove the scan tolerated a
    // genuinely-complete registry.
    let steps = batpak::__private::upcast_steps_for(CompleteV2::KIND.as_raw_u16());
    assert!(
        steps.iter().any(|step| step.from_version == 1),
        "PROPERTY: register_upcast! must link the CompleteV2 1->2 step via inventory"
    );
    assert_eq!(
        PlainV1::PAYLOAD_VERSION,
        1,
        "PROPERTY: PlainV1 must be a version-1 kind (no chain obligation)"
    );

    // With only complete / v1 kinds linked, the binary-wide scan finds no gap.
    validate_upcast_chain_registry()
        .expect("PROPERTY: a registry with only complete chains and v1 kinds must validate clean");
    // Forcing a fresh re-scan must reach the same clean verdict.
    revalidate_upcast_chain_registry()
        .expect("PROPERTY: re-scan of a healthy registry must also validate clean");

    let dir = tempfile::tempdir().expect("temp dir");
    // Default config (FailFast). The healthy registry must let the store open.
    let store = Store::open(StoreConfig::new(dir.path()))
        .expect("default-policy open must succeed when every versioned chain is complete");
    store.close().expect("close store");
}
