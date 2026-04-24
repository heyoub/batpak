// justifies: INV-TEST-PANIC-AS-ASSERTION; this lock-behavior harness uses panic! as assertion style for precise lock-mode drift.
#![allow(clippy::panic)]
//! Store directory locking surface.
//! Harness pattern: State-Machine Harness (open/hold/reject/release lane).
//!
//! PROVES: mutable and read-only opens both hold an exclusive lifetime lock in
//! the first hardening wave, and open attempts fail before cold-start work
//! when the requested mode cannot be acquired.
//! CATCHES: mutable/reopen races that allow two writers, read-only opens that
//! ignore a live mutable owner, or lock guards dropped before the Store handle.
//! SEEDED: not random; deterministic tempdir-based lock choreography.

use batpak::event::kind::EventKindError;
use batpak::store::{ReadOnly, Store, StoreConfig, StoreError, StoreLockMode};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn match_locked(err: StoreError, path: &std::path::Path, mode: StoreLockMode) {
    let expected_path = std::fs::canonicalize(path).expect("canonical tempdir path");
    let StoreError::StoreLocked {
        path: actual_path,
        mode: actual_mode,
    } = err
    else {
        panic!("expected StoreLocked, got {err:?}");
    };
    assert_eq!(actual_path, expected_path);
    assert_eq!(actual_mode, mode);
}

fn wait_for_path(path: &Path, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("{label} did not appear at {}", path.display());
}

fn wait_for_mutable_open_after_release(config: &StoreConfig, path: &Path, label: &str) -> Store {
    let deadline = Instant::now() + Duration::from_secs(2);
    let expected_path = std::fs::canonicalize(path).expect("canonical tempdir path");
    let mut last_err = None;
    while Instant::now() < deadline {
        match Store::open(config.clone()) {
            Ok(store) => return store,
            Err(StoreError::StoreLocked {
                path: actual_path,
                mode,
            }) => {
                assert_eq!(actual_path, expected_path);
                assert_eq!(mode, StoreLockMode::Mutable);
                last_err = Some(StoreError::StoreLocked {
                    path: actual_path,
                    mode,
                });
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(err) => panic!("{label}: unexpected error while waiting for lock release: {err:?}"),
        }
    }
    panic!(
        "{label}: lock did not clear before deadline: {:?}",
        last_err.expect("lock retry loop should record the last StoreLocked error")
    );
}

fn wait_for_read_only_open_after_release(
    config: &StoreConfig,
    path: &Path,
    label: &str,
) -> Store<ReadOnly> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let expected_path = std::fs::canonicalize(path).expect("canonical tempdir path");
    let mut last_err = None;
    while Instant::now() < deadline {
        match Store::<ReadOnly>::open_read_only(config.clone()) {
            Ok(store) => return store,
            Err(StoreError::StoreLocked {
                path: actual_path,
                mode,
            }) => {
                assert_eq!(actual_path, expected_path);
                assert_eq!(mode, StoreLockMode::ReadOnly);
                last_err = Some(StoreError::StoreLocked {
                    path: actual_path,
                    mode,
                });
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(err) => panic!("{label}: unexpected error while waiting for lock release: {err:?}"),
        }
    }
    panic!(
        "{label}: lock did not clear before deadline: {:?}",
        last_err.expect("lock retry loop should record the last StoreLocked error")
    );
}

fn helper_command(data_dir: &Path, ready: &Path, release: &Path) -> Command {
    let mut cmd = Command::new(std::env::current_exe().expect("current test binary"));
    cmd.arg("--exact")
        .arg("subprocess_helper_holds_mutable_lock")
        .arg("--nocapture")
        .env("BATPAK_LOCK_HELPER_DATA_DIR", data_dir)
        .env("BATPAK_LOCK_HELPER_READY", ready)
        .env("BATPAK_LOCK_HELPER_RELEASE", release);
    cmd
}

#[test]
fn mutable_open_holds_exclusive_lock_and_blocks_read_only_until_drop() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path());
    let store = Store::open(config.clone()).expect("open mutable store");

    let err = match Store::open(config.clone()) {
        Ok(_) => panic!("mutable open must not succeed while another mutable store holds the lock"),
        Err(err) => err,
    };
    match_locked(err, dir.path(), StoreLockMode::Mutable);

    let err = match Store::<ReadOnly>::open_read_only(config.clone()) {
        Ok(_) => panic!("read-only open must not succeed while mutable store holds exclusive lock"),
        Err(err) => err,
    };
    match_locked(err, dir.path(), StoreLockMode::ReadOnly);

    drop(store);

    let reopened =
        wait_for_read_only_open_after_release(&config, dir.path(), "read-only open after drop");
    let _ = reopened.query(&batpak::coordinate::Region::all());
}

