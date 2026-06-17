//! On-read payload schema upcasting (Workstream A, ADR-0010 consumer).
//!
//! When a stored event's `payload_version` is *older* than the current
//! [`EventPayload::PAYLOAD_VERSION`], the decode seam runs the registered
//! [`Upcast`] chain to lift the old-shape value to the current shape, then
//! decodes. Migration is **in-memory only**: the chain operates on a decoded
//! [`rmpv::Value`] and the result is re-serialized for the final decode — the
//! stored bytes (and therefore every content hash and signature, which cover
//! the *payload bytes*, not the header) are never rewritten.
//!
//! # Lane neutrality
//!
//! A single registered step serves both replay lanes. The raw lane decodes its
//! msgpack bytes straight to [`rmpv::Value`]; the JSON lane converts its
//! [`serde_json::Value`] to [`rmpv::Value`] (via a msgpack round-trip) before
//! running the same chain. The chain runner here is agnostic to which lane fed
//! it.
//!
//! # Registration
//!
//! Steps register link-time through the same `inventory` pattern as the
//! `EventPayload` kind registry. Use [`register_upcast!`] on an [`Upcast`] impl;
//! each impl supplies a `(KIND, FROM_VERSION)` key and a pure value migration.

use crate::event::{EventKind, EventPayload};
use serde::de::DeserializeOwned;

/// A single vN -> vN+1 migration for one [`EventKind`].
///
/// Implement this for each non-additive hop, then register it with
/// [`register_upcast!`]. The migration is a *pure* value transform: given a
/// decoded value of version [`Upcast::FROM_VERSION`], produce a value of version
/// `FROM_VERSION + 1`. It must not perform I/O and must be deterministic so a
/// replay is byte-stable.
pub trait Upcast {
    /// The kind this migration applies to (must match the payload's `KIND`).
    const KIND: EventKind;
    /// The stored version this step upgrades *from*; it produces `FROM_VERSION + 1`.
    const FROM_VERSION: u16;

    /// Migrate a decoded value of [`Self::FROM_VERSION`] to `FROM_VERSION + 1`.
    ///
    /// # Errors
    /// Returns an [`UpcastError::Step`] (after boxing) when the input shape is
    /// not what this migration expects.
    fn upcast(value: rmpv::Value) -> Result<rmpv::Value, UpcastError>;
}

/// Error raised while upcasting a stored payload to the current version.
#[derive(Debug)]
pub enum UpcastError {
    /// No registered step exists for a `(kind, from_version)` hop the chain
    /// needs to reach the current version — there is a gap in the migration set.
    MissingStep {
        /// The kind being upcast.
        kind: EventKind,
        /// The version the chain was stuck at (no step registered *from* here).
        from_version: u16,
        /// The current target version.
        to_version: u16,
    },
    /// Two registrations claim the same `(kind, from_version)` hop. Ambiguous
    /// migrations are a programming error, surfaced rather than silently
    /// resolved.
    DuplicateStep {
        /// The kind with the duplicated hop.
        kind: EventKind,
        /// The duplicated source version.
        from_version: u16,
    },
    /// A registered step's value transform failed (shape mismatch, etc.).
    Step {
        /// The kind being upcast.
        kind: EventKind,
        /// The hop that failed.
        from_version: u16,
        /// The underlying step error.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Encoding/decoding the value between rmpv and the lane representation failed.
    ValueCodec(String),
}

impl std::fmt::Display for UpcastError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingStep {
                kind,
                from_version,
                to_version,
            } => write!(
                f,
                "no registered upcast step for kind {kind:?} from version {from_version} \
                 (need to reach version {to_version}); register an Upcast for this hop"
            ),
            Self::DuplicateStep { kind, from_version } => write!(
                f,
                "duplicate upcast step registered for kind {kind:?} from version {from_version}"
            ),
            Self::Step {
                kind,
                from_version,
                source,
            } => write!(
                f,
                "upcast step for kind {kind:?} from version {from_version} failed: {source}"
            ),
            Self::ValueCodec(msg) => write!(f, "upcast value codec error: {msg}"),
        }
    }
}

impl std::error::Error for UpcastError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Step { source, .. } => Some(source.as_ref()),
            Self::MissingStep { .. } | Self::DuplicateStep { .. } | Self::ValueCodec(_) => None,
        }
    }
}

