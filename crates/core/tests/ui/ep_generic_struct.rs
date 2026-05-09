//! Compile-fail: #[derive(EventPayload)] rejects generic payload types.

use batpak::EventPayload;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 1, type_id = 1)]
struct GenericPayload<T> {
    value: T,
}

fn main() {}
