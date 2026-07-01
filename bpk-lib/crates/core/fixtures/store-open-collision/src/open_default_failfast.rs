//! RED fixture for the store-open DEFAULT payload-validation policy (companion
//! to `event_payload_registry_startup.rs`'s ctor/verify fixtures).
//!
//! A non-test binary that registers TWO `EventPayload` kinds claiming the same
//! `(category, type_id)` and opens a `Store` with the DEFAULT `StoreConfig`. The
//! default `EventPayloadValidation` is `FailFast`, so the colliding registry MUST
//! make `Store::open` fail with `StoreError::EventPayloadRegistry` naming the
//! seeded collision. The process exits 0 iff that held (the default is
//! `FailFast`) and non-zero with a stderr diagnostic otherwise — so the driver
//! test can assert the default policy WITHOUT linking the collision into its own
//! test binary (under `--all-features` the `startup-registry-check` constructor
//! aborts any binary whose own linked registry collides, before `main`).
//!
//! The colliding registrations are emitted directly via `inventory::submit!`
//! (rather than `#[derive(EventPayload)]`) so this binary carries a real
//! link-time collision without also pulling in the derive's generated
//! `#[cfg(test)]` collision test.

use std::io::Write;
use std::process::ExitCode;

use batpak::prelude::{Store, StoreConfig, StoreError};

const COLLIDING_CATEGORY: u8 = 0xE;
const COLLIDING_TYPE_ID: u16 = 0x654;
const COLLIDING_KIND_BITS: u16 = ((COLLIDING_CATEGORY as u16) << 12) | COLLIDING_TYPE_ID;

batpak::__private::inventory::submit! {
    batpak::__private::EventPayloadRegistration {
        kind_bits: COLLIDING_KIND_BITS,
        payload_version: 1,
        type_name: "store_open_collision::FirstColliding",
    }
}

batpak::__private::inventory::submit! {
    batpak::__private::EventPayloadRegistration {
        kind_bits: COLLIDING_KIND_BITS,
        payload_version: 1,
        type_name: "store_open_collision::SecondColliding",
    }
}

fn fail(message: &str, code: u8) -> ExitCode {
    let mut stderr = std::io::stderr();
    let _ = stderr.write_all(message.as_bytes());
    let _ = stderr.flush();
    ExitCode::from(code)
}

// `std::process::exit` is repo-banned (LAW-001: it skips Drop); returning
// `ExitCode` propagates the status cleanly instead.
fn main() -> ExitCode {
    let dir = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(error) => return fail(&format!("store-open-collision: tempdir failed: {error}\n"), 3),
    };

    // Default config: no `.with_event_payload_validation(...)`. The default is
    // `FailFast`, so the colliding registry must refuse the open with the
    // registry error naming the seeded collision.
    match Store::open(StoreConfig::new(dir.path())) {
        Err(StoreError::EventPayloadRegistry(registry_error))
            if registry_error.collisions().iter().any(|collision| {
                collision.category == COLLIDING_CATEGORY && collision.type_id == COLLIDING_TYPE_ID
            }) =>
        {
            ExitCode::SUCCESS
        }
        Ok(_store) => fail(
            "store-open-collision: DEFAULT open SUCCEEDED on a colliding registry; the default policy is not FailFast\n",
            1,
        ),
        Err(other) => fail(
            &format!("store-open-collision: DEFAULT open failed with an unexpected error: {other}\n"),
            2,
        ),
    }
}
