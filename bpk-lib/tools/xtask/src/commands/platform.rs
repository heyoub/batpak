use super::integrity;
use crate::{PlatformArgs, PlatformCommand};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

const PLATFORM_PROFILE_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct PlatformProfile {
    schema_version: u16,
    host: PlatformProfileHost,
    store_path: StorePathProfile,
    admission: PlatformAdmissionProfile,
    fingerprint_crc32: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct PlatformProfileHost {
    monotonic_clock: PlatformClockEvidence,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct StorePathProfile {
    path_status: PlatformStorePathStatus,
    parent_dir_sync: PlatformParentDirSyncEvidence,
    lock_leaf_symlink_protection: PlatformLockLeafSymlinkProtection,
    mmap_index: PlatformMmapEvidence,
    sealed_segment_mmap: PlatformMmapEvidence,
    active_segment_read: PlatformActiveSegmentReadEvidence,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum PlatformStorePathStatus {
    ObservedDirectory,
    UnknownMissing,
    ObservedUnsupportedNotDirectory,
    ProbeFailed { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct PlatformAdmissionProfile {
    store_lock: PlatformStoreLockAdmission,
    parent_dir_sync: PlatformParentDirSyncAdmission,
    mmap_index: PlatformMmapAdmission,
    sealed_segment_mmap: PlatformMmapAdmission,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum PlatformClockEvidence {
    ProcessLocalInstantAnchor,
    Unknown,
    ProbeFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum PlatformParentDirSyncEvidence {
    UnixFsync,
    RenameOnly,
    Unknown,
    ProbeFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum PlatformLockLeafSymlinkProtection {
    AtomicNoFollow,
    BestEffortCheckThenOpen,
    Unknown,
    ObservedUnsupported,
    ProbeFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum PlatformMmapEvidence {
    FileBacked,
    Unknown,
    ObservedUnsupported,
    ProbeFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum PlatformActiveSegmentReadEvidence {
    UnixReadAt,
    LockedSeekRead,
    Unknown,
    ProbeFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum PlatformStoreLockAdmission {
    AtomicNoFollow,
    BestEffortCheckThenOpen,
    Rejected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum PlatformParentDirSyncAdmission {
    UnixFsync,
    RenameOnly,
    Rejected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum PlatformMmapAdmission {
    FileBacked,
    Rejected,
}

#[derive(Serialize)]
struct PlatformProfileBody<'a> {
    schema_version: u16,
    host: &'a PlatformProfileHost,
    store_path: &'a StorePathProfile,
    admission: &'a PlatformAdmissionProfile,
}

pub(crate) fn platform(args: PlatformArgs) -> Result<()> {
    match args.command {
        PlatformCommand::Doctor(args) => {
            let profile = collect_platform_profile(&args.store_path)?;
            outln!(
                "platform doctor ok: store_path={} fingerprint_crc32={}",
                args.store_path.display(),
                profile.fingerprint_crc32
            );
            Ok(())
        }
        PlatformCommand::Probe(args) | PlatformCommand::Bless(args) => {
            fs::create_dir_all(&args.store_path)
                .with_context(|| format!("create store path {}", args.store_path.display()))?;
            let profile = collect_platform_profile(&args.store_path)?;
            if let Some(parent) = args.profile.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
            let bytes = serde_json::to_vec_pretty(&profile)?;
            fs::write(&args.profile, bytes)
                .with_context(|| format!("write {}", args.profile.display()))?;
            outln!(
                "wrote platform profile {} for store_path={}",
                args.profile.display(),
                args.store_path.display()
            );
            Ok(())
        }
        PlatformCommand::Verify(args) => {
            let expected = read_platform_profile(&args.profile)?;
            let observed = collect_platform_profile(&args.store_path)?;
            if profile_body_tuple(&expected) != profile_body_tuple(&observed) {
                bail!(
                    "platform profile mismatch for {}: expected {:?}, observed {:?}",
                    args.profile.display(),
                    profile_body_tuple(&expected),
                    profile_body_tuple(&observed)
                );
            }
            outln!(
                "platform profile verified: {} fingerprint_crc32={}",
                args.profile.display(),
                expected.fingerprint_crc32
            );
            Ok(())
        }
        PlatformCommand::Audit => integrity("structural-check", []),
    }
}

fn collect_platform_profile(store_path: &Path) -> Result<PlatformProfile> {
    let parent_dir_sync = parent_dir_sync_profile();
    let lock_leaf_symlink_protection = lock_leaf_profile();
    let mmap_evidence = mmap_profile(store_path);
    let store_path = StorePathProfile {
        path_status: path_status_profile(store_path),
        parent_dir_sync,
        lock_leaf_symlink_protection,
        mmap_index: mmap_evidence,
        sealed_segment_mmap: mmap_evidence,
        active_segment_read: active_read_profile(),
    };
    let admission = PlatformAdmissionProfile {
        store_lock: store_lock_admission(lock_leaf_symlink_protection),
        parent_dir_sync: parent_dir_sync_admission(parent_dir_sync),
        mmap_index: mmap_admission(mmap_evidence),
        sealed_segment_mmap: mmap_admission(mmap_evidence),
    };
    let mut profile = PlatformProfile {
        schema_version: PLATFORM_PROFILE_SCHEMA_VERSION,
        host: PlatformProfileHost {
            monotonic_clock: PlatformClockEvidence::ProcessLocalInstantAnchor,
        },
        store_path,
        admission,
        fingerprint_crc32: 0,
    };
    profile.fingerprint_crc32 = platform_profile_fingerprint(&profile)?;
    Ok(profile)
}

fn read_platform_profile(path: &Path) -> Result<PlatformProfile> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let profile: PlatformProfile =
        serde_json::from_slice(&bytes).with_context(|| format!("decode {}", path.display()))?;
    if profile.schema_version != PLATFORM_PROFILE_SCHEMA_VERSION {
        bail!(
            "unsupported platform profile schema_version {}; expected {}",
            profile.schema_version,
            PLATFORM_PROFILE_SCHEMA_VERSION
        );
    }
    validate_platform_profile_semantics(&profile)
        .with_context(|| format!("validate {}", path.display()))?;
    let computed = platform_profile_fingerprint(&profile)?;
    if computed != profile.fingerprint_crc32 {
        bail!(
            "platform profile {} fingerprint mismatch: stored {}, computed {}",
            path.display(),
            profile.fingerprint_crc32,
            computed
        );
    }
    Ok(profile)
}

fn validate_platform_profile_semantics(profile: &PlatformProfile) -> Result<()> {
    let expected_store_lock = store_lock_admission(profile.store_path.lock_leaf_symlink_protection);
    if profile.admission.store_lock != expected_store_lock {
        bail!(
            "inconsistent store_lock admission {:?}; expected {:?} from lock evidence {:?}",
            profile.admission.store_lock,
            expected_store_lock,
            profile.store_path.lock_leaf_symlink_protection
        );
    }
    let expected_parent_dir_sync = parent_dir_sync_admission(profile.store_path.parent_dir_sync);
    if profile.admission.parent_dir_sync != expected_parent_dir_sync {
        bail!(
            "inconsistent parent_dir_sync admission {:?}; expected {:?} from evidence {:?}",
            profile.admission.parent_dir_sync,
            expected_parent_dir_sync,
            profile.store_path.parent_dir_sync
        );
    }
    validate_path_mmap_profile_consistency("mmap_index", profile)?;
    validate_path_mmap_profile_consistency("sealed_segment_mmap", profile)?;
    validate_mmap_profile_admission(
        "mmap_index",
        profile.store_path.mmap_index,
        profile.admission.mmap_index,
    )?;
    validate_mmap_profile_admission(
        "sealed_segment_mmap",
        profile.store_path.sealed_segment_mmap,
        profile.admission.sealed_segment_mmap,
    )?;
    Ok(())
}

fn validate_path_mmap_profile_consistency(field: &str, profile: &PlatformProfile) -> Result<()> {
    let evidence = match field {
        "mmap_index" => profile.store_path.mmap_index,
        "sealed_segment_mmap" => profile.store_path.sealed_segment_mmap,
        _ => bail!("internal platform profile validation bug: unknown mmap field {field}"),
    };
    let required = match profile.store_path.path_status {
        PlatformStorePathStatus::ObservedDirectory => return Ok(()),
        PlatformStorePathStatus::ObservedUnsupportedNotDirectory => {
            PlatformMmapEvidence::ObservedUnsupported
        }
        PlatformStorePathStatus::UnknownMissing => PlatformMmapEvidence::Unknown,
        PlatformStorePathStatus::ProbeFailed { .. } => PlatformMmapEvidence::ProbeFailed,
    };
    if evidence != required {
        bail!(
            "inconsistent {field} evidence {evidence:?}; expected {required:?} from path_status {:?}",
            profile.store_path.path_status
        );
    }
    Ok(())
}

fn validate_mmap_profile_admission(
    field: &str,
    evidence: PlatformMmapEvidence,
    admission: PlatformMmapAdmission,
) -> Result<()> {
    let expected = match evidence {
        PlatformMmapEvidence::FileBacked => PlatformMmapAdmission::FileBacked,
        PlatformMmapEvidence::Unknown
        | PlatformMmapEvidence::ObservedUnsupported
        | PlatformMmapEvidence::ProbeFailed => PlatformMmapAdmission::Rejected,
    };
    if admission != expected {
        bail!(
            "inconsistent {field} admission {admission:?}; expected {expected:?} from mmap evidence {evidence:?}"
        );
    }
    Ok(())
}

fn platform_profile_fingerprint(profile: &PlatformProfile) -> Result<u32> {
    let body = PlatformProfileBody {
        schema_version: profile.schema_version,
        host: &profile.host,
        store_path: &profile.store_path,
        admission: &profile.admission,
    };
    let bytes = serde_json::to_vec(&body)?;
    Ok(crc32fast::hash(&bytes))
}

fn profile_body_tuple(
    profile: &PlatformProfile,
) -> (
    u16,
    &PlatformProfileHost,
    &StorePathProfile,
    &PlatformAdmissionProfile,
) {
    (
        profile.schema_version,
        &profile.host,
        &profile.store_path,
        &profile.admission,
    )
}

fn path_status_profile(store_path: &Path) -> PlatformStorePathStatus {
    match fs::metadata(store_path) {
        Ok(metadata) if metadata.is_dir() => PlatformStorePathStatus::ObservedDirectory,
        Ok(_) => PlatformStorePathStatus::ObservedUnsupportedNotDirectory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            PlatformStorePathStatus::UnknownMissing
        }
        Err(error) => PlatformStorePathStatus::ProbeFailed {
            reason: error.to_string(),
        },
    }
}

fn parent_dir_sync_profile() -> PlatformParentDirSyncEvidence {
    if cfg!(unix) {
        PlatformParentDirSyncEvidence::UnixFsync
    } else {
        PlatformParentDirSyncEvidence::RenameOnly
    }
}

fn lock_leaf_profile() -> PlatformLockLeafSymlinkProtection {
    if cfg!(unix) {
        PlatformLockLeafSymlinkProtection::AtomicNoFollow
    } else {
        PlatformLockLeafSymlinkProtection::BestEffortCheckThenOpen
    }
}

fn active_read_profile() -> PlatformActiveSegmentReadEvidence {
    if cfg!(unix) {
        PlatformActiveSegmentReadEvidence::UnixReadAt
    } else {
        PlatformActiveSegmentReadEvidence::LockedSeekRead
    }
}

fn mmap_profile(store_path: &Path) -> PlatformMmapEvidence {
    match path_status_profile(store_path) {
        PlatformStorePathStatus::ObservedDirectory => {}
        PlatformStorePathStatus::ObservedUnsupportedNotDirectory => {
            return PlatformMmapEvidence::ObservedUnsupported;
        }
        PlatformStorePathStatus::UnknownMissing => return PlatformMmapEvidence::Unknown,
        PlatformStorePathStatus::ProbeFailed { .. } => return PlatformMmapEvidence::ProbeFailed,
    }

    let Ok(mut probe) = tempfile::NamedTempFile::new_in(store_path) else {
        return PlatformMmapEvidence::ProbeFailed;
    };
    if std::io::Write::write_all(&mut probe, &[0])
        .and_then(|()| std::io::Write::flush(&mut probe))
        .is_err()
    {
        return PlatformMmapEvidence::ProbeFailed;
    }
    // SAFETY: this maps a private one-byte temp file to observe whether the
    // target supports file-backed mmap. It does not establish store semantics.
    match unsafe { memmap2::MmapOptions::new().len(1).map(probe.as_file()) } {
        Ok(_map) => PlatformMmapEvidence::FileBacked,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::Unsupported | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            PlatformMmapEvidence::ObservedUnsupported
        }
        Err(_) => PlatformMmapEvidence::ProbeFailed,
    }
}

fn store_lock_admission(evidence: PlatformLockLeafSymlinkProtection) -> PlatformStoreLockAdmission {
    match evidence {
        PlatformLockLeafSymlinkProtection::AtomicNoFollow => {
            PlatformStoreLockAdmission::AtomicNoFollow
        }
        PlatformLockLeafSymlinkProtection::BestEffortCheckThenOpen => {
            PlatformStoreLockAdmission::BestEffortCheckThenOpen
        }
        PlatformLockLeafSymlinkProtection::Unknown
        | PlatformLockLeafSymlinkProtection::ObservedUnsupported
        | PlatformLockLeafSymlinkProtection::ProbeFailed => PlatformStoreLockAdmission::Rejected,
    }
}

fn mmap_admission(evidence: PlatformMmapEvidence) -> PlatformMmapAdmission {
    match evidence {
        PlatformMmapEvidence::FileBacked => PlatformMmapAdmission::FileBacked,
        PlatformMmapEvidence::Unknown
        | PlatformMmapEvidence::ObservedUnsupported
        | PlatformMmapEvidence::ProbeFailed => PlatformMmapAdmission::Rejected,
    }
}

fn parent_dir_sync_admission(
    evidence: PlatformParentDirSyncEvidence,
) -> PlatformParentDirSyncAdmission {
    match evidence {
        PlatformParentDirSyncEvidence::UnixFsync => PlatformParentDirSyncAdmission::UnixFsync,
        PlatformParentDirSyncEvidence::RenameOnly => PlatformParentDirSyncAdmission::RenameOnly,
        PlatformParentDirSyncEvidence::Unknown | PlatformParentDirSyncEvidence::ProbeFailed => {
            PlatformParentDirSyncAdmission::Rejected
        }
    }
}

#[cfg(test)]
mod tests {
    use super::read_platform_profile;
    use std::path::Path;

    #[test]
    fn platform_profile_fixtures_stay_xtask_readable() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("repo root");
        for fixture in [
            "crates/core/tests/fixtures/platform/linux_basic.profile",
            "crates/core/tests/fixtures/platform/non_unix_best_effort_lock.profile",
            "crates/core/tests/fixtures/platform/mmap_unavailable.profile",
            "crates/core/tests/fixtures/platform/profile_mismatch.profile",
        ] {
            read_platform_profile(&repo_root.join(fixture)).expect("fixture profile must decode");
        }
    }
}
