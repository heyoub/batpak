use std::time::Duration;

const TEST_WAIT_TIMEOUT: Duration = Duration::from_secs(2);

pub fn blocking<T, F>(name: &'static str, f: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .unwrap_or_else(|err| {
            panic!("PROPERTY: failed to spawn bounded wait thread {name}: {err}")
        });
    rx.recv_timeout(TEST_WAIT_TIMEOUT)
        .unwrap_or_else(|err| panic!("PROPERTY: timed out waiting for {name}: {err}"))
}
