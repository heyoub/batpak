use crate::store::stats::{
    ClockEvidence, LockLeafSymlinkProtection, MmapAdmissionSummary, MmapEvidence,
    ParentDirSyncAdmissionSummary, ParentDirSyncEvidence, PlatformAdmissionSummary,
    PlatformEvidenceSummary, StoreLockAdmissionSummary, StorePathEvidenceSummary,
    StorePathStatusEvidence,
};
use crate::store::StoreError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub(crate) const PLATFORM_PROFILE_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PlatformProfile {
    pub(crate) schema_version: u16,
    pub(crate) host: PlatformProfileHost,
    pub(crate) store_path: StorePathEvidenceSummary,
    pub(crate) admission: PlatformAdmissionSummary,
    pub(crate) fingerprint_crc32: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PlatformProfileHost {
    pub(crate) monotonic_clock: ClockEvidence,
}

#[derive(Serialize)]
struct PlatformProfileBody<'a> {
    schema_version: u16,
    host: &'a PlatformProfileHost,
    store_path: &'a StorePathEvidenceSummary,
    admission: &'a PlatformAdmissionSummary,
}

impl PlatformProfile {
    pub(crate) fn from_evidence(evidence: &PlatformEvidenceSummary) -> Result<Self, StoreError> {
        let host = PlatformProfileHost {
            monotonic_clock: evidence.host.monotonic_clock,
        };
        let mut profile = Self {
            schema_version: PLATFORM_PROFILE_SCHEMA_VERSION,
            host,
            store_path: evidence.store_path.clone(),
            admission: evidence.admission.clone(),
            fingerprint_crc32: 0,
        };
        profile.fingerprint_crc32 =
            profile
                .compute_fingerprint()
                .map_err(|reason| StoreError::PlatformProfileInvalid {
                    path: PathBuf::from("<generated>"),
                    reason,
                })?;
        Ok(profile)
    }

