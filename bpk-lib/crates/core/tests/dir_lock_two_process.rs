//! Cross-process store directory lock witness (audit C3).
//!
//! PROVES: STORE-DIR-LOCK-TWO-PROCESS — a second process cannot acquire a
//! mutable store open while another live process holds `.batpak.lock`.
//! CATCHES: advisory flock that fails to exclude a concurrent writer open
//! across process boundaries.
//! SEEDED: deterministic tempdir + self-reexec child-process choreography.

use batpak::store::{Store, StoreConfig, StoreError, StoreLockMode};
use std::error::Error;
use std::io::{Error as IoError, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tempfile::TempDir;

type TestResult = Result<(), Box<dyn Error>>;

const CHILD_ENV: &str = "BATPAK_DIR_LOCK_CHILD";
const DATA_DIR_ENV: &str = "BATPAK_LOCK_HELPER_DATA_DIR";
const READY_ENV: &str = "BATPAK_LOCK_HELPER_READY";
const RELEASE_ENV: &str = "BATPAK_LOCK_HELPER_RELEASE";

fn wait_for_path(path: &Path, label: &str) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if path.exists() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err(IoError::new(
        ErrorKind::TimedOut,
        format!("{label} did not appear at {}", path.display()),
    )
    .into())
}

fn env_path(name: &str) -> Result<PathBuf, IoError> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| IoError::new(ErrorKind::InvalidInput, format!("{name} is required")))
}

fn helper_command(data_dir: &Path, ready: &Path, release: &Path) -> Result<Command, IoError> {
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.env(CHILD_ENV, "1")
        .env(DATA_DIR_ENV, data_dir)
        .env(READY_ENV, ready)
        .env(RELEASE_ENV, release)
        .arg("lock_holder_child_process")
        .arg("--exact")
        .arg("--nocapture");
    Ok(cmd)
}

fn assert_store_locked(err: StoreError, path: &Path, mode: StoreLockMode) -> TestResult {
    let expected_path = std::fs::canonicalize(path)?;
    let StoreError::StoreLocked {
        path: actual_path,
        mode: actual_mode,
    } = err
    else {
        return Err(IoError::other(format!("PROPERTY: expected StoreLocked, got {err:?}")).into());
    };
    if actual_path != expected_path {
        return Err(IoError::other(format!(
            "PROPERTY: StoreLocked path mismatch: expected {}, got {}",
            expected_path.display(),
            actual_path.display()
        ))
        .into());
    }
    if actual_mode != mode {
        return Err(IoError::other(format!(
            "PROPERTY: StoreLocked mode mismatch: expected {mode:?}, got {actual_mode:?}"
        ))
        .into());
    }
    Ok(())
}

/// Child entry invoked via self-reexec (`--exact lock_holder_child_process`).
#[test]
fn lock_holder_child_process() -> TestResult {
    if std::env::var(CHILD_ENV).is_err() {
        return Ok(());
    }

    let data_dir = env_path(DATA_DIR_ENV)?;
    let ready = env_path(READY_ENV)?;
    let release = env_path(RELEASE_ENV)?;

    let store = Store::open(StoreConfig::new(&data_dir))?;
    std::fs::write(&ready, b"ready")?;
    wait_for_path(&release, "helper release file")?;
    drop(store);
    Ok(())
}

#[test]
fn second_writer_open_refuses_while_other_process_holds_lock() -> TestResult {
    if std::env::var(CHILD_ENV).is_ok() {
        return Ok(());
    }

    let dir = TempDir::new()?;
    let ready = dir.path().join("ready");
    let release = dir.path().join("release");
    let config = StoreConfig::new(dir.path());

    let mut child = helper_command(dir.path(), &ready, &release)?.spawn()?;

    wait_for_path(&ready, "helper ready file")?;

    let err = match Store::open(config) {
        Ok(_) => {
            return Err(IoError::other(
                "PROPERTY: second mutable open must fail while helper owns the lock",
            )
            .into());
        }
        Err(e) => e,
    };
    assert_store_locked(err, dir.path(), StoreLockMode::Mutable)?;

    std::fs::write(&release, b"release")?;
    let status = child.wait()?;
    if !status.success() {
        return Err(IoError::other(format!(
            "PROPERTY: helper process must exit successfully: {status}"
        ))
        .into());
    }
    Ok(())
}
