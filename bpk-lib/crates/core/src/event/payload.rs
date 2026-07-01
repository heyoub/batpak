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
    /// `Upcast`) whenever a non-additive change lands. See `06_EVENTS.md` →
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
///
/// The default is [`EventPayloadValidation::FailFast`]: two registered payload
/// types that claim the same `(category, type_id)` give the binary ambiguous
/// wire identity, so `Store::open` REFUSES to open when the linked registry
/// contains a collision. The weaker log-and-proceed and skip-the-check modes
/// stay reachable only as explicit opt-outs ([`EventPayloadValidation::Warn`]
/// and [`EventPayloadValidation::Silent`]). This mirrors the store's
/// signing-policy and receipt-hashing idiom: the safe behavior is the default
/// and the looser behavior is an explicit escape hatch.
///
/// This policy only runs at `Store::open`. A binary that registers colliding
/// payloads but **never opens a store** is not covered here; call
/// [`verify_registry`] once at startup (or enable the non-default
/// `startup-registry-check` feature) to catch that case in a release build.
///
/// Set via
/// [`StoreConfig::with_event_payload_validation`](crate::store::StoreConfig::with_event_payload_validation).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum EventPayloadValidation {
    /// Log a single process-wide warning if duplicate payload kinds are linked,
    /// then open anyway. Explicit opt-out for callers that knowingly tolerate a
    /// duplicate registration (it is no longer the default).
    Warn,
    /// Return an error from `Store::open` when duplicate payload kinds are
    /// linked. This is the safe default: a collision is refused at open.
    #[default]
    FailFast,
    /// Do not check the payload registry during `Store::open`. Explicit opt-out
    /// that skips the registry scan entirely.
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

/// Release-startup entry point: verify the linked `EventPayload` registry has no
/// duplicate-kind collisions.
///
/// Call this once at process startup **if your binary registers `EventPayload`
/// types (directly or through a dependency crate) but may never open a
/// [`Store`](crate::store::Store)**. `Store::open` already runs this same check
/// under the default [`EventPayloadValidation::FailFast`] policy, so a binary
/// that opens a store is covered. A binary that registers colliding payloads and
/// never opens a store would otherwise get **no** collision check in a release
/// build: the `#[derive(EventPayload)]` macro's own collision test is
/// `#[cfg(test)]`-only and is absent from a non-test binary. This entry point
/// closes that gap and catches the linked-kind collision a release build would
/// not see.
///
/// For automatic enforcement without a manual call, enable the non-default
/// `startup-registry-check` feature: it installs one process-wide startup
/// constructor that runs this check and aborts on a collision before `main`.
///
/// This is a thin, clearly-named alias for
/// [`validate_event_payload_registry`]; the two are interchangeable.
///
/// # Errors
/// Returns [`EventPayloadRegistryError`] if two or more linked payload types
/// register the same `(category, type_id)` pair.
pub fn verify_registry() -> Result<(), EventPayloadRegistryError> {
    validate_event_payload_registry()
}

/// Process-wide startup constructor installed by the non-default
/// `startup-registry-check` feature.
///
/// Runs before `main`, so a release binary that registers colliding
/// `EventPayload` kinds and never opens a `Store` still fails fast: it writes a
/// diagnostic to `stderr` and aborts the process. One central constructor covers
/// the whole binary (the derive emits no per-type startup hook), so this is
/// idempotent by construction. The diagnostic is written with `write_all` on
/// `std::io::stderr()` rather than `eprintln!` to honor the crate's
/// no-`print_stderr` discipline, and the write result is deliberately ignored:
/// if `stderr` itself is unwritable the process must still abort so the collision
/// can never be silently accepted at startup.
#[cfg(feature = "startup-registry-check")]
#[ctor::ctor]
fn __batpak_verify_registry_at_startup() {
    use std::io::Write;

    if let Err(error) = verify_registry() {
        let message = format!("batpak startup-registry-check: aborting before main: {error}\n");
        let mut stderr = std::io::stderr();
        let _ = stderr.write_all(message.as_bytes());
        let _ = stderr.flush();
        std::process::abort();
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
