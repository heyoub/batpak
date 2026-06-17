use std::fmt;
use std::hash::Hash;
use std::str::FromStr;

/// EntityIdType: Layer 0 trait. No uuid dep.
/// All IDs are `u128` internally. Keep `uuid::Uuid` out of the public API.
pub trait EntityIdType:
    Copy + Clone + Eq + Hash + fmt::Debug + fmt::Display + FromStr + Send + Sync + 'static
{
    /// The canonical string name of this entity type (e.g. `"event"`).
    const ENTITY_NAME: &'static str;
    /// Construct an instance from a raw `u128` identifier.
    fn new(id: u128) -> Self;
    /// Return the underlying `u128` value.
    fn as_u128(&self) -> u128;
    /// Generate a new UUIDv7-based instance using the current time.
    fn now_v7() -> Self;
    /// Generate a new UUIDv7-based instance using a caller-provided clock.
    fn now_v7_with_clock(clock: &dyn crate::store::Clock) -> Self {
        Self::new(generate_v7_id_with_clock(clock))
    }
    /// Return the nil (zero) instance.
    fn nil() -> Self;
}

/// Helper function: generates a UUIDv7 as u128. Used by the macro below.
/// This keeps `uuid` as a private dependency — downstream crates calling
/// define_entity_id! don't need uuid in their own Cargo.toml.
/// \[DEP:uuid::Uuid::now_v7\] → generates UUIDv7, .as_u128() → u128
pub fn generate_v7_id() -> u128 {
    uuid::Uuid::now_v7().as_u128()
}

/// Generate a UUIDv7 as `u128` using a caller-provided store clock.
///
/// This is the deterministic counterpart to [`generate_v7_id`]. The UUIDv7
/// timestamp is derived from `clock.now_us()`, while the random/counter portion
/// remains owned by the `uuid` crate.
pub fn generate_v7_id_with_clock(clock: &dyn crate::store::Clock) -> u128 {
    let timestamp_us = clock.now_us().max(0);
    let seconds = timestamp_us / 1_000_000;
    let subsec_nanos = (timestamp_us % 1_000_000) * 1_000;
    let seconds = u64::try_from(seconds).unwrap_or(u64::MAX);
    let subsec_nanos = u32::try_from(subsec_nanos).unwrap_or(u32::MAX);
    uuid::Uuid::new_v7(uuid::Timestamp::from_unix(
        uuid::NoContext,
        seconds,
        subsec_nanos,
    ))
    .as_u128()
}

/// define_entity_id!: Layer 1+ macro. Uses generate_v7_id() helper.
/// Downstream crates do NOT need uuid as a direct dependency.
#[macro_export]
macro_rules! define_entity_id {
    ($name:ident, $entity:literal) => {
        #[doc = concat!("Typed entity ID for `", $entity, "` entities. Wraps a `u128` UUIDv7.")]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub struct $name(u128);

        impl $crate::id::EntityIdType for $name {
            const ENTITY_NAME: &'static str = $entity;

            fn new(id: u128) -> Self {
                Self(id)
            }

            fn as_u128(&self) -> u128 {
                self.0
            }

            fn now_v7() -> Self {
                Self($crate::id::generate_v7_id())
            }

            fn now_v7_with_clock(clock: &dyn $crate::store::Clock) -> Self {
                Self($crate::id::generate_v7_id_with_clock(clock))
            }

            fn nil() -> Self {
                Self(0)
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                // Display as "entity_name:hex" e.g. "event:0a1b2c..."
                write!(f, "{}:{:032x}", $entity, self.0)
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = String;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                // Parse "entity_name:hex" — bare hex without the explicit
                // entity prefix is rejected so ambiguous inputs (e.g. two
                // entity types that share the same hex) cannot silently
                // alias.
                let hex = s.strip_prefix(concat!($entity, ":")).ok_or_else(|| {
                    format!(
                        "invalid {}: missing entity prefix '{}:' in {s:?}",
                        $entity, $entity
                    )
                })?;
                u128::from_str_radix(hex, 16)
                    .map(Self)
                    .map_err(|e| format!("invalid {}: {e}", $entity))
            }
        }
    };
}

// Library defines the canonical public id types. Each is a typed newtype
// around `u128` so the API cannot silently confuse (say) an event id with a
// causation id. Raw `u128` traffic is an internal-only escape hatch —
// external crates should prefer the typed constructors, and the `From<u128>`
// / `as_u128()` helpers are documented as the wire-serde boundary seam.
define_entity_id!(EventId, "event");
define_entity_id!(CorrelationId, "correlation");
define_entity_id!(CausationId, "causation");
define_entity_id!(IdempotencyKey, "idempotency");

impl From<u128> for EventId {
    fn from(id: u128) -> Self {
        <Self as EntityIdType>::new(id)
    }
}

