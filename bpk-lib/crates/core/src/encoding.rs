//! Stable batpak encoding helpers.
//!
//! These helpers expose the crate's current named-field MessagePack bytes so
//! consumers can produce the same encoded extension bytes batpak stores and
//! signs. Public report-body bytes are patch-stable only where a schema version
//! and golden fixture pin that boundary. The implementation intentionally pins
//! the exact `rmp-serde` encoder version; bumping it requires refreshing the
//! relevant goldens and reviewing ADR-0019. This module does not implement
//! protocol-specific canonicalization such as JSON Canonicalization Scheme.

use serde::de::DeserializeOwned;
use serde::Serialize;

/// Encode `value` using batpak's stable named-field MessagePack surface.
///
/// # Errors
/// Returns any MessagePack encoding error reported by `rmp-serde`.
pub fn to_bytes<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    rmp_serde::to_vec_named(value)
}

/// Decode `value` from batpak's stable named-field MessagePack surface.
///
/// # Errors
/// Returns any MessagePack decoding error reported by `rmp-serde`.
pub fn from_bytes<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, rmp_serde::decode::Error> {
    rmp_serde::from_slice(bytes)
}
