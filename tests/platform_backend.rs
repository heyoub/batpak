// justifies: INV-TEST-PANIC-AS-ASSERTION; platform backend integration tests use panic/expect assertion style to prove fail-closed open behavior and diagnostics without hiding errors.
#![allow(clippy::panic, clippy::unwrap_used)]
//! Store platform backend surface.
//! Harness pattern: State-Machine Harness (profile/admission/open failure lane).
//!
//! PROVES: platform evidence stays descriptive, admission posture is reported
//! through diagnostics, and configured profile mismatch fails open before writer
//! spawn or successful-open observability.
//! CATCHES: profile/reverify drift that downgrades mismatch into warning-only
//! behavior, admits the wrong lock/sync/mmap posture, or appends lifecycle
//! success after a failed platform admission.
//! SEEDED: not random; deterministic tempdir-based opens and checked-in profile
//! fixtures.

use batpak::store::{
    ActiveSegmentReadEvidence, ClockEvidence, HostEvidenceSummary, LockLeafSymlinkProtection,
    MmapAdmissionSummary, MmapEvidence, OpenIndexReport, OpenReportObserver,
    ParentDirSyncAdmissionSummary, ParentDirSyncEvidence, PlatformAdmissionSummary,
    PlatformEvidenceSummary, Store, StoreConfig, StoreDiagnostics, StoreError,
    StoreLockAdmissionSummary, StorePathEvidenceSummary, StorePathStatusEvidence,
};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

fn test_store() -> (Store, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_segment_max_bytes(4096)
            .with_sync_every_n_events(1),
    )
    .expect("open store");
    (store, dir)
}

#[test]
fn diagnostics_reports_config() {
    let (store, dir) = test_store();
    let diag: StoreDiagnostics = store.diagnostics();
    let expected_data_dir = std::fs::canonicalize(dir.path()).expect("canonical temp dir");

    assert_eq!(diag.data_dir, expected_data_dir);
    assert_eq!(diag.segment_max_bytes, 4096);
    assert_eq!(diag.event_count, 1);

    let platform: PlatformEvidenceSummary = diag.platform_evidence.clone();
    let host: HostEvidenceSummary = platform.host;
    assert!(host.process_clock_epoch_marker_ns > 0);
    assert_eq!(
        host.monotonic_clock,
        ClockEvidence::ProcessLocalInstantAnchor
    );

    let store_path: StorePathEvidenceSummary = platform.store_path;
    assert_eq!(
        store_path.path_status,
        StorePathStatusEvidence::ObservedDirectory
    );
    assert_eq!(store_path.mmap_index, MmapEvidence::FileBacked);
    assert_eq!(store_path.sealed_segment_mmap, MmapEvidence::FileBacked);

    let admission: PlatformAdmissionSummary = platform.admission;
    assert_eq!(admission.mmap_index, MmapAdmissionSummary::FileBacked);
    assert_eq!(
        admission.sealed_segment_mmap,
        MmapAdmissionSummary::FileBacked
    );

    #[cfg(unix)]
    {
        assert_eq!(store_path.parent_dir_sync, ParentDirSyncEvidence::UnixFsync);
        assert_eq!(
            admission.parent_dir_sync,
            ParentDirSyncAdmissionSummary::UnixFsync
        );
        assert_eq!(
            store_path.lock_leaf_symlink_protection,
            LockLeafSymlinkProtection::AtomicNoFollow
        );
        assert_eq!(
            admission.store_lock,
            StoreLockAdmissionSummary::AtomicNoFollow
        );
        assert_eq!(
            store_path.active_segment_read,
            ActiveSegmentReadEvidence::UnixReadAt
        );
    }
    #[cfg(not(unix))]
    {
        assert_eq!(
            store_path.parent_dir_sync,
            ParentDirSyncEvidence::RenameOnly
        );
        assert_eq!(
            admission.parent_dir_sync,
            ParentDirSyncAdmissionSummary::RenameOnly
        );
        assert_eq!(
            store_path.lock_leaf_symlink_protection,
            LockLeafSymlinkProtection::BestEffortCheckThenOpen
        );
        assert_eq!(
            admission.store_lock,
            StoreLockAdmissionSummary::BestEffortCheckThenOpen
        );
        assert_eq!(
            store_path.active_segment_read,
            ActiveSegmentReadEvidence::LockedSeekRead
        );
    }

    store.close().expect("close");
}

#[test]
fn platform_profile_invalid_fails_before_open_completed() {
    let dir = TempDir::new().expect("temp dir");
    let profile_path = dir.path().join("bad-platform-profile.json");
    std::fs::write(&profile_path, b"{not json").expect("write bad profile");

    let err = match Store::open(
        StoreConfig::new(dir.path())
            .with_segment_max_bytes(4096)
            .with_platform_profile_path(profile_path.clone()),
    ) {
        Ok(_) => panic!("PROPERTY: invalid platform profile must fail open"),
        Err(error) => error,
    };
    assert!(
        matches!(
            err,
            StoreError::PlatformProfileInvalid { ref path, .. } if path == &profile_path
        ),
        "expected PlatformProfileInvalid for configured profile, got {err:?}"
    );
    assert!(
        !dir.path().join("000000.fbat").exists(),
        "profile reverify must fail before writer spawn or lifecycle append"
    );
}

