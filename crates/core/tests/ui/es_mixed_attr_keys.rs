//! Compile-fail: a single `#[batpak(...)]` attribute cannot mix config keys
//! (`input`, `cache_version`) with event-binding keys (`event`, `handler`).

use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 8, type_id = 1)]
struct Ping {}

#[derive(Default, serde::Serialize, serde::Deserialize, EventSourced)]
#[batpak(input = JsonValueInput, event = Ping, handler = on_ping)]
struct State {}

impl State {
    fn on_ping(&mut self, _p: Ping) {}
}

fn main() {}