/// Register an [`Upcast`] implementation so the decode seam can find it.
///
/// Mirrors the `inventory::submit!` block `#[derive(EventPayload)]` emits for
/// the kind registry. Place at item scope:
///
/// ```ignore
/// struct ThingV1ToV2;
/// impl Upcast for ThingV1ToV2 { /* ... */ }
/// register_upcast!(ThingV1ToV2);
/// ```
#[macro_export]
macro_rules! register_upcast {
    ($ty:ty) => {
        $crate::__private::inventory::submit! {
            $crate::__private::UpcastRegistration {
                kind_bits: <$ty as $crate::event::Upcast>::KIND.as_raw_u16(),
                from_version: <$ty as $crate::event::Upcast>::FROM_VERSION,
                step: |value| {
                    <$ty as $crate::event::Upcast>::upcast(value)
                        .map_err(|e| -> ::std::boxed::Box<
                            dyn ::std::error::Error + ::std::marker::Send + ::std::marker::Sync,
                        > { ::std::boxed::Box::new(e) })
                },
            }
        }
    };
}

/// Run the registered upcast chain for `kind`, lifting `value` from
/// `from_version` up to `to_version`, then decode into `T`.
///
/// `value` is the stored payload already decoded to [`rmpv::Value`] (raw lane:
/// straight from the msgpack bytes; JSON lane: converted from
/// `serde_json::Value`). The returned `T` is the current-version struct.
///
/// # Errors
/// Returns [`UpcastError::MissingStep`] when the migration set has a gap,
/// [`UpcastError::DuplicateStep`] on an ambiguous hop, [`UpcastError::Step`]
/// when a migration's transform fails, or [`UpcastError::ValueCodec`] when the
/// final re-encode/decode into `T` fails.
pub(crate) fn upcast_and_decode<T: EventPayload>(
    value: rmpv::Value,
    from_version: u16,
    to_version: u16,
) -> Result<T, UpcastError> {
    let lifted = run_chain(T::KIND, value, from_version, to_version)?;
    decode_value::<T>(&lifted)
}

/// Apply registered steps for `kind` from `from_version` to `to_version`.
///
/// Exposed at crate scope (not `pub`) so the seam and tests can drive the chain
/// directly without the final `T` decode.
pub(crate) fn run_chain(
    kind: EventKind,
    mut value: rmpv::Value,
    from_version: u16,
    to_version: u16,
) -> Result<rmpv::Value, UpcastError> {
    let registry = crate::__private::upcast_steps_for(kind.as_raw_u16());
    let mut current = from_version;
    while current < to_version {
        let mut matches = registry.iter().filter(|reg| reg.from_version == current);
        let Some(step) = matches.next() else {
            return Err(UpcastError::MissingStep {
                kind,
                from_version: current,
                to_version,
            });
        };
        if matches.next().is_some() {
            return Err(UpcastError::DuplicateStep {
                kind,
                from_version: current,
            });
        }
        value = (step.step)(value).map_err(|source| UpcastError::Step {
            kind,
            from_version: current,
            source,
        })?;
        current += 1;
    }
    Ok(value)
}

/// Decode an [`rmpv::Value`] into `T` via a msgpack round-trip.
///
/// rmpv -> named msgpack bytes -> `T`, so the named-field decode contract is
/// identical to the raw lane's normal path.
fn decode_value<T: DeserializeOwned>(value: &rmpv::Value) -> Result<T, UpcastError> {
    let mut buf = Vec::new();
    rmpv::encode::write_value(&mut buf, value)
        .map_err(|e| UpcastError::ValueCodec(format!("re-encode upcasted value: {e}")))?;
    crate::encoding::from_bytes::<T>(&buf)
        .map_err(|e| UpcastError::ValueCodec(format!("decode upcasted value into target: {e}")))
}

/// Decode raw msgpack payload bytes into an [`rmpv::Value`] for upcasting.
pub(crate) fn value_from_msgpack(bytes: &[u8]) -> Result<rmpv::Value, UpcastError> {
    let mut cursor = bytes;
    rmpv::decode::read_value(&mut cursor)
        .map_err(|e| UpcastError::ValueCodec(format!("read stored msgpack as value: {e}")))
}

/// Convert a `serde_json::Value` (JSON replay lane) into an [`rmpv::Value`].
///
/// Routes through named msgpack so the result matches the byte-shape the raw
/// lane would have produced for the same logical payload.
pub(crate) fn value_from_json(value: &serde_json::Value) -> Result<rmpv::Value, UpcastError> {
    let bytes = crate::encoding::to_bytes(value)
        .map_err(|e| UpcastError::ValueCodec(format!("encode json payload to msgpack: {e}")))?;
    value_from_msgpack(&bytes)
}
