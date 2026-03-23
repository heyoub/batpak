use std::fmt;
use std::hash::Hash;
use std::str::FromStr;

/// EntityIdType: Layer 0 trait. No uuid dep.
/// All IDs are u128 internally. No Uuid in public API. [SPEC:src/id/mod.rs]
/// [SPEC:RED FLAGS — DO NOT put uuid::Uuid in the public API]
pub trait EntityIdType:
    Copy + Clone + Eq + Hash + fmt::Debug + fmt::Display + FromStr + Send + Sync + 'static
{
    const ENTITY_NAME: &'static str;
    fn new(id: u128) -> Self;
    fn as_u128(&self) -> u128;
    fn now_v7() -> Self;
    fn nil() -> Self;
}

/// Helper function: generates a UUIDv7 as u128. Used by the macro below.
/// This keeps `uuid` as a private dependency — downstream crates calling
/// define_entity_id! don't need uuid in their own Cargo.toml.
/// \[DEP:uuid::Uuid::now_v7\] → generates UUIDv7, .as_u128() → u128
pub fn generate_v7_id() -> u128 {
    uuid::Uuid::now_v7().as_u128()
}

/// define_entity_id!: Layer 1+ macro. Uses generate_v7_id() helper.
/// Downstream crates do NOT need uuid as a direct dependency.
#[macro_export]
macro_rules! define_entity_id {
    ($name:ident, $entity:literal) => {
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
                // Parse "entity_name:hex" or bare hex
                let hex = s.strip_prefix(concat!($entity, ":")).unwrap_or(s);
                u128::from_str_radix(hex, 16)
                    .map(Self)
                    .map_err(|e| format!("invalid {}: {e}", $entity))
            }
        }
    };
}

// Library defines ONE id type.
define_entity_id!(EventId, "event");
