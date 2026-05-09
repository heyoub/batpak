//! Compile-fail: #[derive(EventSourced)] on an enum (only named-field structs allowed).

use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 8, type_id = 1)]
struct Ping {}

#[derive(serde::Serialize, serde::Deserialize, EventSourced)]
#[batpak(input = JsonValueInput)]
#[batpak(event = Ping, handler = on_ping)]
enum NotAStruct {
    A,
    B,
}

fn main() {}