    pub(crate) fn load(path: &Path) -> Result<Self, StoreError> {
        let bytes = std::fs::read(path).map_err(|error| StoreError::PlatformProfileInvalid {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
        let profile: Self =
            serde_json::from_slice(&bytes).map_err(|error| StoreError::PlatformProfileInvalid {
                path: path.to_path_buf(),
                reason: error.to_string(),
            })?;
        profile.validate_fingerprint(path)?;
        Ok(profile)
    }

    #[cfg(test)]
    fn from_store_path_for_test(data_dir: &Path) -> Result<Self, StoreError> {
        let evidence = crate::store::platform::evidence::collect_for_store_path(data_dir);
        Self::from_evidence(&evidence)
    }

    pub(crate) fn verify_current_store_path(
        profile_path: &Path,
        data_dir: &Path,
    ) -> Result<PlatformEvidenceSummary, StoreError> {
        let expected = Self::load(profile_path)?;
        let current_evidence = crate::store::platform::evidence::collect_for_store_path(data_dir);
        let current = Self::from_evidence(&current_evidence).map_err(|error| {
            StoreError::PlatformProfileInvalid {
                path: profile_path.to_path_buf(),
                reason: error.to_string(),
            }
        })?;
        if expected.profile_body_tuple() != current.profile_body_tuple() {
            return Err(StoreError::PlatformProfileMismatch {
                path: profile_path.to_path_buf(),
                reason: format!(
                    "expected {:?}, observed {:?}",
                    expected.profile_body_tuple(),
                    current.profile_body_tuple()
                ),
            });
        }
        Ok(current_evidence)
    }

    fn validate_fingerprint(&self, path: &Path) -> Result<(), StoreError> {
        if self.schema_version != PLATFORM_PROFILE_SCHEMA_VERSION {
            return Err(StoreError::PlatformProfileInvalid {
                path: path.to_path_buf(),
                reason: format!(
                    "schema_version {} is not supported; expected {}",
                    self.schema_version, PLATFORM_PROFILE_SCHEMA_VERSION
                ),
            });
        }
        let computed =
            self.compute_fingerprint()
                .map_err(|reason| StoreError::PlatformProfileInvalid {
                    path: path.to_path_buf(),
                    reason,
                })?;
        if self.fingerprint_crc32 != computed {
            return Err(StoreError::PlatformProfileInvalid {
                path: path.to_path_buf(),
                reason: format!(
                    "fingerprint_crc32 {} does not match computed {}",
                    self.fingerprint_crc32, computed
                ),
            });
        }
        self.validate_admission_semantics(path)?;
        Ok(())
    }

    fn validate_admission_semantics(&self, path: &Path) -> Result<(), StoreError> {
        let expected_store_lock = match self.store_path.lock_leaf_symlink_protection {
            LockLeafSymlinkProtection::AtomicNoFollow => StoreLockAdmissionSummary::AtomicNoFollow,
            LockLeafSymlinkProtection::BestEffortCheckThenOpen => {
                StoreLockAdmissionSummary::BestEffortCheckThenOpen
            }
            LockLeafSymlinkProtection::Unknown
            | LockLeafSymlinkProtection::ObservedUnsupported
            | LockLeafSymlinkProtection::ProbeFailed => StoreLockAdmissionSummary::Rejected,
        };
        if self.admission.store_lock != expected_store_lock {
            return Err(StoreError::PlatformProfileInvalid {
                path: path.to_path_buf(),
                reason: format!(
                    "store_lock admission {:?} is inconsistent with lock evidence {:?}",
                    self.admission.store_lock, self.store_path.lock_leaf_symlink_protection
                ),
            });
        }

        let expected_parent_dir_sync = match self.store_path.parent_dir_sync {
            ParentDirSyncEvidence::UnixFsync => ParentDirSyncAdmissionSummary::UnixFsync,
            ParentDirSyncEvidence::RenameOnly => ParentDirSyncAdmissionSummary::RenameOnly,
            ParentDirSyncEvidence::Unknown | ParentDirSyncEvidence::ProbeFailed => {
                ParentDirSyncAdmissionSummary::Rejected
            }
        };
        if self.admission.parent_dir_sync != expected_parent_dir_sync {
            return Err(StoreError::PlatformProfileInvalid {
                path: path.to_path_buf(),
                reason: format!(
                    "parent_dir_sync admission {:?} is inconsistent with evidence {:?}",
                    self.admission.parent_dir_sync, self.store_path.parent_dir_sync
                ),
            });
        }

        self.validate_path_mmap_consistency(path, "mmap_index", self.store_path.mmap_index)?;
        self.validate_path_mmap_consistency(
            path,
            "sealed_segment_mmap",
            self.store_path.sealed_segment_mmap,
        )?;
        self.validate_mmap_admission(
            path,
            "mmap_index",
            self.store_path.mmap_index,
            self.admission.mmap_index,
        )?;
        self.validate_mmap_admission(
            path,
            "sealed_segment_mmap",
            self.store_path.sealed_segment_mmap,
            self.admission.sealed_segment_mmap,
        )
    }

    fn validate_path_mmap_consistency(
        &self,
        path: &Path,
        field: &'static str,
        evidence: MmapEvidence,
    ) -> Result<(), StoreError> {
        let required = match self.store_path.path_status {
            StorePathStatusEvidence::ObservedDirectory => return Ok(()),
            StorePathStatusEvidence::ObservedUnsupportedNotDirectory => {
                MmapEvidence::ObservedUnsupported
            }
            StorePathStatusEvidence::UnknownMissing => MmapEvidence::Unknown,
            StorePathStatusEvidence::ProbeFailed { .. } => MmapEvidence::ProbeFailed,
        };
        if evidence != required {
            return Err(StoreError::PlatformProfileInvalid {
                path: path.to_path_buf(),
                reason: format!(
                    "{field} evidence {evidence:?} is inconsistent with path_status {:?}; expected {required:?}",
                    self.store_path.path_status
                ),
            });
        }
        Ok(())
    }

    fn validate_mmap_admission(
        &self,
        path: &Path,
        field: &'static str,
        evidence: MmapEvidence,
        admission: MmapAdmissionSummary,
    ) -> Result<(), StoreError> {
        let expected = match evidence {
            MmapEvidence::FileBacked => MmapAdmissionSummary::FileBacked,
            MmapEvidence::Unknown
            | MmapEvidence::ObservedUnsupported
            | MmapEvidence::ProbeFailed => MmapAdmissionSummary::Rejected,
        };
        if admission != expected {
            return Err(StoreError::PlatformProfileInvalid {
                path: path.to_path_buf(),
                reason: format!(
                    "{field} admission {admission:?} is inconsistent with mmap evidence {evidence:?}"
                ),
            });
        }
        Ok(())
    }

    fn compute_fingerprint(&self) -> Result<u32, String> {
        let body = PlatformProfileBody {
            schema_version: self.schema_version,
            host: &self.host,
            store_path: &self.store_path,
            admission: &self.admission,
        };
        let bytes = serde_json::to_vec(&body).map_err(|error| error.to_string())?;
        Ok(crc32fast::hash(&bytes))
    }

    fn profile_body_tuple(
        &self,
    ) -> (
        u16,
        &PlatformProfileHost,
        &StorePathEvidenceSummary,
        &PlatformAdmissionSummary,
    ) {
        (
            self.schema_version,
            &self.host,
            &self.store_path,
            &self.admission,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::PlatformProfile;
    use crate::store::StoreError;
    use std::error::Error;
    use tempfile::TempDir;

    type TestResult = Result<(), Box<dyn Error>>;

    #[test]
    fn platform_profile_round_trips_with_valid_fingerprint() -> TestResult {
        let dir = TempDir::new()?;
        let profile = PlatformProfile::from_store_path_for_test(dir.path())?;
        let path = dir.path().join("platform.profile.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&profile)?)?;

        let loaded = PlatformProfile::load(&path)?;
        assert_eq!(profile, loaded);
        Ok(())
    }

    #[test]
    fn platform_profile_mismatch_fails_closed() -> TestResult {
        let dir = TempDir::new()?;
        let profile = PlatformProfile::from_store_path_for_test(dir.path())?;
        let path = dir.path().join("platform.profile.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&profile)?)?;
        let missing = dir.path().join("missing-store-path");

        let Err(error) = PlatformProfile::verify_current_store_path(&path, &missing) else {
            return Err(std::io::Error::other("expected profile mismatch").into());
        };
        assert!(matches!(error, StoreError::PlatformProfileMismatch { .. }));
        Ok(())
    }
}
