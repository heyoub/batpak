//! Typed decode/route seam (Dispatch Chapter, ADR-0010 consumer).
//!
//! [`DecodeTyped`] is the single shared dispatch primitive used by every
//! downstream typed surface (projection derives, typed reactors, multi-event
//! reactors). Given an `Event<_>` in either replay lane, it answers exactly
//! one question: *is this event of kind `T::KIND`, and if so, can it decode
//! to `T`?*
//!
//! The seam is deliberately tiny so every consumer of it inherits the same
//! semantics:
//!
//! * [`route_typed`](DecodeTyped::route_typed) returns `Ok(None)` when the
//!   event's kind does not match `T::KIND` — a filter, not an error.
//! * [`decode_typed`](DecodeTyped::decode_typed) returns
//!   [`TypedDecodeError::KindMismatch`] when the caller asserted a match but
//!   the event's kind says otherwise — a strict-mode contract, distinct from
//!   a deserialization failure.
//! * Both methods return [`TypedDecodeError::DecodeFailure`] only when the
//!   kind matched and the payload bytes could not be deserialized into `T`.
//!
//! Both replay lanes implement the trait: [`Event<serde_json::Value>`] via
//! [`serde_json::from_value`] and [`Event<Vec<u8>>`] via
//! [`rmp_serde::from_slice`]. Neither lane is privileged; callers consuming
//! the seam cannot accidentally lock themselves into JSON-only behaviour.
//!
//! `P::KIND` is the sole identity check. The seam does not consult any
//! runtime registry.

use crate::event::{Event, EventKind, EventPayload};
use serde::de::DeserializeOwned;

/// Source of a payload decode failure, retaining the lane-specific error chain.
#[derive(Debug)]
pub enum DecodeSource {
    /// Decode via `serde_json::from_value` failed.
    Json(serde_json::Error),
    /// Decode via `rmp_serde::from_slice` failed.
    Msgpack(rmp_serde::decode::Error),
}

impl std::fmt::Display for DecodeSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(e) => write!(f, "json decode: {e}"),
            Self::Msgpack(e) => write!(f, "msgpack decode: {e}"),
        }
    }
}

impl std::error::Error for DecodeSource {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(e) => Some(e),
            Self::Msgpack(e) => Some(e),
        }
    }
}

/// Error returned by [`DecodeTyped::decode_typed`] (and by
/// [`DecodeTyped::route_typed`] when the kind matched but decode failed).
///
/// The two variants separate the two distinct failure modes at the type
/// level, so callers (including derive-generated dispatch code) never
/// conflate "this event is not for me" with "this event was malformed."
#[derive(Debug)]
pub enum TypedDecodeError {
    /// The event's kind did not match the target type's `KIND`.
    ///
    /// Emitted by [`DecodeTyped::decode_typed`] only. `route_typed` returns
    /// `Ok(None)` in this case.
    KindMismatch {
        /// The `KIND` the caller asserted.
        expected: EventKind,
        /// The `EventKind` on the event.
        got: EventKind,
    },
    /// The kind matched but the payload could not be deserialized into the
    /// target type. The lane-specific error is chained via [`DecodeSource`].
    DecodeFailure {
        /// The matched kind.
        kind: EventKind,
        /// The underlying lane-specific decode error.
        source: DecodeSource,
    },
}

impl std::fmt::Display for TypedDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::KindMismatch { expected, got } => {
                write!(f, "kind mismatch: expected {expected:?}, got {got:?}")
            }
            Self::DecodeFailure { kind, source } => {
                write!(f, "decode failed for kind {kind:?}: {source}")
            }
        }
    }
}

impl std::error::Error for TypedDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::KindMismatch { .. } => None,
            Self::DecodeFailure { source, .. } => Some(source),
        }
    }
}

/// Typed decode/route seam.
///
/// Implemented for every replay lane: `Event<serde_json::Value>` (JSON) and
/// `Event<Vec<u8>>` (raw msgpack). Both lanes share an identical behavioural
/// contract so downstream consumers (projections, reactors) inherit the same
/// semantics regardless of which lane they read.
pub trait DecodeTyped {
    /// Route an event by kind, decoding into `T` iff `self.event_kind() == T::KIND`.
    ///
    /// Returns:
    /// * `Ok(None)` when the event's kind differs from `T::KIND` (not an error; the event is not for this target type).
    /// * `Ok(Some(t))` when the kind matched and the payload decoded.
    /// * `Err(TypedDecodeError::DecodeFailure)` when the kind matched but the payload could not be deserialized.
    ///
    /// `TypedDecodeError::KindMismatch` is never returned by `route_typed` —
    /// the mismatch case is an `Ok(None)` by design, so generated dispatch
    /// code can chain arms without an error path per non-matching kind.
    ///
    /// # Errors
    /// Returns [`TypedDecodeError::DecodeFailure`] only when the event's
    /// kind matched `T::KIND` but the payload could not be deserialized
    /// into `T`. Wrong-kind events return `Ok(None)`, not an error.
    fn route_typed<T: EventPayload>(&self) -> Result<Option<T>, TypedDecodeError>;

