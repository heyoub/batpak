//! Compile-fail: `cache_version` must be an integer literal parseable as u64.
//! (Projection cache invalidation key — not a payload schema field.)

use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 8, type_id = 1)]
struct Ping {}

#[derive(Default, serde::Serialize, serde::Deserialize, EventSourced)]
#[batpak(input = JsonValueInput, cache_version = "not-a-number")]
#[batpak(event = Ping, handler = on_ping)]
struct State {}

impl State {
    fn on_ping(&mut self, _p: Ping) {}
}

fn main() {}
