use crate::event::EventKind;
use serde::de::DeserializeOwned;
use serde::Serialize;

/// Binds a Rust type to a batpak [`EventKind`] at compile time.
///
/// Implement this trait on any named-field struct that represents a discrete event
/// in your domain. The `KIND` constant is used by the typed append/query surfaces
/// (`append_typed`, `by_fact_typed`, etc.) so callers never hand-write
/// `EventKind::custom(...)` at callsites.
///
/// # Example
///
/// ```rust
/// use batpak::prelude::EventKind;
/// use batpak::event::EventPayload;
///
/// #[derive(serde::Serialize, serde::Deserialize)]
/// struct ThingHappened {
///     amount: u64,
/// }
///
/// impl EventPayload for ThingHappened {
///     const KIND: EventKind = EventKind::custom(0x1, 1);
/// }
/// ```
pub trait EventPayload: Serialize + DeserializeOwned {
    /// The [`EventKind`] this payload type is bound to.
    ///
    /// Must be unique within a binary. The `#[derive(EventPayload)]` macro (when
    /// available) enforces this with a per-binary collision test.
    const KIND: EventKind;
}
