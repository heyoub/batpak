//! Content identity: the canonical-hash primitive plus the two digest newtypes
//! the host uses to name modules and whole compositions.
//!
//! Both digests are computed the same family-canonical way bvisor seals its plan
//! material: serialize a domain-separated view with batpak's canonical encoder,
//! then BLAKE3 it. Identical canonical inputs ⇒ identical digest; any change to
//! the declared bytes changes the digest.

use serde::Serialize;

use crate::error::HostError;

/// A 32-byte BLAKE3 digest over canonical bytes.
pub type Digest = [u8; 32];

/// Hash a domain-separated, canonically encoded view into a 32-byte digest.
///
/// # Errors
/// [`HostError::CanonicalEncoding`] if the canonical encoder rejects `value`
/// (unreachable for the crate's frozen wire shapes).
pub(crate) fn canonical_digest<T: Serialize>(value: &T) -> Result<Digest, HostError> {
    let bytes =
        batpak::canonical::to_bytes(value).map_err(|error| HostError::CanonicalEncoding {
            detail: error.to_string(),
        })?;
    Ok(batpak::event::hash::compute_hash(&bytes))
}

/// Render a 32-byte digest as lowercase hex.
fn hex(digest: &Digest) -> String {
    let mut out = String::with_capacity(64);
    for byte in digest {
        // Each byte renders as exactly two lowercase hex nibbles.
        out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    out
}

/// Content identity of a single mounted module: `H_module = H(canonical
/// manifest)` (see [`crate::manifest::HostModuleManifest`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleDigest(pub Digest);

impl ModuleDigest {
    /// The raw 32 bytes.
    #[must_use]
    pub const fn bytes(&self) -> &Digest {
        &self.0
    }

    /// Lowercase-hex rendering of the digest.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }
}

impl std::fmt::Display for ModuleDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Content identity of a whole host composition: `H_host = H("hostbat.host.v1" ‖
/// canonical(sorted (module-id, module-digest) pairs))`. Mount order does not
/// affect it; the mounted module *set* does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostFingerprint(pub Digest);

impl HostFingerprint {
    /// The raw 32 bytes.
    #[must_use]
    pub const fn bytes(&self) -> &Digest {
        &self.0
    }

    /// Lowercase-hex rendering of the fingerprint.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }
}

impl std::fmt::Display for HostFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Client-visible interface identity of a whole host composition:
/// `H_interface = H("hostbat.interface.v1" ‖ canonical(client-visible surface))`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InterfaceFingerprint(pub Digest);

impl InterfaceFingerprint {
    /// The raw 32 bytes.
    #[must_use]
    pub const fn bytes(&self) -> &Digest {
        &self.0
    }

    /// Lowercase-hex rendering of the fingerprint.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }
}

impl std::fmt::Display for InterfaceFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}
