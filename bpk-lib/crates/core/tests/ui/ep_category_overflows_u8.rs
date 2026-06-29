//! Compile-fail: category literal exceeds the u8 range that the derive's
//! bounded-cast guard accepts (event_payload.rs `category_u64 > u8::MAX`).
//!
//! Distinct from `ep_invalid_category.rs` (category = 0, the reserved-value
//! guard): here the literal is `256`, which is rejected BEFORE the narrowing
//! `u8::try_from` so the cast can never silently truncate `0x100` to `0x00`.

use batpak::EventPayload;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 256, type_id = 1)]
struct CategoryOverflows {
    value: u64,
}

fn main() {}
