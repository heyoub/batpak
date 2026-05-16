//! Compile-fail: #[derive(EventSourced)] on a tuple struct.

use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 8, type_id = 1)]
struct Ping {}

#[derive(Default, serde::Serialize, serde::Deserialize, EventSourced)]
#[batpak(input = JsonValueInput)]
#[batpak(event = Ping, handler = on_ping)]
struct Tuple(u64);

fn main() {}
