//! Compile-fail: more than one #[batpak(...)] attribute on the same struct.

use batpak::EventPayload;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 1, type_id = 1)]
#[batpak(category = 2, type_id = 2)]
struct MultiAttr {
    value: u64,
}

fn main() {}