#[test]
fn read_only_open_is_also_exclusive_in_first_hardening_wave() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path());

    let ro = Store::<ReadOnly>::open_read_only(config.clone()).expect("open read-only store");

    let err = match Store::<ReadOnly>::open_read_only(config.clone()) {
        Ok(_) => panic!("second read-only open must not succeed while first read-only store holds the exclusive lock"),
        Err(err) => err,
    };
    match_locked(err, dir.path(), StoreLockMode::ReadOnly);

    let err = match Store::open(config.clone()) {
        Ok(_) => {
            panic!("mutable open must not succeed while read-only store holds the exclusive lock")
        }
        Err(err) => err,
    };
    match_locked(err, dir.path(), StoreLockMode::Mutable);

    drop(ro);

    let store = wait_for_mutable_open_after_release(
        &config,
        dir.path(),
        "mutable open after read-only release",
    );
    let _ = store.diagnostics();
}

#[test]
fn subprocess_mutable_owner_blocks_other_processes() {
    let dir = TempDir::new().expect("temp dir");
    let ready = dir.path().join("ready");
    let release = dir.path().join("release");
    let config = StoreConfig::new(dir.path());

    let mut child = helper_command(dir.path(), &ready, &release)
        .spawn()
        .expect("spawn lock helper");

    wait_for_path(&ready, "helper ready file");

    let err = match Store::open(config.clone()) {
        Ok(_) => panic!("second mutable open must fail while helper owns the lock"),
        Err(err) => err,
    };
    match_locked(err, dir.path(), StoreLockMode::Mutable);

    let err = match Store::<ReadOnly>::open_read_only(config) {
        Ok(_) => panic!("read-only open must fail while helper owns the lock"),
        Err(err) => err,
    };
    match_locked(err, dir.path(), StoreLockMode::ReadOnly);

    std::fs::write(&release, b"release").expect("release helper");
    let status = child.wait().expect("wait on helper");
    assert!(
        status.success(),
        "helper process must exit successfully: {status}"
    );
}

#[test]
fn event_kind_error_pub_surface_has_a_real_test_witness() {
    let err = EventKindError::ReservedSystemCategory;
    let display = err.to_string();
    assert!(
        display.contains("reserved"),
        "public EventKindError witness should remain a real path-position test use"
    );
}

#[test]
fn subprocess_helper_holds_mutable_lock() {
    let data_dir = match std::env::var_os("BATPAK_LOCK_HELPER_DATA_DIR") {
        Some(path) => PathBuf::from(path),
        None => return,
    };
    let ready = PathBuf::from(std::env::var_os("BATPAK_LOCK_HELPER_READY").expect("ready path"));
    let release =
        PathBuf::from(std::env::var_os("BATPAK_LOCK_HELPER_RELEASE").expect("release path"));

    let store = Store::open(StoreConfig::new(&data_dir)).expect("helper opens mutable store");
    std::fs::write(&ready, b"ready").expect("write ready file");
    wait_for_path(&release, "helper release file");
    drop(store);
}
