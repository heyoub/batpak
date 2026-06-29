//! Store directory locking surface.
//! PROVES: INV-JOURNAL-SINGLE-LIVE-OWNER.
//! Harness pattern: State-Machine Harness (open/hold/reject/release lane).
//!
//! PROVES: mutable and read-only opens both hold an exclusive lifetime lock
//! under the exclusive-only ownership contract, and open attempts fail before
//! cold-start work when the requested mode cannot be acquired.
//! CATCHES: mutable/reopen races that allow two writers, read-only opens that
//! ignore a live mutable owner, or lock guards dropped before the Store handle.
//! SEEDED: not random; deterministic tempdir-based lock choreography.

use batpak::event::kind::EventKindError;
use batpak::store::{ReadOnly, Store, StoreConfig, StoreError, StoreLockMode};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn match_locked(err: StoreError, path: &std::path::Path, mode: StoreLockMode) {
    let expected_path = std::fs::canonicalize(path).expect("canonical tempdir path");
    assert!(
        matches!(&err, StoreError::StoreLocked { .. }),
        "expected StoreLocked, got {err:?}"
    );
    let StoreError::StoreLocked {
        path: actual_path,
        mode: actual_mode,
    } = err
    else {
        unreachable!("matched StoreLocked above")
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
    unreachable!("{label} did not appear at {}", path.display())
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
            Err(err) => {
                unreachable!("{label}: unexpected error while waiting for lock release: {err:?}")
            }
        }
    }
    unreachable!(
        "{label}: lock did not clear before deadline: {:?}",
        last_err.expect("lock retry loop should record the last StoreLocked error")
    )
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
            Err(err) => {
                unreachable!("{label}: unexpected error while waiting for lock release: {err:?}")
            }
        }
    }
    unreachable!(
        "{label}: lock did not clear before deadline: {:?}",
        last_err.expect("lock retry loop should record the last StoreLocked error")
    )
}

/// Locate the pre-built `store_lock_helper` binary, if it exists.
///
/// The test binary lives in `target/<profile>/deps/`; a workspace build places
/// the helper bin one level up in the profile root (`target/<profile>/`). Probe
/// both. Returns `None` when the helper was never built — e.g. `cargo test -p
/// batpak` alone, or the sealed CI devcontainer (`BATPAK_DEVCONTAINER_SKIP_BUILD`),
/// where `batpak-examples` is not compiled. The cross-process witness then SKIPs
/// rather than shelling out to a nested `cargo run` that cannot build under
/// `SKIP_BUILD` and blows the ready deadline. The two in-process lock tests above
/// still witness exclusive ownership unconditionally.
fn prebuilt_helper() -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    let helper_name = format!("store_lock_helper{}", std::env::consts::EXE_SUFFIX);
    let deps_dir = current.parent()?;
    let mut candidates = vec![deps_dir.join(&helper_name)];
    if let Some(profile_root) = deps_dir.parent() {
        candidates.push(profile_root.join(&helper_name));
    }
    candidates.into_iter().find(|candidate| candidate.exists())
}

fn helper_command(helper: &Path, data_dir: &Path, ready: &Path, release: &Path) -> Command {
    let mut cmd = Command::new(helper);
    cmd.env("BATPAK_LOCK_HELPER_DATA_DIR", data_dir)
        .env("BATPAK_LOCK_HELPER_READY", ready)
        .env("BATPAK_LOCK_HELPER_RELEASE", release);
    cmd
}

#[test]
fn mutable_open_holds_exclusive_lock_and_blocks_read_only_until_drop() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path());
    let store = Store::open(config.clone()).expect("open mutable store");

    let err = Store::open(config.clone())
        .map(|_| ())
        .expect_err("mutable open must not succeed while another mutable store holds the lock");
    match_locked(err, dir.path(), StoreLockMode::Mutable);

    let err = Store::<ReadOnly>::open_read_only(config.clone())
        .map(|_| ())
        .expect_err("read-only open must not succeed while mutable store holds exclusive lock");
    match_locked(err, dir.path(), StoreLockMode::ReadOnly);

    drop(store);

    let reopened =
        wait_for_read_only_open_after_release(&config, dir.path(), "read-only open after drop");
    let _ = reopened.query(&batpak::coordinate::Region::all());
}

#[test]
fn read_only_open_is_also_exclusive_under_ownership_contract() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path());

    let ro = Store::<ReadOnly>::open_read_only(config.clone()).expect("open read-only store");

    let err = Store::<ReadOnly>::open_read_only(config.clone())
        .map(|_| ())
        .expect_err(
            "second read-only open must not succeed while first read-only store holds the exclusive lock",
        );
    match_locked(err, dir.path(), StoreLockMode::ReadOnly);

    let err = Store::open(config.clone())
        .map(|_| ())
        .expect_err("mutable open must not succeed while read-only store holds the exclusive lock");
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
    let Some(helper) = prebuilt_helper() else {
        let _ = writeln!(
            std::io::stderr(),
            "SKIP subprocess_mutable_owner_blocks_other_processes: store_lock_helper bin not \
             built in this run; the in-process mutable/read-only lock tests still witness \
             exclusive ownership. Run a workspace build to exercise the cross-process witness."
        );
        return;
    };
    let dir = TempDir::new().expect("temp dir");
    let ready = dir.path().join("ready");
    let release = dir.path().join("release");
    let config = StoreConfig::new(dir.path());

    let mut child = helper_command(&helper, dir.path(), &ready, &release)
        .spawn()
        .expect("spawn lock helper");

    wait_for_path(&ready, "helper ready file");

    let err = Store::open(config.clone())
        .map(|_| ())
        .expect_err("second mutable open must fail while helper owns the lock");
    match_locked(err, dir.path(), StoreLockMode::Mutable);

    let err = Store::<ReadOnly>::open_read_only(config)
        .map(|_| ())
        .expect_err("read-only open must fail while helper owns the lock");
    match_locked(err, dir.path(), StoreLockMode::ReadOnly);

    std::fs::write(&release, b"release").expect("release helper");
    // Intentional: child process wait follows the release signal; helper exit
    // is the test assertion.
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
