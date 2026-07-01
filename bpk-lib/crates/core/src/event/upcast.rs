//! On-read payload schema upcasting (ADR-0010 consumer).
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
//! `EventPayload` kind registry. Use [`register_upcast!`](crate::register_upcast) on an [`Upcast`] impl;
//! each impl supplies a `(KIND, FROM_VERSION)` key and a pure value migration.

use crate::event::{EventKind, EventPayload};
use serde::de::DeserializeOwned;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// A single vN -> vN+1 migration for one [`EventKind`].
///
/// Implement this for each non-additive hop, then register it with
/// [`register_upcast!`](crate::register_upcast). The migration is a *pure* value transform: given a
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
///
/// Rejects trailing bytes after the value: a valid msgpack value followed by
/// junk indicates a truncated/corrupt payload, not a clean single value.
pub(crate) fn value_from_msgpack(bytes: &[u8]) -> Result<rmpv::Value, UpcastError> {
    let mut cursor = bytes;
    let value = rmpv::decode::read_value(&mut cursor)
        .map_err(|e| UpcastError::ValueCodec(format!("read stored msgpack as value: {e}")))?;
    if !cursor.is_empty() {
        return Err(UpcastError::ValueCodec(format!(
            "{} trailing byte(s) after stored msgpack value",
            cursor.len()
        )));
    }
    Ok(value)
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

// ─── Open-time upcast-chain completeness validation ─────────────────────────
//
// A `#[batpak(version = N)]` payload with `N > 1` compiles fine, but if its
// registered `Upcast` steps do not cover every `1 -> 2 -> ... -> N` hop, an
// event written at any uncovered version becomes undecodable at READ time
// (`run_chain` returns `UpcastError::MissingStep`). `Store::open` runs this scan
// — mirroring the kind-collision check in `event::payload` — so that silent
// read-time footgun fails closed at open instead.

static UPCAST_CHAIN_OPEN_CACHE: Mutex<Option<Result<(), UpcastChainRegistryError>>> =
    Mutex::new(None);
static UPCAST_CHAIN_WARNED: AtomicBool = AtomicBool::new(false);

/// A linked payload kind whose declared `PAYLOAD_VERSION > 1` is missing one or
/// more upcast hops, so an event stored at an older version would hit
/// [`UpcastError::MissingStep`] at read time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncompleteUpcastChain {
    /// The payload kind with the incomplete chain.
    pub kind: EventKind,
    /// The declared current payload version (always `> 1`).
    pub current_version: u16,
    /// The `from_version` hops in `1..current_version` with no registered step.
    pub missing_from_versions: Vec<u16>,
    /// A registered Rust type name for this kind.
    pub type_name: &'static str,
}

impl IncompleteUpcastChain {
    fn from_support(chain: batpak_macros_support::IncompleteUpcastChain) -> Self {
        // `from_raw_u16` narrows the packed nibbles behind EventKind's own
        // invariant, so no unchecked cast is needed here.
        Self {
            kind: EventKind::from_raw_u16(chain.kind_bits),
            current_version: chain.current_version,
            missing_from_versions: chain.missing_from_versions,
            type_name: chain.type_name,
        }
    }
}

/// Error returned when a linked `version > 1` payload kind lacks a complete
/// `1 -> ... -> N` upcast chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpcastChainRegistryError {
    incomplete: Vec<IncompleteUpcastChain>,
}

impl UpcastChainRegistryError {
    /// Create an error from a list of incomplete chains.
    ///
    /// [`validate_upcast_chain_registry`] only constructs this with a non-empty
    /// list; the constructor stays total (no panic on empty) so tooling can
    /// build sample values, and [`Display`](std::fmt::Display) renders an empty
    /// list as a benign zero-count message rather than indexing out of bounds.
    pub fn new(incomplete: Vec<IncompleteUpcastChain>) -> Self {
        Self { incomplete }
    }

    /// The incomplete upcast chains found in the linked registry.
    pub fn incomplete_chains(&self) -> &[IncompleteUpcastChain] {
        &self.incomplete
    }
}

