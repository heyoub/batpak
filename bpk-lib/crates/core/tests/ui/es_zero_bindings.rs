//! Compile-fail: `#[derive(EventSourced)]` with `input =` but no event-binding attrs.
//!
//! A projection with zero bindings has nothing to dispatch on; the derive must
//! reject it up front rather than producing a dispatcher that routes to nothing.

use batpak::prelude::*;

#[derive(Default, serde::Serialize, serde::Deserialize, EventSourced)]
#[batpak(input = JsonValueInput)]
struct State {
    value: i64,
}

fn main() {}
