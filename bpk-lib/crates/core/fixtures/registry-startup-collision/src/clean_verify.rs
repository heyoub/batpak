//! GREEN control for the item #133 RED fixture.
//!
//! Same shape as `collide_verify`, but the two registrations use DISTINCT
//! `(category, type_id)` pairs, so `verify_registry()` returns `Ok` and `main`
//! exits 0. The driver asserts this exit-0 to prove the sibling `collide_verify`
//! exit-1 is caused by the seeded collision and not by the harness or a broken
//! entry point (this is how RED is confirmed).

use std::io::Write;
use std::process::ExitCode;

const CATEGORY: u8 = 0xE;
const KIND_A: u16 = ((CATEGORY as u16) << 12) | 0x331;
const KIND_B: u16 = ((CATEGORY as u16) << 12) | 0x332;

batpak::__private::inventory::submit! {
    batpak::__private::EventPayloadRegistration {
        kind_bits: KIND_A,
        payload_version: 1,
        type_name: "registry_startup_clean::FirstDistinct",
    }
}

batpak::__private::inventory::submit! {
    batpak::__private::EventPayloadRegistration {
        kind_bits: KIND_B,
        payload_version: 1,
        type_name: "registry_startup_clean::SecondDistinct",
    }
}

fn main() -> ExitCode {
    match batpak::event::verify_registry() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Never expected: a clean registry must verify cleanly.
            let message = format!("registry-startup-clean UNEXPECTED collision: {error}\n");
            let mut stderr = std::io::stderr();
            let _ = stderr.write_all(message.as_bytes());
            let _ = stderr.flush();
            ExitCode::from(2)
        }
    }
}