impl From<EventId> for u128 {
    fn from(id: EventId) -> Self {
        id.as_u128()
    }
}

impl From<u128> for CorrelationId {
    fn from(id: u128) -> Self {
        <Self as EntityIdType>::new(id)
    }
}

impl From<CorrelationId> for u128 {
    fn from(id: CorrelationId) -> Self {
        id.as_u128()
    }
}

impl From<u128> for CausationId {
    fn from(id: u128) -> Self {
        <Self as EntityIdType>::new(id)
    }
}

impl From<CausationId> for u128 {
    fn from(id: CausationId) -> Self {
        id.as_u128()
    }
}

impl From<u128> for IdempotencyKey {
    fn from(id: u128) -> Self {
        <Self as EntityIdType>::new(id)
    }
}

impl From<IdempotencyKey> for u128 {
    fn from(id: IdempotencyKey) -> Self {
        id.as_u128()
    }
}

impl IdempotencyKey {
    /// Derive a deterministic idempotency key from an OPERATION IDENTITY: a
    /// `domain` plus an ordered list of `components`.
    ///
    /// The key is blake3 over a **length-delimited** encoding — each of the
    /// domain and every component is length-prefixed before hashing — so that
    /// `["ab", "c"]` and `["a", "bc"]` produce DIFFERENT keys (no boundary
    /// ambiguity / canonicalization collisions). The first 16 bytes of the
    /// digest are taken as the `u128`.
    ///
    /// # This is operation identity, NOT a content hash
    ///
    /// `for_operation` answers "is this the SAME operation I already performed?"
    /// — e.g. `("transfer", &[from, to, request_id])`. It deliberately does
    /// NOT hash the event payload bytes. Do not misuse it as a
    /// content-addressing scheme: two semantically-identical operations
    /// submitted with the same components must collide to the same key (that is
    /// the whole point of idempotency), whereas a content hash must change
    /// whenever any payload byte changes. Conflating the two re-introduces the
    /// exact failure mode the schema-evolution plan warns against — treating an
    /// operation key as if it certified payload contents.
    ///
    /// # Example
    /// ```
    /// use batpak::id::IdempotencyKey;
    /// let a = IdempotencyKey::for_operation("transfer", &["acct:1", "acct:2", "req:42"]);
    /// let b = IdempotencyKey::for_operation("transfer", &["acct:1", "acct:2", "req:42"]);
    /// assert_eq!(a, b, "same operation identity -> same key (idempotent)");
    /// let c = IdempotencyKey::for_operation("transfer", &["acct:12", "acct:2", "req:42"]);
    /// assert_ne!(a, c, "length-delimited encoding keeps boundaries distinct");
    /// ```
    #[must_use]
    pub fn for_operation(domain: &str, components: &[&str]) -> Self {
        let mut hasher = blake3::Hasher::new();
        // Length-delimit the domain.
        hasher.update(&(domain.len() as u64).to_le_bytes());
        hasher.update(domain.as_bytes());
        // Length-delimit the component count, then each component.
        hasher.update(&(components.len() as u64).to_le_bytes());
        for component in components {
            hasher.update(&(component.len() as u64).to_le_bytes());
            hasher.update(component.as_bytes());
        }
        let digest = hasher.finalize();
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest.as_bytes()[..16]);
        <Self as EntityIdType>::new(u128::from_be_bytes(bytes))
    }
}

// Serde impls for the typed ids. Wire format is unchanged from the raw u128
// path: each newtype serializes as 16 big-endian bytes via the existing
// crate::wire::u128_bytes helpers. EventHeader's #[serde(with = ...)] field
// annotations point at the typed wrappers (event_id_bytes etc) below.

impl serde::Serialize for EventId {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        crate::wire::u128_bytes::serialize(&self.0, ser)
    }
}

impl<'de> serde::Deserialize<'de> for EventId {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        crate::wire::u128_bytes::deserialize(de).map(Self)
    }
}

impl serde::Serialize for CorrelationId {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        crate::wire::u128_bytes::serialize(&self.0, ser)
    }
}

impl<'de> serde::Deserialize<'de> for CorrelationId {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        crate::wire::u128_bytes::deserialize(de).map(Self)
    }
}

impl serde::Serialize for CausationId {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        crate::wire::u128_bytes::serialize(&self.0, ser)
    }
}

impl<'de> serde::Deserialize<'de> for CausationId {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        crate::wire::u128_bytes::deserialize(de).map(Self)
    }
}

impl serde::Serialize for IdempotencyKey {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        crate::wire::u128_bytes::serialize(&self.0, ser)
    }
}

impl<'de> serde::Deserialize<'de> for IdempotencyKey {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        crate::wire::u128_bytes::deserialize(de).map(Self)
    }
}
