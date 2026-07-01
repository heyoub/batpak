//! RED fixture for item #133 (opt-in `startup-registry-check` / ctor path).
//!
//! Registers TWO colliding `EventPayload` kinds and has an effectively-empty
//! `main`. This crate depends on batpak with `features = ["startup-registry-check"]`,
//! so the library installs one process-wide `#[ctor::ctor]` constructor that runs
//! `verify_registry()` BEFORE `main`, writes a diagnostic to stderr, and aborts
//! on the collision.
//!
//! Because `main` would exit 0 if it were ever reached, a non-zero / aborting
//! exit PROVES the constructor fired before `main`. If the constructor failed to
//! run, `main` prints the `REACHED_MAIN_WITHOUT_ABORT` sentinel so the driver can
//! distinguish that failure from a correct abort.

use std::io::Write;
use std::process::ExitCode;

const COLLIDING_CATEGORY: u8 = 0xE;
const COLLIDING_TYPE_ID: u16 = 0x654;
const COLLIDING_KIND_BITS: u16 = ((COLLIDING_CATEGORY as u16) << 12) | COLLIDING_TYPE_ID;

batpak::__private::inventory::submit! {
    batpak::__private::EventPayloadRegistration {
        kind_bits: COLLIDING_KIND_BITS,
        payload_version: 1,
        type_name: "registry_startup_ctor::FirstColliding",
    }
}

batpak::__private::inventory::submit! {
    batpak::__private::EventPayloadRegistration {
        kind_bits: COLLIDING_KIND_BITS,
        payload_version: 1,
        type_name: "registry_startup_ctor::SecondColliding",
    }
}

fn main() -> ExitCode {
    // Reaching this line means the startup constructor did NOT fire, which is the
    // bug this fixture guards against. Emit a sentinel so the driver sees it.
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(b"REACHED_MAIN_WITHOUT_ABORT\n");
    let _ = stdout.flush();
    ExitCode::SUCCESS
}
