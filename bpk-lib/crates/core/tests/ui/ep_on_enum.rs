//! Compile-fail: #[derive(EventPayload)] on an enum (only named-field structs allowed).

use batpak::EventPayload;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 1, type_id = 1)]
enum NotAStruct {
    A,
    B,
}

fn main() {}
