//! GREEN companion to `open_default_failfast`: the SAME colliding registry opens
//! under an EXPLICIT `EventPayloadValidation::Warn` opt-out (log-and-proceed).
//!
//! Proves the loose policy stays reachable and — as the RED control — that the
//! sibling `open_default_failfast` exit-0 is caused by the DEFAULT being
//! `FailFast`, not by the collision being unconditionally fatal. Exits 0 iff the
//! store opened (and closed) cleanly.

use std::io::Write;
use std::process::ExitCode;

use batpak::prelude::{EventPayloadValidation, Store, StoreConfig};

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

    // The loose log-and-proceed behavior stays reachable as an explicit opt-out:
    // requesting `Warn` must still open despite the same colliding registry.
    match Store::open(
        StoreConfig::new(dir.path()).with_event_payload_validation(EventPayloadValidation::Warn),
    ) {
        Ok(store) => match store.close() {
            Ok(_closed) => ExitCode::SUCCESS,
            Err(error) => fail(&format!("store-open-collision: close failed: {error}\n"), 2),
        },
        Err(error) => fail(
            &format!(
                "store-open-collision: explicit Warn open FAILED on a colliding registry: {error}\n"
            ),
            1,
        ),
    }
}
