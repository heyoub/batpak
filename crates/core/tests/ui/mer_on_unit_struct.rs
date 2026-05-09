//! Compile-fail: `#[derive(MultiEventReactor)]` on a unit struct.
//!
//! Mirrors the rejection that `EventPayload` and `EventSourced` already enforce:
//! all three derives reject unit structs with the same error style.

use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 8, type_id = 1)]
struct Ping {}

#[derive(Debug)]
struct NeverFails;
impl std::fmt::Display for NeverFails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "never")
    }
}
impl std::error::Error for NeverFails {}

#[derive(Default, MultiEventReactor)]
#[batpak(input = JsonValueInput, error = NeverFails)]
#[batpak(event = Ping, handler = on_ping)]
struct Reactor;

fn main() {}
