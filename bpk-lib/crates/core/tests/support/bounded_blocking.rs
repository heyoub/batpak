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
        .map_err(|err| format!("PROPERTY: failed to spawn bounded wait thread {name}: {err}"))
        .expect("bounded wait thread must spawn");
    rx.recv_timeout(TEST_WAIT_TIMEOUT)
        .map_err(|err| format!("PROPERTY: timed out waiting for {name}: {err}"))
        .expect("bounded wait must complete within timeout")
}
