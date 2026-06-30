//! PROVES: `Store::open` defaults to `EventPayloadValidation::FailFast`, so a
//! binary whose linked payload registry carries a `(category, type_id)`
//! collision REFUSES to open unless the caller explicitly opts back into the
//! looser `Warn` (log-and-proceed) policy.
//! CATCHES: a regression that re-defaults the payload-registry policy to `Warn`,
//! which would let two payload types silently share wire identity.
//! SEEDED: deterministic / no randomness.

use batpak_testkit::prelude::*;

// Two payload registrations that claim the SAME `(category, type_id)`. This is
// the minimal colliding registration: emitting the `inventory::submit!` items
// directly (rather than via `#[derive(EventPayload)]`) gives this binary a real
// link-time collision WITHOUT also pulling in the derive's generated
// `#[cfg(test)]` collision panic-test. Category `0xE` is a caller-defined
// category not used elsewhere in this binary, so this pair is the only
// collision the registry scanner can see here.
const COLLIDING_CATEGORY: u8 = 0xE;
const COLLIDING_TYPE_ID: u16 = 0x654;
const COLLIDING_KIND_BITS: u16 = ((COLLIDING_CATEGORY as u16) << 12) | COLLIDING_TYPE_ID;

batpak::__private::inventory::submit! {
    batpak::__private::EventPayloadRegistration {
        kind_bits: COLLIDING_KIND_BITS,
        payload_version: 1,
        type_name: "event_payload_collision_default_fail_fast::FirstColliding",
    }
}

batpak::__private::inventory::submit! {
    batpak::__private::EventPayloadRegistration {
        kind_bits: COLLIDING_KIND_BITS,
        payload_version: 1,
        type_name: "event_payload_collision_default_fail_fast::SecondColliding",
    }
}

fn registry_sees_the_seeded_collision() -> bool {
    let Err(error) = validate_event_payload_registry() else {
        return false;
    };
    error.collisions().iter().any(|collision| {
        collision.category == COLLIDING_CATEGORY && collision.type_id == COLLIDING_TYPE_ID
    })
}

#[test]
fn default_policy_fail_fast_refuses_open_on_kind_collision() {
    // Precondition: the seeded collision is actually linked and visible to the
    // binary-wide registry scanner (otherwise the assertion below could pass
    // vacuously even if the default were still Warn).
    assert!(
        registry_sees_the_seeded_collision(),
        "PROPERTY: the two seeded colliding registrations must be visible to the binary-wide payload registry"
    );

    let dir = tempfile::tempdir().expect("temp dir");
    // Default config: no `.with_event_payload_validation(...)`. The default is
    // now `FailFast`, so the colliding registry must make the open FAIL with the
    // registry error that names the seeded collision. `Store` is not `Debug`, so
    // inspect only the error half of the result.
    let open_error = Store::open(StoreConfig::new(dir.path())).err();
    assert!(
        matches!(
            open_error.as_ref(),
            Some(StoreError::EventPayloadRegistry(registry_error))
                if registry_error.collisions().iter().any(|collision| {
                    collision.category == COLLIDING_CATEGORY
                        && collision.type_id == COLLIDING_TYPE_ID
                })
        ),
        "PROPERTY: default-policy open must fail with StoreError::EventPayloadRegistry naming the seeded ({COLLIDING_CATEGORY:#X}, {COLLIDING_TYPE_ID:#X}) collision, got {open_error:?}"
    );
}

#[test]
fn explicit_warn_opt_in_still_opens_on_kind_collision() {
    assert!(
        registry_sees_the_seeded_collision(),
        "PROPERTY: the two seeded colliding registrations must be visible to the binary-wide payload registry"
    );

    let dir = tempfile::tempdir().expect("temp dir");
    // The loose log-and-proceed behavior stays reachable as an explicit opt-out:
    // requesting `Warn` must still open despite the same colliding registry.
    let store = Store::open(
        StoreConfig::new(dir.path()).with_event_payload_validation(EventPayloadValidation::Warn),
    )
    .expect("explicit Warn opt-in opens despite the colliding registry");
    store.close().expect("close store");
}
