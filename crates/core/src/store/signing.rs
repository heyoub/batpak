use crate::coordinate::Coordinate;
use crate::event::EventKind;
use crate::store::{AppendReceipt, DenialReceipt, ExtensionKey};
use ed25519_compact::{KeyPair, PublicKey, Seed, Signature};
use std::collections::BTreeMap;
use std::sync::Arc;
use zeroize::Zeroizing;

const COVER_VERSION_V1: u8 = 0x01;

/// Opt-in Ed25519 signing key for receipt signatures.
#[derive(Clone)]
pub struct SigningKey {
    seed: Zeroizing<[u8; 32]>,
}

impl SigningKey {
    /// Construct a signing key from 32 seed bytes.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self {
            seed: Zeroizing::new(bytes),
        }
    }

    pub(crate) fn key_id(&self) -> [u8; 32] {
        match self.public_key_bytes() {
            Some(bytes) => key_id_for_public_key(&bytes),
            None => [0; 32],
        }
    }

    fn key_pair(&self) -> KeyPair {
        KeyPair::from_seed(Seed::new(*self.seed))
    }

    fn public_key_bytes(&self) -> Option<[u8; 32]> {
        <[u8; 32]>::try_from(self.key_pair().pk.as_ref()).ok()
    }

    fn sign_cover(&self, cover: [u8; 32]) -> [u8; 64] {
        let signature = self.key_pair().sk.sign(cover, None);
        let mut bytes = [0u8; 64];
        bytes.copy_from_slice(signature.as_ref());
        bytes
    }
}

impl std::fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SigningKey")
            .field("key_id", &self.key_id())
            .finish()
    }
}

#[derive(Clone, Default)]
pub(crate) struct ReceiptSigningRegistry {
    current: Option<Arc<SigningKey>>,
    verifying_keys: Arc<BTreeMap<[u8; 32], [u8; 32]>>,
}

impl ReceiptSigningRegistry {
    pub(crate) fn from_keys(keys: &[SigningKey]) -> Self {
        let mut verifying_keys = BTreeMap::new();
        let mut current = None;
        for key in keys {
            let key = Arc::new(key.clone());
            if let Some(public_key_bytes) = key.public_key_bytes() {
                verifying_keys.insert(key.key_id(), public_key_bytes);
                current = Some(key);
            }
        }
        Self {
            current,
            verifying_keys: Arc::new(verifying_keys),
        }
    }

    pub(crate) fn sign_append_receipt(
        &self,
        receipt: &mut AppendReceipt,
        coord: &Coordinate,
        kind: EventKind,
        prev_hash: [u8; 32],
    ) {
        let cover = match cover_bytes(
            receipt.event_id,
            receipt.sequence,
            coord,
            kind,
            prev_hash,
            receipt.content_hash,
            &receipt.extensions,
        ) {
            Ok(cover) => cover,
            Err(error) => {
                tracing::error!(error = %error, "failed to build receipt signature cover");
                receipt.key_id = [0; 32];
                receipt.signature = None;
                return;
            }
        };
        if let Some(current) = &self.current {
            receipt.key_id = current.key_id();
            receipt.signature = Some(current.sign_cover(cover));
            return;
        }
        receipt.key_id = [0; 32];
        receipt.signature = None;
    }

    pub(crate) fn verify_append_receipt(
        &self,
        receipt: &AppendReceipt,
        coord: &Coordinate,
        kind: EventKind,
        prev_hash: [u8; 32],
    ) -> bool {
        let cover = match cover_bytes(
            receipt.event_id,
            receipt.sequence,
            coord,
            kind,
            prev_hash,
            receipt.content_hash,
            &receipt.extensions,
        ) {
            Ok(cover) => cover,
            Err(error) => {
                tracing::error!(error = %error, "failed to rebuild append receipt signature cover");
                return false;
            }
        };
        self.verify_signature(receipt.key_id, receipt.signature, cover)
    }

    pub(crate) fn verify_denial_receipt(
        &self,
        receipt: &DenialReceipt,
        coord: &Coordinate,
        kind: EventKind,
        prev_hash: [u8; 32],
    ) -> bool {
        let cover = match cover_bytes(
            receipt.event_id,
            receipt.sequence,
            coord,
            kind,
            prev_hash,
            receipt.content_hash,
            &receipt.extensions,
        ) {
            Ok(cover) => cover,
            Err(error) => {
                tracing::error!(error = %error, "failed to rebuild denial receipt signature cover");
                return false;
            }
        };
        self.verify_signature(receipt.key_id, receipt.signature, cover)
    }