    /// Decode an event strictly, asserting the caller already knows the kind matches.
    ///
    /// Returns:
    /// * `Ok(t)` when the kind matched and the payload decoded.
    /// * `Err(TypedDecodeError::KindMismatch)` when the kind did not match.
    /// * `Err(TypedDecodeError::DecodeFailure)` when the kind matched but the payload could not be deserialized.
    ///
    /// Use this when the caller has already filtered on kind and a mismatch
    /// would be a bug, not a skip.
    ///
    /// # Errors
    /// Returns [`TypedDecodeError::KindMismatch`] if `self.event_kind() != T::KIND`,
    /// or [`TypedDecodeError::DecodeFailure`] if the payload cannot be deserialized
    /// into `T`.
    fn decode_typed<T: EventPayload>(&self) -> Result<T, TypedDecodeError>;
}

impl DecodeTyped for Event<serde_json::Value> {
    fn route_typed<T: EventPayload>(&self) -> Result<Option<T>, TypedDecodeError> {
        if self.header.event_kind != T::KIND {
            return Ok(None);
        }
        decode_json::<T>(self.header.event_kind, &self.payload).map(Some)
    }

    fn decode_typed<T: EventPayload>(&self) -> Result<T, TypedDecodeError> {
        if self.header.event_kind != T::KIND {
            return Err(TypedDecodeError::KindMismatch {
                expected: T::KIND,
                got: self.header.event_kind,
            });
        }
        decode_json::<T>(self.header.event_kind, &self.payload)
    }
}

impl DecodeTyped for Event<Vec<u8>> {
    fn route_typed<T: EventPayload>(&self) -> Result<Option<T>, TypedDecodeError> {
        if self.header.event_kind != T::KIND {
            return Ok(None);
        }
        decode_msgpack::<T>(self.header.event_kind, &self.payload).map(Some)
    }

    fn decode_typed<T: EventPayload>(&self) -> Result<T, TypedDecodeError> {
        if self.header.event_kind != T::KIND {
            return Err(TypedDecodeError::KindMismatch {
                expected: T::KIND,
                got: self.header.event_kind,
            });
        }
        decode_msgpack::<T>(self.header.event_kind, &self.payload)
    }
}

fn decode_json<T: DeserializeOwned>(
    kind: EventKind,
    value: &serde_json::Value,
) -> Result<T, TypedDecodeError> {
    // Borrow-based decode: `&Value` implements `Deserializer`, so
    // `T::deserialize(value)` goes straight through without allocating.
    // The older `serde_json::from_value(value.clone())` form allocated a
    // full `Value` copy on every decode — real cost on hot reactor /
    // projection paths.
    T::deserialize(value).map_err(|e| TypedDecodeError::DecodeFailure {
        kind,
        source: DecodeSource::Json(e),
    })
}

fn decode_msgpack<T: DeserializeOwned>(
    kind: EventKind,
    bytes: &[u8],
) -> Result<T, TypedDecodeError> {
    rmp_serde::from_slice::<T>(bytes).map_err(|e| TypedDecodeError::DecodeFailure {
        kind,
        source: DecodeSource::Msgpack(e),
    })
}

#[cfg(test)]
mod in_crate_derive_proof {
    //! In-crate path-hygiene proof for `#[derive(EventPayload)]` and the
    //! `DecodeTyped` seam.
    //!
    //! Integration tests in `tests/` compile as separate crates that depend
    //! on `batpak` — they're effectively "downstream-style" proofs. The true
    //! in-crate proof requires exercising the derive from a module inside
    //! `src/`, under `#[cfg(test)]`, reaching types via `::batpak::...`
    //! paths. That works iff `pub extern crate self as batpak;` at the crate
    //! root (src/lib.rs) is in place.
    //!
    //! If the self-alias ever regresses, this module fails to compile.
    //!
    //! Pairs with `fixtures/downstream/` (which proves the outward-facing
    //! direction).
    use super::DecodeTyped;
    use ::batpak::EventPayload;

    #[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq, Debug, EventPayload)]
    #[batpak(category = 0xE, type_id = 0xAB1)]
    struct InCrateProof {
        value: u64,
    }

    #[test]
    fn derive_resolves_from_inside_crate() {
        let expected = ::batpak::event::EventKind::custom(0xE, 0xAB1);
        assert_eq!(
            <InCrateProof as ::batpak::event::EventPayload>::KIND,
            expected,
            "PROPERTY: ::batpak::... paths must resolve from inside the crate (pub extern crate self as batpak)"
        );
    }

    #[test]
    fn route_typed_works_from_inside_crate() {
        use crate::event::{Event, EventHeader};

        let header = EventHeader::new(
            1,
            0,
            None,
            0,
            crate::coordinate::DagPosition::root(),
            0,
            <InCrateProof as ::batpak::event::EventPayload>::KIND,
        );
        let event: Event<serde_json::Value> =
            Event::new(header, serde_json::json!({ "value": 99 }));
        let routed: Option<InCrateProof> = event.route_typed().expect("route_typed");
        assert_eq!(routed, Some(InCrateProof { value: 99 }));
    }
}
