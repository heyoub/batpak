//! Compile-fail: handler takes the wrong payload type. The `const _: fn()`
//! pointer-cast in the derived output forces a span-pointed error on the
//! handler method itself, not somewhere deep in the dispatch body.

use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 8, type_id = 1)]
struct Ping {}

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 8, type_id = 2)]
struct Pong {}

#[derive(Default, serde::Serialize, serde::Deserialize, EventSourced)]
#[batpak(input = JsonValueInput)]
#[batpak(event = Ping, handler = on_wrong)]
struct State {}

impl State {
    // Wrong signature: derive declared `event = Ping` but handler takes `Pong`.
    fn on_wrong(&mut self, _p: Pong) {}
}

fn main() {}
