use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::ser::Serializer;
use std::fmt;

/// Serde helpers for u128 serialization as [u8; 16] big-endian.
/// MessagePack has no native u128 type. Bare u128 causes rmp-serde errors.
/// Big-endian preserves sort order and is standard network byte order.
///
/// ZERO internal dependencies. This module is declared FIRST in lib.rs.
/// Every serializable type with a u128 field uses these helpers.
pub mod u128_bytes {
    /// Usage: #[serde(with = "crate::wire::u128_bytes")]
    /// Annotated on: EventHeader.event_id, EventHeader.correlation_id,
    ///   Notification.event_id, Notification.correlation_id,
    ///   Committed.event_id, WaitCondition::Event.event_id,
    ///   CompensationAction::Notify.target_id, Outcome::Pending.resume_token
    use super::*;

    /// Serialize a `u128` as 16 big-endian bytes into the given serializer.
    ///
    /// # Errors
    /// Returns `S::Error` if the underlying serializer fails to write the byte array.
    pub fn serialize<S: Serializer>(val: &u128, ser: S) -> Result<S::Ok, S::Error> {
        // Convert to 16-byte big-endian array, serialize as bytes.
        // [DEP:serde::Serializer::serialize_bytes]
        ser.serialize_bytes(&val.to_be_bytes())
    }

    /// Deserialize a `u128` from 16 big-endian bytes produced by the paired `serialize`.
    ///
    /// # Errors
    /// Returns `D::Error` if the input is not exactly 16 bytes or the deserializer fails.
    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<u128, D::Error> {
        // Accept bytes, convert from big-endian to u128.
        // Use a Visitor that handles both byte arrays and sequences.
        // [DEP:serde::de::Visitor]
        de.deserialize_bytes(U128Visitor)
    }

    /// Module-level visitor (was previously a local struct inside `deserialize`).
    /// Pulled out so unit tests can construct it directly and exercise every
    /// `Visitor` method — local-struct visitors are unreachable from tests
    /// and would be invisible to mutation testing.
    pub(super) struct U128Visitor;
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
}

/// Serde helpers for `Option<u128>` serialization as optional 16-byte big-endian arrays.
pub mod option_u128_bytes {
    /// Usage: #[serde(with = "crate::wire::option_u128_bytes")]
    /// Annotated on: EventHeader.causation_id, Notification.causation_id
    use super::*;

    /// Serialize an `Option<u128>` as `None` or 16 big-endian bytes.
    ///
    /// # Errors
    /// Returns `S::Error` if the underlying serializer fails.
    pub fn serialize<S: Serializer>(val: &Option<u128>, ser: S) -> Result<S::Ok, S::Error> {
        match val {
            Some(v) => ser.serialize_bytes(&v.to_be_bytes()),
            None => ser.serialize_none(),
        }
    }

    /// Deserialize an `Option<u128>` from `None` or 16 big-endian bytes.
    ///
    /// # Errors
    /// Returns `D::Error` if the input is present but not exactly 16 bytes, or the deserializer fails.
    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Option<u128>, D::Error> {
        // Visitor that handles None (nil) and Some(bytes).
        de.deserialize_option(OptU128Visitor)
    }

    /// Module-level visitor (was previously a local struct inside `deserialize`).
    /// Extracted so unit tests can construct it and exercise `visit_bytes` —
    /// the defensive-fallback path that rmp-serde never reaches via the
    /// normal `visit_some` + recursive `u128_bytes::deserialize` flow.
    pub(super) struct OptU128Visitor;
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
}

/// Serde helpers for `Vec<u128>` serialization as sequences of 16-byte big-endian arrays.
pub mod vec_u128_bytes {
    /// Usage: #[serde(with = "crate::wire::vec_u128_bytes")]
    /// Annotated on: CompensationAction::Rollback.event_ids,
    ///   CompensationAction::Release.resource_ids
    use super::*;

    /// Serialize a `Vec<u128>` as a sequence of 16-byte big-endian arrays.
    ///
    /// # Errors
    /// Returns `S::Error` if the underlying serializer fails to write the sequence.
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

    /// Deserialize a `Vec<u128>` from a sequence of 16-byte big-endian arrays.
    ///
    /// # Errors
    /// Returns `D::Error` if any element is not a valid 16-byte array or the deserializer fails.
    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<u128>, D::Error> {
        // Deserialize a sequence of [u8; 16] arrays back to Vec<u128>.
        de.deserialize_seq(VecU128Visitor)
    }

    /// Module-level visitor (was previously a local struct inside `deserialize`).
    /// Extracted so unit tests can construct it and exercise `expecting()`
    /// directly — local-struct visitors are unreachable from tests and would
    /// be invisible to mutation testing.
    pub(super) struct VecU128Visitor;
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
}

