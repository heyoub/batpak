//! Compile-fail: category is reserved (0x0).

use batpak::EventPayload;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0, type_id = 1)]
struct ReservedCat {
    value: u64,
}

fn main() {}
