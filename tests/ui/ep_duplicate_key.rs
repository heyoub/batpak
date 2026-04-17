//! Compile-fail: the same key appears twice inside #[batpak(...)].

use batpak::EventPayload;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 1, category = 2, type_id = 1)]
struct DupKey {
    value: u64,
}

fn main() {}
