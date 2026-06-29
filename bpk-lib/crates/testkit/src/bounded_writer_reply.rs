use batpak::store::StoreError;
use std::io;
use std::time::Duration;

const TEST_WAIT_TIMEOUT: Duration = Duration::from_secs(2);

/// Block on a writer reply channel with a bounded timeout, returning the
/// writer's `Result`.
///
/// # Errors
///
/// Returns the [`StoreError`] carried by the reply, or a timed-out
/// [`StoreError::Io`] if no reply arrives within the bounded test timeout.
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
