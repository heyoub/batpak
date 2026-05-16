//! Compile-fail: #[derive(EventPayload)] without the required #[batpak(...)] attribute.

use batpak::EventPayload;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
struct NoAttr {
    value: u64,
}

fn main() {}