    fn verify_signature(
        &self,
        key_id: [u8; 32],
        signature: Option<[u8; 64]>,
        cover: [u8; 32],
    ) -> bool {
        if signature.is_none() {
            return key_id == [0; 32];
        }
        if key_id == [0; 32] {
            return false;
        }
        let Some(signature_bytes) = signature else {
            return false;
        };
        let Some(public_key_bytes) = self.verifying_keys.get(&key_id) else {
            return false;
        };
        let signature = Signature::new(signature_bytes);
        PublicKey::new(*public_key_bytes)
            .verify(cover, &signature)
            .is_ok()
    }
}

#[derive(Debug)]
enum CoverBuildError {
    CoordinateEncoding(rmp_serde::encode::Error),
    ExtensionsEncoding(rmp_serde::encode::Error),
    #[cfg(not(feature = "blake3"))]
    Blake3Unavailable,
}

impl std::fmt::Display for CoverBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CoordinateEncoding(error) => {
                write!(
                    f,
                    "coordinate encoding failed while building receipt cover: {error}"
                )
            }
            Self::ExtensionsEncoding(error) => {
                write!(
                    f,
                    "extension encoding failed while building receipt cover: {error}"
                )
            }
            #[cfg(not(feature = "blake3"))]
            Self::Blake3Unavailable => {
                write!(
                    f,
                    "receipt signing requires the blake3 feature for cover hashing"
                )
            }
        }
    }
}

impl std::error::Error for CoverBuildError {}

fn key_id_for_public_key(public_key: &[u8; 32]) -> [u8; 32] {
    #[cfg(feature = "blake3")]
    {
        crate::event::hash::compute_hash(public_key)
    }
    #[cfg(not(feature = "blake3"))]
    {
        let _ = public_key;
        [0; 32]
    }
}

fn cover_bytes(
    event_id: u128,
    sequence: u64,
    coord: &Coordinate,
    kind: EventKind,
    prev_hash: [u8; 32],
    content_hash: [u8; 32],
    extensions: &BTreeMap<ExtensionKey, Vec<u8>>,
) -> Result<[u8; 32], CoverBuildError> {
    let mut cover = Vec::new();
    cover.push(COVER_VERSION_V1);
    cover.extend_from_slice(&event_id.to_le_bytes());
    cover.extend_from_slice(&sequence.to_le_bytes());
    let coord_bytes =
        crate::canonical::to_bytes(coord).map_err(CoverBuildError::CoordinateEncoding)?;
    cover.extend_from_slice(&coord_bytes);
    let raw_kind = (u16::from(kind.category()) << 12) | kind.type_id();
    cover.extend_from_slice(&raw_kind.to_le_bytes());
    cover.extend_from_slice(&prev_hash);
    cover.extend_from_slice(&content_hash);
    let extension_bytes =
        crate::canonical::to_bytes(extensions).map_err(CoverBuildError::ExtensionsEncoding)?;
    cover.extend_from_slice(&extension_bytes);
    digest_bytes(&cover)
}

#[cfg(feature = "blake3")]
fn digest_bytes(bytes: &[u8]) -> Result<[u8; 32], CoverBuildError> {
    Ok(crate::event::hash::compute_hash(bytes))
}

#[cfg(not(feature = "blake3"))]
fn digest_bytes(_bytes: &[u8]) -> Result<[u8; 32], CoverBuildError> {
    Err(CoverBuildError::Blake3Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "blake3")]
    #[test]
    fn cover_bytes_separates_event_kind_category_and_type_bits() {
        let coord = Coordinate::new("receipt:cover", "scope:test").expect("coordinate");
        let extensions = BTreeMap::new();

        let cover_a = cover_bytes(
            1,
            1,
            &coord,
            EventKind::custom(0xF, 0x055),
            [0x11; 32],
            [0x22; 32],
            &extensions,
        )
        .expect("cover A");
        let cover_b = cover_bytes(
            1,
            1,
            &coord,
            EventKind::custom(0xE, 0x055),
            [0x11; 32],
            [0x22; 32],
            &extensions,
        )
        .expect("cover B");
        let cover_c = cover_bytes(
            1,
            1,
            &coord,
            EventKind::custom(0xF, 0x056),
            [0x11; 32],
            [0x22; 32],
            &extensions,
        )
        .expect("cover C");

        assert_ne!(
            cover_a, cover_b,
            "PROPERTY: receipt signature cover must include the EventKind category bits"
        );
        assert_ne!(
            cover_a, cover_c,
            "PROPERTY: receipt signature cover must include the EventKind type-id bits"
        );
    }
}
