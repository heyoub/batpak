//! Compile-fail: `event = crate::Foo` uses a multi-segment path.
//!
//! The derive rejects multi-segment paths so that stringified comparison can
//! deduplicate event bindings without running full path resolution. The qualified
//! form would otherwise alias `Foo` and bypass the duplicate-event check.

use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 8, type_id = 1)]
struct Foo {}

#[derive(Default, serde::Serialize, serde::Deserialize, EventSourced)]
#[batpak(input = JsonValueInput)]
#[batpak(event = Foo, handler = a)]
#[batpak(event = crate::Foo, handler = b)]
struct State {}

impl State {
    fn a(&mut self, _p: &Foo) {}
    fn b(&mut self, _p: &Foo) {}
}

fn main() {}
