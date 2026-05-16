//! Compile-fail: type_id exceeds the 12-bit field (> 0xFFF).

use batpak::EventPayload;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 1, type_id = 4096)]
struct OverflowType {
    value: u64,
}

fn main() {}
