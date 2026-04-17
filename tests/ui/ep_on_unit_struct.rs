//! Compile-fail: #[derive(EventPayload)] on a unit struct.

use batpak::EventPayload;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 1, type_id = 1)]
struct Unit;

fn main() {}
