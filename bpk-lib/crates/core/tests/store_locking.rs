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
use std::path::Path;
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

fn helper_command(data_dir: &Path, ready: &Path, release: &Path) -> Command {
    let current = std::env::current_exe().expect("current test binary");
    let helper_name = format!("store_lock_helper{}", std::env::consts::EXE_SUFFIX);
    // The test binary lives in `target/<profile>/deps/`; workspace `cargo`/`nextest`
    // builds the `store_lock_helper` bin one level up in the profile root
    // (`target/<profile>/`). Probe both so a pre-built helper is used directly —
    // the nested `cargo run` fallback below only fires when the bin was never
    // built (e.g. `cargo test -p batpak` alone) and can blow the ready deadline
    // while it compiles.
    let deps_dir = current
        .parent()
        .expect("test binary lives under target profile");
    let helper = [Some(deps_dir), deps_dir.parent()]
        .into_iter()
        .flatten()
        .map(|dir| dir.join(&helper_name))
        .find(|candidate| candidate.exists());
    let mut cmd = if let Some(helper) = helper {
        Command::new(helper)
    } else {
        let mut cargo = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
        cargo
            .arg("run")
            .arg("--quiet")
            .arg("-p")
            .arg("batpak-examples")
            .arg("--bin")
            .arg("store_lock_helper")
            .arg("--");
        cargo
    };
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
    let dir = TempDir::new().expect("temp dir");
    let ready = dir.path().join("ready");
    let release = dir.path().join("release");
    let config = StoreConfig::new(dir.path());

    let mut child = helper_command(dir.path(), &ready, &release)
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
