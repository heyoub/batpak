//! Compile-fail: only one of the two required keys is present.

use batpak::EventPayload;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 1)]
struct OnlyCategory {
    value: u64,
}

fn main() {}
