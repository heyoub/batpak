//! Compile-fail: `input = NotAMarker` references a type that does not implement ProjectionInput.

use batpak::prelude::*;

struct NotAMarker;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 8, type_id = 1)]
struct Ping {}

#[derive(Default, serde::Serialize, serde::Deserialize, EventSourced)]
#[batpak(input = NotAMarker)]
#[batpak(event = Ping, handler = on_ping)]
struct State {}

impl State {
    fn on_ping(&mut self, _p: Ping) {}
}

fn main() {}
