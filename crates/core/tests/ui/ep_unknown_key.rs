//! Compile-fail: unknown key inside #[batpak(...)].

use batpak::EventPayload;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 1, type_id = 1, extra = 2)]
struct HasExtraKey {
    value: u64,
}

fn main() {}