impl std::fmt::Display for UpcastChainRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "linked EventPayload registry has {} kind(s) declaring version > 1 \
             without a complete upcast chain",
            self.incomplete.len()
        )?;
        for chain in &self.incomplete {
            write!(
                f,
                "; kind category=0x{:X} type_id=0x{:03X} (`{}`) declares version {} \
                 but is missing upcast step(s) from version(s) {:?} — register an Upcast \
                 for each missing hop (1 -> 2 -> ... -> {})",
                chain.kind.category(),
                chain.kind.type_id(),
                chain.type_name,
                chain.current_version,
                chain.missing_from_versions,
                chain.current_version,
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for UpcastChainRegistryError {}

/// Validate that every linked `version > 1` payload kind has a complete
/// `1 -> ... -> N` upcast chain registered.
///
/// The chain is what lets the decode seam lift an event stored at an older
/// version up to the current struct shape. A `version = N` kind with a gap in
/// its registered [`Upcast`] steps lets any event written at a missing version
/// silently become undecodable ([`UpcastError::MissingStep`]) at read time.
/// Calling this at `Store::open` turns that read-time footgun into a fail-closed
/// open-time error.
///
/// # Errors
/// Returns [`UpcastChainRegistryError`] naming every kind whose declared version
/// exceeds 1 but whose registered upcast steps do not cover every hop.
pub fn validate_upcast_chain_registry() -> Result<(), UpcastChainRegistryError> {
    let incomplete = batpak_macros_support::find_incomplete_upcast_chains()
        .into_iter()
        .map(IncompleteUpcastChain::from_support)
        .collect::<Vec<_>>();
    if incomplete.is_empty() {
        Ok(())
    } else {
        Err(UpcastChainRegistryError::new(incomplete))
    }
}

/// Re-scan the linked upcast registry and refresh the cached open-time result.
///
/// Mirrors
/// [`revalidate_event_payload_registry`](crate::event::revalidate_event_payload_registry):
/// registrations are static once linked, so most applications never need this;
/// tests and tooling that intentionally exercise registry boundaries call it to
/// force the next `Store::open` decision to use a fresh scan.
///
/// # Errors
/// Returns [`UpcastChainRegistryError`] if any `version > 1` kind has an
/// incomplete chain.
pub fn revalidate_upcast_chain_registry() -> Result<(), UpcastChainRegistryError> {
    let result = validate_upcast_chain_registry();
    UPCAST_CHAIN_WARNED.store(false, Ordering::SeqCst);
    let Ok(mut cached) = UPCAST_CHAIN_OPEN_CACHE.lock() else {
        return result;
    };
    *cached = Some(result.clone());
    result
}

pub(crate) fn cached_upcast_chain_registry_validation() -> Result<(), UpcastChainRegistryError> {
    let Ok(mut cached) = UPCAST_CHAIN_OPEN_CACHE.lock() else {
        return validate_upcast_chain_registry();
    };
    if let Some(result) = cached.as_ref() {
        return result.clone();
    }
    let result = validate_upcast_chain_registry();
    *cached = Some(result.clone());
    result
}

pub(crate) fn mark_upcast_chain_registry_warning_emitted() -> bool {
    !UPCAST_CHAIN_WARNED.swap(true, Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use proptest::test_runner::TestCaseError;
    use serde_json::{Map, Value};

    fn arb_json_value() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(|n| Value::Number(n.into())),
            "[a-zA-Z0-9 _:-]{0,24}".prop_map(Value::String),
        ];

        leaf.prop_recursive(3, 24, 4, |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
                proptest::collection::btree_map("[a-zA-Z0-9_:-]{1,12}", inner, 0..4).prop_map(
                    |items| {
                        let mut map = Map::new();
                        for (key, value) in items {
                            map.insert(key, value);
                        }
                        Value::Object(map)
                    }
                ),
            ]
        })
    }

    fn prop_result<T, E: std::fmt::Display>(
        result: Result<T, E>,
        context: &'static str,
    ) -> Result<T, TestCaseError> {
        result.map_err(|err| TestCaseError::fail(format!("{context}: {err}")))
    }

    #[test]
    fn value_from_msgpack_rejects_trailing_bytes() {
        // A valid msgpack value (the integer 42) followed by junk must be
        // rejected — trailing bytes indicate a corrupt/truncated payload, not a
        // clean single value.
        let mut bytes = Vec::new();
        rmpv::encode::write_value(&mut bytes, &rmpv::Value::from(42u8)).expect("encode test value");
        bytes.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

        let err = value_from_msgpack(&bytes)
            .expect_err("PROPERTY: trailing bytes after a msgpack value must be rejected");
        assert!(
            matches!(&err, UpcastError::ValueCodec(_)),
            "expected ValueCodec error, got {err:?}"
        );
        let UpcastError::ValueCodec(msg) = err else {
            unreachable!("matches! above already asserted the ValueCodec variant")
        };
        assert!(
            msg.contains("4 trailing byte(s)"),
            "error must report the trailing byte count, got: {msg}"
        );
    }

    #[test]
    fn value_from_msgpack_accepts_a_clean_single_value() {
        let mut bytes = Vec::new();
        rmpv::encode::write_value(&mut bytes, &rmpv::Value::from(7u8)).expect("encode test value");
        let value = value_from_msgpack(&bytes).expect("clean single value decodes");
        assert_eq!(value.as_u64(), Some(7));
    }

    proptest! {
        #[test]
        fn value_from_json_and_msgpack_agree(value in arb_json_value()) {
            let bytes = prop_result(crate::encoding::to_bytes(&value), "encode json value")?;
            let from_json = prop_result(value_from_json(&value), "value from json")?;
            let from_msgpack = prop_result(value_from_msgpack(&bytes), "value from msgpack")?;
            prop_assert_eq!(from_json, from_msgpack);
        }
    }
}