// ── Unit tests ────────────────────────────────────────────────────────────────
//
// These tests exist specifically to make every Visitor method visible to
// mutation testing. The previous (now-extracted) visitors were local structs
// inside the `deserialize` functions, so they were unreachable from any test
// and `cargo mutants` flagged 6 surviving mutations against their `expecting()`
// and `visit_bytes()` methods. After extracting the visitors to module level,
// these tests construct each visitor directly and call its methods with known
// inputs — every mutation now has a corresponding assertion that catches it.

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::Visitor;

    /// Lightweight de::Error implementation so we can call visit_bytes
    /// without needing a full Deserializer. The actual error contents
    /// don't matter — we just need a type that satisfies the trait.
    #[derive(Debug)]
    struct StubError(String);
    impl std::fmt::Display for StubError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0)
        }
    }
    impl std::error::Error for StubError {}
    impl de::Error for StubError {
        fn custom<T: std::fmt::Display>(msg: T) -> Self {
            StubError(msg.to_string())
        }
    }

    // ── u128_bytes::U128Visitor ────────────────────────────────────────────

    #[test]
    fn u128_visitor_expecting_writes_specific_message() {
        let mut buf = String::new();
        std::fmt::write(
            &mut buf,
            format_args!("{}", ExpectingDisplay(&u128_bytes::U128Visitor)),
        )
        .expect("write");
        assert_eq!(
            buf, "16 bytes for u128",
            "PROPERTY: U128Visitor::expecting must produce the canonical message. \
             A mutation that replaces the body with `Ok(Default::default())` \
             would produce an empty string and break this assertion."
        );
    }

    #[test]
    fn u128_visitor_visit_bytes_round_trips_known_value() {
        let value: u128 = 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10;
        let bytes = value.to_be_bytes();
        let result: Result<u128, StubError> = u128_bytes::U128Visitor.visit_bytes(&bytes);
        assert_eq!(
            result.expect("16-byte input must decode"),
            value,
            "PROPERTY: U128Visitor::visit_bytes must round-trip a known 16-byte big-endian \
             encoding back to the original u128. Mutations that hard-code 0 or default would \
             fail this assertion."
        );
    }

    #[test]
    fn u128_visitor_visit_bytes_rejects_wrong_length() {
        let result: Result<u128, StubError> = u128_bytes::U128Visitor.visit_bytes(&[0u8; 8]);
        assert!(
            result.is_err(),
            "PROPERTY: U128Visitor::visit_bytes must reject inputs that are not exactly 16 bytes."
        );
    }

    // ── option_u128_bytes::OptU128Visitor ──────────────────────────────────

    #[test]
    fn opt_u128_visitor_expecting_writes_specific_message() {
        let mut buf = String::new();
        std::fmt::write(
            &mut buf,
            format_args!("{}", ExpectingDisplay(&option_u128_bytes::OptU128Visitor)),
        )
        .expect("write");
        assert_eq!(
            buf, "null or 16 bytes for u128",
            "PROPERTY: OptU128Visitor::expecting must produce the canonical message."
        );
    }

    #[test]
    fn opt_u128_visitor_visit_bytes_returns_some_known_value() {
        // This is the path rmp-serde DOES NOT reach (it goes through
        // visit_some + recursive deserialize). The defensive fallback
        // exists for formats that emit bytes directly into an Option<T>
        // visitor — exercise it here so mutation testing has signal.
        let value: u128 = 0xDEAD_BEEF_CAFE_F00D_FEED_FACE_BAAD_F00D;
        let bytes = value.to_be_bytes();
        let result: Result<Option<u128>, StubError> =
            option_u128_bytes::OptU128Visitor.visit_bytes(&bytes);
        assert_eq!(
            result.expect("16-byte input must decode"),
            Some(value),
            "PROPERTY: OptU128Visitor::visit_bytes must round-trip a known 16-byte big-endian \
             encoding back to Some(u128). Mutations that return Ok(None), Ok(Some(0)), or \
             Ok(Some(1)) would all fail this assertion."
        );
    }

    #[test]
    fn opt_u128_visitor_visit_bytes_rejects_wrong_length() {
        let result: Result<Option<u128>, StubError> =
            option_u128_bytes::OptU128Visitor.visit_bytes(&[0u8; 4]);
        assert!(
            result.is_err(),
            "PROPERTY: OptU128Visitor::visit_bytes must reject inputs that are not exactly 16 bytes."
        );
    }

    #[test]
    fn opt_u128_visitor_visit_none_returns_none() {
        let result: Result<Option<u128>, StubError> =
            option_u128_bytes::OptU128Visitor.visit_none();
        assert_eq!(
            result.expect("visit_none never errors"),
            None,
            "PROPERTY: OptU128Visitor::visit_none must return Ok(None)."
        );
    }

    // ── vec_u128_bytes::VecU128Visitor ─────────────────────────────────────

    #[test]
    fn vec_u128_visitor_expecting_writes_specific_message() {
        let mut buf = String::new();
        std::fmt::write(
            &mut buf,
            format_args!("{}", ExpectingDisplay(&vec_u128_bytes::VecU128Visitor)),
        )
        .expect("write");
        assert_eq!(
            buf, "sequence of 16-byte u128 values",
            "PROPERTY: VecU128Visitor::expecting must produce the canonical message."
        );
    }

    /// Helper that adapts a Visitor's `expecting()` to a `Display` impl so
    /// we can format it without needing a real Deserializer error path.
    /// Visitor::expecting takes `&mut Formatter`, exactly the same shape as
    /// Display::fmt, so this adapter is a one-liner.
    struct ExpectingDisplay<'a, V: ?Sized>(&'a V);
    impl<'a, V: for<'de> serde::de::Visitor<'de>> std::fmt::Display for ExpectingDisplay<'a, V> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.expecting(f)
        }
    }
}
