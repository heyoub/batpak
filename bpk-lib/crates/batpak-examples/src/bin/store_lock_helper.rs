use batpak::store::{Store, StoreConfig};
use std::error::Error;
use std::io::{Error as IoError, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn wait_for_path(path: &Path, label: &str) -> Result<(), IoError> {
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
    ))
}

fn env_path(name: &str) -> Result<PathBuf, IoError> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| IoError::new(ErrorKind::InvalidInput, format!("{name} is required")))
}

fn main() -> Result<(), Box<dyn Error>> {
    let data_dir = env_path("BATPAK_LOCK_HELPER_DATA_DIR")?;
    let ready = env_path("BATPAK_LOCK_HELPER_READY")?;
    let release = env_path("BATPAK_LOCK_HELPER_RELEASE")?;

    let store = Store::open(StoreConfig::new(&data_dir))?;
    std::fs::write(&ready, b"ready")?;
    wait_for_path(&release, "helper release file")?;
    drop(store);
    Ok(())
}
