//! RED fixture for item #133 (A4, always-on path).
//!
//! A non-test binary that registers TWO `EventPayload` kinds claiming the same
//! `(category, type_id)` and NEVER opens a `Store`. Because this is a `[[bin]]`
//! target, `cfg(test)` is false, so the `#[derive(EventPayload)]` per-type
//! collision test (which is `#[cfg(test)]`-only) is absent here exactly as it is
//! in a release binary. `main` calls the portable `verify_registry()` entry
//! point and fails the process on the collision, proving a release binary can
//! catch a linked-kind collision it otherwise would not see.
//!
//! The colliding registrations are emitted directly via `inventory::submit!`
//! (rather than `#[derive(EventPayload)]`) so this binary carries a real
//! link-time collision without also pulling in the derive's generated
//! `#[cfg(test)]` collision test.

use std::io::Write;
use std::process::ExitCode;

const COLLIDING_CATEGORY: u8 = 0xE;
const COLLIDING_TYPE_ID: u16 = 0x321;
const COLLIDING_KIND_BITS: u16 = ((COLLIDING_CATEGORY as u16) << 12) | COLLIDING_TYPE_ID;

batpak::__private::inventory::submit! {
    batpak::__private::EventPayloadRegistration {
        kind_bits: COLLIDING_KIND_BITS,
        payload_version: 1,
        type_name: "registry_startup_collision::FirstColliding",
    }
}

batpak::__private::inventory::submit! {
    batpak::__private::EventPayloadRegistration {
        kind_bits: COLLIDING_KIND_BITS,
        payload_version: 1,
        type_name: "registry_startup_collision::SecondColliding",
    }
}

// `std::process::exit` is a repo-banned method (LAW-001: it skips Drop). Returning
// `ExitCode` propagates a non-zero status cleanly instead.
fn main() -> ExitCode {
    match batpak::event::verify_registry() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let message = format!("registry-startup-collision: {error}\n");
            let mut stderr = std::io::stderr();
            let _ = stderr.write_all(message.as_bytes());
            let _ = stderr.flush();
            ExitCode::from(1)
        }
    }
}
