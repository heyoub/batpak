/// Detailed outcome from receipt verification against the current store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReceiptVerification {
    /// The receipt signature verified against a configured signing key.
    Signed,
    /// The receipt was intentionally unsigned and the store has no verifying
    /// key registry, so unsigned admission is valid for this store.
    UnsignedAccepted,
    /// The receipt does not match the store's committed index/signing state.
    Invalid(ReceiptVerificationError),
}

impl ReceiptVerification {
    /// Return true when the receipt is valid for the current store state.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        matches!(self, Self::Signed | Self::UnsignedAccepted)
    }

    /// Return true only when the receipt carried a signature that verified
    /// against a configured signing key.
    ///
    /// Unlike [`is_valid`](Self::is_valid), an unsigned receipt that was
    /// accepted only because the store has no verifying keys returns `false`
    /// here. Callers that require cryptographic proof of authenticity (rather
    /// than "valid under this store's signing policy") must use this method.
    #[must_use]
    pub fn is_signed(&self) -> bool {
        matches!(self, Self::Signed)
    }

    /// Return the rejection reason, if verification failed.
    #[must_use]
    pub fn error(&self) -> Option<&ReceiptVerificationError> {
        if let Self::Invalid(error) = self {
            Some(error)
        } else {
            None
        }
    }
}

/// Reason a receipt failed verification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReceiptVerificationError {
    /// The receipt's event id is absent from the current store index.
    MissingCommittedEvent,
    /// The receipt event id differs from the committed index entry.
    EventIdMismatch,
    /// The receipt sequence differs from the committed index entry.
    SequenceMismatch,
    /// The receipt disk position differs from the committed index entry.
    DiskPositionMismatch,
    /// The receipt content hash differs from the committed index entry.
    ContentHashMismatch,
    /// The receipt extension cargo differs from the committed index entry.
    ExtensionsMismatch,
    /// A denial receipt points at an index entry that is not a system denial.
    DenialKindMismatch,
    /// The receipt is unsigned, but this store has verifying keys configured.
    UnsignedReceiptRejected,
    /// The receipt omitted a signature while naming a non-sentinel key id.
    MissingSignature,
    /// The receipt carried a signature while naming the unsigned sentinel key.
    ZeroKeyWithSignature,
    /// The receipt key id is not in this store's verifying-key registry.
    UnknownSigningKey,
    /// The receipt signature did not verify against its cover bytes.
    InvalidSignature,
    /// The signature cover could not be rebuilt from the committed entry.
    CoverBuildFailed {
        /// Human-readable encoding failure returned while rebuilding the cover.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::{ReceiptVerification, ReceiptVerificationError};

    #[test]
    fn is_signed_distinguishes_cryptographic_proof_from_mere_validity() {
        // A genuinely signed receipt is both signed and valid.
        assert!(ReceiptVerification::Signed.is_signed());
        assert!(ReceiptVerification::Signed.is_valid());

        // An unsigned receipt accepted only because the store has no verifying
        // keys is VALID for the store but carries NO cryptographic proof, so a
        // caller demanding authenticity must not be fooled by `is_valid`.
        assert!(!ReceiptVerification::UnsignedAccepted.is_signed());
        assert!(ReceiptVerification::UnsignedAccepted.is_valid());

        // A rejected receipt is neither signed nor valid.
        let invalid = ReceiptVerification::Invalid(ReceiptVerificationError::MissingSignature);
        assert!(!invalid.is_signed());
        assert!(!invalid.is_valid());
    }
}
