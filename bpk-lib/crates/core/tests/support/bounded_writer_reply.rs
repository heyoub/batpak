use batpak::store::StoreError;
use std::io;
use std::time::Duration;

const TEST_WAIT_TIMEOUT: Duration = Duration::from_secs(2);

pub fn writer_reply<T>(
    rx: &flume::Receiver<Result<T, StoreError>>,
    label: &str,
) -> Result<T, StoreError> {
    rx.recv_timeout(TEST_WAIT_TIMEOUT).map_err(|err| {
        StoreError::Io(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("PROPERTY: timed out waiting for {label}: {err}"),
        ))
    })?
}
