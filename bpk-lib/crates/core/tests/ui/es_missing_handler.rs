//! Compile-fail: `handler = does_not_exist` references a method that isn't defined.

use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 8, type_id = 1)]
struct Ping {}

#[derive(Default, serde::Serialize, serde::Deserialize, EventSourced)]
#[batpak(input = JsonValueInput)]
#[batpak(event = Ping, handler = does_not_exist)]
struct State {}

fn main() {}
