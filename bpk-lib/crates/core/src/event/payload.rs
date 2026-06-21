use crate::event::EventKind;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static PAYLOAD_REGISTRY_OPEN_CACHE: Mutex<Option<Result<(), EventPayloadRegistryError>>> =
    Mutex::new(None);
static PAYLOAD_REGISTRY_WARNED: AtomicBool = AtomicBool::new(false);

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

    /// The wire schema version of this payload's serialized shape.
    ///
    /// Stamped into [`EventHeader::payload_version`](crate::event::EventHeader)
    /// at the typed-append seam so a future decoder can tell which struct shape
    /// produced the stored bytes and run the registered [`Upcast`] chain when
    /// they differ. Defaults to `1`; bump it (and freeze a new fixture + add an
    /// `Upcast`) whenever a non-additive change lands. See `EVENTS.md` →
    /// "Schema Evolution".
    ///
    /// `0` is reserved as the legacy/untyped sentinel and is never a valid
    /// declared version. The `#[derive(EventPayload)]` macro accepts an optional
    /// `#[batpak(version = N)]` key (default `1`, `0` rejected).
    ///
    /// [`Upcast`]: crate::event::Upcast
    const PAYLOAD_VERSION: u16 = 1;
}

/// How `Store::open` handles linked `EventPayload` kind collisions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EventPayloadValidation {
    /// Log a single process-wide warning if duplicate payload kinds are linked.
    #[default]
    Warn,
    /// Return an error from `Store::open` when duplicate payload kinds are linked.
    FailFast,
    /// Do not check the payload registry during `Store::open`.
    Silent,
}

/// A duplicate `EventKind` assignment found in the linked payload registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventPayloadKindCollision {
    /// The upper 4-bit event category.
    pub category: u8,
    /// The lower 12-bit type identifier within the category.
    pub type_id: u16,
    /// First registered Rust payload type name.
    pub first_type_name: &'static str,
    /// Second registered Rust payload type name.
    pub second_type_name: &'static str,
}

impl EventPayloadKindCollision {
    fn from_support(collision: batpak_macros_support::EventKindCollision) -> Self {
        // `category()`/`type_id()` narrow the packed nibbles behind EventKind's
        // own invariant, so no unchecked cast is needed here.
        let kind = EventKind::from_raw_u16(collision.kind_bits);
        Self {
            category: kind.category(),
            type_id: kind.type_id(),
            first_type_name: collision.first_type_name,
            second_type_name: collision.second_type_name,
        }
    }
}

/// Error returned when two linked `EventPayload` types claim the same kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventPayloadRegistryError {
    collisions: Vec<EventPayloadKindCollision>,
}

impl EventPayloadRegistryError {
    /// Create an error from a non-empty collision list.
    ///
    /// # Panics
    /// Panics if `collisions` is empty. Callers should use
    /// [`validate_event_payload_registry`] for ordinary validation.
    pub fn new(collisions: Vec<EventPayloadKindCollision>) -> Self {
        assert!(
            !collisions.is_empty(),
            "EventPayloadRegistryError requires at least one collision"
        );
        Self { collisions }
    }

    /// Duplicate kind assignments found in the current binary.
    pub fn collisions(&self) -> &[EventPayloadKindCollision] {
        &self.collisions
    }
}

impl fmt::Display for EventPayloadRegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let first = &self.collisions[0];
        write!(
            f,
            "EventPayload registry contains {} duplicate kind assignment(s); first collision is category=0x{:X} type_id=0x{:03X} between `{}` and `{}`",
            self.collisions.len(),
            first.category,
            first.type_id,
            first.first_type_name,
            first.second_type_name,
        )
    }
}

impl std::error::Error for EventPayloadRegistryError {}

/// Validate that linked `EventPayload` derives use unique `(category, type_id)` pairs.
///
/// The derive-generated registry is binary-wide. Calling this at process
/// startup catches collisions across dependency crates before any typed
/// dispatch path sees ambiguous wire identity.
///
/// # Errors
/// Returns [`EventPayloadRegistryError`] if two or more linked payload types
/// register the same `(category, type_id)` pair.
pub fn validate_event_payload_registry() -> Result<(), EventPayloadRegistryError> {
    let collisions = batpak_macros_support::find_kind_collisions()
        .into_iter()
        .map(EventPayloadKindCollision::from_support)
        .collect::<Vec<_>>();
    if collisions.is_empty() {
        Ok(())
    } else {
        Err(EventPayloadRegistryError::new(collisions))
    }
}

/// Re-scan the linked payload registry and refresh the cached open-time result.
///
/// Most applications never need this because registrations are static once the
/// binary is linked. Tests and tooling that intentionally exercise registry
/// boundaries can call it to force the next `Store::open` warning/fail-fast
/// decision to use a fresh scan.
///
/// # Errors
/// Returns [`EventPayloadRegistryError`] if duplicate payload kinds are linked.
pub fn revalidate_event_payload_registry() -> Result<(), EventPayloadRegistryError> {
    let result = validate_event_payload_registry();
    PAYLOAD_REGISTRY_WARNED.store(false, Ordering::SeqCst);
    let Ok(mut cached) = PAYLOAD_REGISTRY_OPEN_CACHE.lock() else {
        return result;
    };
    *cached = Some(result.clone());
    result
}

pub(crate) fn cached_event_payload_registry_validation() -> Result<(), EventPayloadRegistryError> {
    let Ok(mut cached) = PAYLOAD_REGISTRY_OPEN_CACHE.lock() else {
        return validate_event_payload_registry();
    };
    if let Some(result) = cached.as_ref() {
        return result.clone();
    }
    let result = validate_event_payload_registry();
    *cached = Some(result.clone());
    result
}

pub(crate) fn mark_event_payload_registry_warning_emitted() -> bool {
    !PAYLOAD_REGISTRY_WARNED.swap(true, Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_marker_returns_true_once_until_revalidation_resets_it() {
        revalidate_event_payload_registry().expect("test registry is clean");

        assert!(
            mark_event_payload_registry_warning_emitted(),
            "PROPERTY: first open-time collision warning in a process must be emitted"
        );
        assert!(
            !mark_event_payload_registry_warning_emitted(),
            "PROPERTY: later open-time checks in the same process must not emit duplicate warnings"
        );

        revalidate_event_payload_registry().expect("test registry is clean after marker reset");
        assert!(
            mark_event_payload_registry_warning_emitted(),
            "PROPERTY: explicit revalidation must reset the one-shot warning marker"
        );
    }
}
