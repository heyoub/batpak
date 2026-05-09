//! Compile-fail: #[derive(EventPayload)] rejects generic payload enums before shape-specific checks.

use batpak::EventPayload;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 1, type_id = 1)]
enum GenericEnumPayload<T> {
    Value(T),
}

fn main() {}
