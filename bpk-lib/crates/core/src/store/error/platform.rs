use crate::store::stats::{
    LockLeafSymlinkProtection, MmapAdmissionSummary, MmapEvidence, ParentDirSyncAdmissionSummary,
    ParentDirSyncEvidence, StoreLockAdmissionSummary, StorePathStatusEvidence,
};

/// Store open mode for lifetime-held directory locking.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreLockMode {
    /// Mutable open: writer thread active, exclusive lock required.
    Mutable,
    /// Read-only open: no writer thread, but still exclusive under the
    /// current store-ownership contract.
    ReadOnly,
}

/// Typed reason a persisted platform profile was rejected.
#[derive(Debug)]
#[non_exhaustive]
pub enum ProfileInvalidKind {
    /// Reading the profile file failed.
    Io(std::io::Error),
    /// Decoding the JSON profile failed.
    DecodeJson(serde_json::Error),
    /// Encoding the canonical fingerprint body failed.
    FingerprintEncode(serde_json::Error),
    /// The profile schema version is not supported by this crate.
    UnsupportedSchemaVersion {
        /// Version observed in the profile.
        observed: u16,
        /// Version this crate accepts.
        expected: u16,
    },
    /// The stored fingerprint did not match the computed fingerprint.
    FingerprintMismatch {
        /// Fingerprint stored in the profile.
        observed: u32,
        /// Fingerprint computed from the profile body.
        computed: u32,
    },
    /// Store-lock admission contradicts the observed lock evidence.
    InconsistentLockAdmission {
        /// Admission recorded in the profile.
        admission: StoreLockAdmissionSummary,
        /// Evidence recorded in the profile.
        evidence: LockLeafSymlinkProtection,
    },
    /// Parent-directory sync admission contradicts the observed evidence.
    InconsistentParentDirSyncAdmission {
        /// Admission recorded in the profile.
        admission: ParentDirSyncAdmissionSummary,
        /// Evidence recorded in the profile.
        evidence: ParentDirSyncEvidence,
    },
    /// mmap evidence contradicts the store-path status.
    InconsistentMmapPath {
        /// Profile field whose evidence was inconsistent.
        field: &'static str,
        /// Evidence recorded in the profile.
        evidence: MmapEvidence,
        /// Evidence required by the path status.
        expected: MmapEvidence,
        /// Path status that determined the expected evidence.
        path_status: StorePathStatusEvidence,
    },
    /// mmap admission contradicts the observed mmap evidence.
    InconsistentMmapAdmission {
        /// Profile field whose admission was inconsistent.
        field: &'static str,
        /// Admission recorded in the profile.
        admission: MmapAdmissionSummary,
        /// Evidence recorded in the profile.
        evidence: MmapEvidence,
    },
}

impl std::fmt::Display for ProfileInvalidKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::DecodeJson(error) | Self::FingerprintEncode(error) => write!(f, "{error}"),
            Self::UnsupportedSchemaVersion { observed, expected } => write!(
                f,
                "schema_version {observed} is not supported; expected {expected}"
            ),
            Self::FingerprintMismatch { observed, computed } => write!(
                f,
                "fingerprint_crc32 {observed} does not match computed {computed}"
            ),
            Self::InconsistentLockAdmission {
                admission,
                evidence,
            } => write!(
                f,
                "store_lock admission {admission:?} is inconsistent with lock evidence {evidence:?}"
            ),
            Self::InconsistentParentDirSyncAdmission {
                admission,
                evidence,
            } => write!(
                f,
                "parent_dir_sync admission {admission:?} is inconsistent with evidence {evidence:?}"
            ),
            Self::InconsistentMmapPath {
                field,
                evidence,
                expected,
                path_status,
            } => write!(
                f,
                "{field} evidence {evidence:?} is inconsistent with path_status {path_status:?}; expected {expected:?}"
            ),
            Self::InconsistentMmapAdmission {
                field,
                admission,
                evidence,
            } => write!(
                f,
                "{field} admission {admission:?} is inconsistent with mmap evidence {evidence:?}"
            ),
        }
    }
}

impl ProfileInvalidKind {
    pub(super) fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::DecodeJson(error) | Self::FingerprintEncode(error) => Some(error),
            Self::UnsupportedSchemaVersion { .. }
            | Self::FingerprintMismatch { .. }
            | Self::InconsistentLockAdmission { .. }
            | Self::InconsistentParentDirSyncAdmission { .. }
            | Self::InconsistentMmapPath { .. }
            | Self::InconsistentMmapAdmission { .. } => None,
        }
    }
}