#[test]
fn missing_platform_profile_reports_profile_invalid() {
    let dir = TempDir::new().expect("temp dir");
    let profile_path = dir.path().join("missing-platform-profile.json");

    let err = match Store::open(
        StoreConfig::new(dir.path())
            .with_segment_max_bytes(4096)
            .with_platform_profile_path(profile_path.clone()),
    ) {
        Ok(_) => panic!("PROPERTY: missing platform profile must fail open"),
        Err(error) => error,
    };
    assert!(
        matches!(
            err,
            StoreError::PlatformProfileInvalid { ref path, .. } if path == &profile_path
        ),
        "expected PlatformProfileInvalid for missing configured profile, got {err:?}"
    );
}

#[test]
fn impossible_path_mmap_profile_reports_profile_invalid() {
    let dir = TempDir::new().expect("temp dir");
    let profile_path = dir.path().join("impossible-platform-profile.json");
    let body = concat!(
        r#"{"schema_version":1,"#,
        r#""host":{"monotonic_clock":"ProcessLocalInstantAnchor"},"#,
        r#""store_path":{"path_status":"UnknownMissing","parent_dir_sync":"UnixFsync","lock_leaf_symlink_protection":"AtomicNoFollow","mmap_index":"FileBacked","sealed_segment_mmap":"FileBacked","active_segment_read":"UnixReadAt"},"#,
        r#""admission":{"store_lock":"AtomicNoFollow","parent_dir_sync":"UnixFsync","mmap_index":"FileBacked","sealed_segment_mmap":"FileBacked"}}"#
    );
    let fingerprint = crc32fast::hash(body.as_bytes());
    let profile = format!(
        "{}{}{}",
        body.trim_end_matches('}'),
        r#","fingerprint_crc32":"#,
        fingerprint
    ) + "}";
    std::fs::write(&profile_path, profile).expect("write impossible profile");

    let err = match Store::open(
        StoreConfig::new(dir.path())
            .with_segment_max_bytes(4096)
            .with_platform_profile_path(profile_path.clone()),
    ) {
        Ok(_) => panic!("PROPERTY: impossible path/mmap profile must fail open"),
        Err(error) => error,
    };
    assert!(
        matches!(
            err,
            StoreError::PlatformProfileInvalid { ref path, .. } if path == &profile_path
        ),
        "expected PlatformProfileInvalid for impossible path/mmap profile, got {err:?}"
    );
}

#[test]
fn mmap_unavailable_profile_fails_reverify_before_open() {
    let dir = TempDir::new().expect("temp dir");
    let profile_path = std::path::PathBuf::from("tests/fixtures/platform/mmap_unavailable.profile");

    let err = match Store::open(
        StoreConfig::new(dir.path())
            .with_segment_max_bytes(4096)
            .with_platform_profile_path(profile_path.clone()),
    ) {
        Ok(_) => panic!("PROPERTY: mmap-unavailable platform profile must fail current open"),
        Err(error) => error,
    };
    assert!(
        matches!(
            err,
            StoreError::PlatformProfileMismatch { ref path, .. } if path == &profile_path
        ),
        "expected PlatformProfileMismatch for unavailable mmap posture, got {err:?}"
    );
    assert!(
        !dir.path().join("000000.fbat").exists(),
        "mmap profile mismatch must fail before writer spawn or lifecycle append"
    );
}

#[test]
fn without_platform_profile_path_clears_reverify_requirement() {
    let dir = TempDir::new().expect("temp dir");
    let missing_profile = dir.path().join("missing-platform-profile.json");

    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_segment_max_bytes(4096)
            .with_platform_profile_path(missing_profile)
            .without_platform_profile_path(),
    )
    .expect("cleared platform profile path should not run reverify");
    store.close().expect("close");
}

#[test]
fn platform_profile_match_allows_open_and_mismatch_fails_before_lifecycle() {
    let dir = TempDir::new().expect("temp dir");
    #[cfg(unix)]
    let valid_profile = std::path::PathBuf::from("tests/fixtures/platform/linux_basic.profile");
    #[cfg(not(unix))]
    let valid_profile =
        std::path::PathBuf::from("tests/fixtures/platform/non_unix_best_effort_lock.profile");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_segment_max_bytes(4096)
            .with_platform_profile_path(valid_profile),
    )
    .expect("valid platform profile should admit open");
    store.close().expect("close");

    let mismatch_dir = TempDir::new().expect("temp dir");
    #[cfg(unix)]
    let mismatch_profile =
        std::path::PathBuf::from("tests/fixtures/platform/profile_mismatch.profile");
    #[cfg(not(unix))]
    let mismatch_profile = std::path::PathBuf::from("tests/fixtures/platform/linux_basic.profile");
    let observed_reports = Arc::new(Mutex::new(Vec::<OpenIndexReport>::new()));
    let observer: OpenReportObserver = {
        let observed_reports = Arc::clone(&observed_reports);
        Arc::new(move |report: &OpenIndexReport| {
            observed_reports
                .lock()
                .expect("open report observer lock")
                .push(report.clone());
        })
    };
    let err = match Store::open(
        StoreConfig::new(mismatch_dir.path())
            .with_segment_max_bytes(4096)
            .with_open_report_observer(Some(observer))
            .with_platform_profile_path(mismatch_profile.clone()),
    ) {
        Ok(_) => panic!("PROPERTY: mismatched platform profile must fail open"),
        Err(error) => error,
    };
    assert!(
        matches!(
            err,
            StoreError::PlatformProfileMismatch { ref path, .. } if path == &mismatch_profile
        ),
        "expected PlatformProfileMismatch, got {err:?}"
    );
    assert!(
        !mismatch_dir.path().join("000000.fbat").exists(),
        "profile mismatch must fail before writer spawn or lifecycle append"
    );
    assert!(
        observed_reports
            .lock()
            .expect("observed reports lock")
            .is_empty(),
        "profile mismatch must fail before successful-open report observability"
    );
}
