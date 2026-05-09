//! Compile-fail: same `event = X` appears twice — each payload may bind to exactly one handler.

use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 8, type_id = 1)]
struct Ping {}

#[derive(Default, serde::Serialize, serde::Deserialize, EventSourced)]
#[batpak(input = JsonValueInput)]
#[batpak(event = Ping, handler = first)]
#[batpak(event = Ping, handler = second)]
struct State {}

impl State {
    fn first(&mut self, _p: Ping) {}
    fn second(&mut self, _p: Ping) {}
}

fn main() {}
