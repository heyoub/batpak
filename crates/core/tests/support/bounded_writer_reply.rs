use batpak::store::StoreError;
use std::time::Duration;

const TEST_WAIT_TIMEOUT: Duration = Duration::from_secs(2);

pub fn writer_reply<T>(
    rx: &flume::Receiver<Result<T, StoreError>>,
    label: &str,
) -> Result<T, StoreError> {
    rx.recv_timeout(TEST_WAIT_TIMEOUT)
        .unwrap_or_else(|err| panic!("PROPERTY: timed out waiting for {label}: {err}"))
}
