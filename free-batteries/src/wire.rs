use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::ser::Serializer;
use std::fmt;

/// Serde helpers for u128 serialization as [u8; 16] big-endian.
/// MessagePack has no native u128 type. Bare u128 causes rmp-serde errors.
/// Big-endian preserves sort order and is standard network byte order.
/// [SPEC:WIRE FORMAT DECISIONS item 2]
///
/// ZERO internal dependencies. This module is declared FIRST in lib.rs.
/// Every serializable type with a u128 field uses these helpers.
/// [SPEC:BUILD ORDER STEP 4 — wire.rs is FIRST]
pub mod u128_bytes {
    /// Usage: #[serde(with = "crate::wire::u128_bytes")]
    /// Annotated on: EventHeader.event_id, EventHeader.correlation_id,
    ///   Notification.event_id, Notification.correlation_id,
    ///   Committed.event_id, WaitCondition::Event.event_id,
    ///   CompensationAction::Notify.target_id, Outcome::Pending.resume_token
    use super::*;

    pub fn serialize<S: Serializer>(val: &u128, ser: S) -> Result<S::Ok, S::Error> {
        // Convert to 16-byte big-endian array, serialize as bytes.
        // [DEP:serde::Serializer::serialize_bytes]
        ser.serialize_bytes(&val.to_be_bytes())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<u128, D::Error> {
        // Accept bytes, convert from big-endian to u128.
        // Use a Visitor that handles both byte arrays and sequences.
        // [DEP:serde::de::Visitor]
        struct U128Visitor;
        impl<'de> Visitor<'de> for U128Visitor {
            type Value = u128;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("16 bytes for u128")
            }
            fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<u128, E> {
                // v must be exactly 16 bytes. Convert via from_be_bytes.
                let arr: [u8; 16] = v
                    .try_into()
                    .map_err(|_| E::invalid_length(v.len(), &"16 bytes"))?;
                Ok(u128::from_be_bytes(arr))
            }
            // Also handle seq format (some deserializers emit sequences not bytes)
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<u128, A::Error> {
                let mut bytes = [0u8; 16];
                for (i, byte) in bytes.iter_mut().enumerate() {
                    *byte = seq
                        .next_element()?
                        .ok_or_else(|| de::Error::invalid_length(i, &"16 bytes"))?;
                }
                Ok(u128::from_be_bytes(bytes))
            }
        }
        de.deserialize_bytes(U128Visitor)
    }
}

pub mod option_u128_bytes {
    /// Usage: #[serde(with = "crate::wire::option_u128_bytes")]
    /// Annotated on: EventHeader.causation_id, Notification.causation_id
    use super::*;

    pub fn serialize<S: Serializer>(val: &Option<u128>, ser: S) -> Result<S::Ok, S::Error> {
        match val {
            Some(v) => ser.serialize_bytes(&v.to_be_bytes()),
            None => ser.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Option<u128>, D::Error> {
        // Visitor that handles None (nil) and Some(bytes).
        struct OptU128Visitor;
        impl<'de> Visitor<'de> for OptU128Visitor {
            type Value = Option<u128>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("null or 16 bytes for u128")
            }
            fn visit_none<E: de::Error>(self) -> Result<Option<u128>, E> {
                Ok(None)
            }
            fn visit_some<D2: Deserializer<'de>>(self, de: D2) -> Result<Option<u128>, D2::Error> {
                super::u128_bytes::deserialize(de).map(Some)
            }
            fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Option<u128>, E> {
                let arr: [u8; 16] = v
                    .try_into()
                    .map_err(|_| E::invalid_length(v.len(), &"16 bytes"))?;
                Ok(Some(u128::from_be_bytes(arr)))
            }
        }
        de.deserialize_option(OptU128Visitor)
    }
}

pub mod vec_u128_bytes {
    /// Usage: #[serde(with = "crate::wire::vec_u128_bytes")]
    /// Annotated on: CompensationAction::Rollback.event_ids,
    ///   CompensationAction::Release.resource_ids
    use super::*;

    pub fn serialize<S: Serializer>(val: &[u128], ser: S) -> Result<S::Ok, S::Error> {
        // Serialize as a sequence of [u8; 16] fixed-size arrays (NOT bytes).
        // Using arrays ensures serialize and deserialize use the same msgpack
        // format (array of arrays, not array of bin). Avoids format mismatch.
        use serde::ser::SerializeSeq;
        let mut seq = ser.serialize_seq(Some(val.len()))?;
        for v in val {
            seq.serialize_element(&v.to_be_bytes())?; // [u8; 16], serialized as array
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<u128>, D::Error> {
        // Deserialize a sequence of [u8; 16] arrays back to Vec<u128>.
        struct VecU128Visitor;
        impl<'de> Visitor<'de> for VecU128Visitor {
            type Value = Vec<u128>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("sequence of 16-byte u128 values")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<u128>, A::Error> {
                let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0));
                while let Some(arr) = seq.next_element::<[u8; 16]>()? {
                    out.push(u128::from_be_bytes(arr));
                }
                Ok(out)
            }
        }
        de.deserialize_seq(VecU128Visitor)
    }
}
