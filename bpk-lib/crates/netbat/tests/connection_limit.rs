//! PROVES: NETBAT-TCP-CONNECTION-LIMIT — the `ConnectionLimit` admission policy
//! on the request listener.
//! CATCHES: a `Concurrent` cap that leaks a slot (on disconnect or on a worker
//! panic), an empty-pool that fails open instead of blocking, and a `Lifetime`
//! cap that no longer stops after N total accepts.
//! SEEDED: localhost listeners with a ping handler, a gate handler that blocks a
//! worker (pinning the single permit), and a panic handler.

use netbat as nb;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::num::NonZeroUsize;
use std::thread;
use std::time::Duration;
use syncbat::{Core, EffectClass, Handler, HandlerResult, OperationDescriptor};

const PING: OperationDescriptor = OperationDescriptor::new(
    "ping",
    EffectClass::Inspect,
    "schema.ping.input.v1",
    "schema.ping.output.v1",
    "receipt.ping.v1",
);
const GATE: OperationDescriptor = OperationDescriptor::new(
    "gate",
    EffectClass::Inspect,
    "schema.gate.input.v1",
    "schema.gate.output.v1",
    "receipt.gate.v1",
);
const BOOM: OperationDescriptor = OperationDescriptor::new(
    "boom",
    EffectClass::Inspect,
    "schema.boom.input.v1",
    "schema.boom.output.v1",
    "receipt.boom.v1",
);

struct PingHandler;

impl Handler for PingHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut syncbat::Ctx<'_>) -> HandlerResult {
        Ok(input.to_vec())
    }
}

/// A handler that signals on `entered` and then blocks on `release`, pinning the
/// connection's worker (and thus its concurrency permit) until the test sends.
struct GateHandler {
    entered: flume::Sender<()>,
    release: flume::Receiver<()>,
}

impl Handler for GateHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut syncbat::Ctx<'_>) -> HandlerResult {
        let _ = self.entered.send(());
        // Block until the test releases this worker (or the sender is dropped at
        // teardown, which returns an error we ignore so the handler unwinds).
        let _ = self.release.recv();
        Ok(input.to_vec())
    }
}

/// Panics via a genuine out-of-bounds index (not `panic!`/`unwrap`) so the test
/// stays inside the crate's zero-panic-macro posture; the containment path under
/// test is agnostic to how the panic was raised.
struct PanicHandler;

impl Handler for PanicHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut syncbat::Ctx<'_>) -> HandlerResult {
        let probe = input.to_vec();
        let escape = probe[probe.len() + 1];
        Ok(vec![escape])
    }
}

fn build_core(entered: flume::Sender<()>, release: flume::Receiver<()>) -> Core {
    let mut builder = Core::builder();
    builder.register(PING, PingHandler).expect("register ping");
    builder
        .register(GATE, GateHandler { entered, release })
        .expect("register gate");
    builder.register(BOOM, PanicHandler).expect("register boom");
    builder.without_receipts();
    builder.build().expect("core builds")
}

fn concurrent(value: usize) -> nb::ConnectionLimit {
    nb::ConnectionLimit::Concurrent(NonZeroUsize::new(value).expect("nonzero"))
}

fn lifetime(value: usize) -> nb::ConnectionLimit {
    nb::ConnectionLimit::Lifetime(NonZeroUsize::new(value).expect("nonzero"))
}

fn localhost_listener() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").expect("bind localhost listener")
}

fn connect(addr: std::net::SocketAddr, read_timeout: Duration) -> TcpStream {
    let stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(read_timeout))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .expect("set write timeout");
    stream
}

fn read_line_with_timeout(stream: &TcpStream, timeout: Duration) -> io::Result<String> {
    stream.set_read_timeout(Some(timeout))?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    Ok(line)
}

fn spawn_capped_server(
    listener: TcpListener,
    config: nb::TcpServerConfig,
    shutdown: nb::ShutdownHandle,
    entered: flume::Sender<()>,
    release: flume::Receiver<()>,
) -> thread::JoinHandle<nb::TcpServeStats> {
    thread::Builder::new()
        .name("netbat-cap-server".to_owned())
        .spawn(move || {
            let factory = move || build_core(entered.clone(), release.clone());
            nb::serve_tcp_listener(listener, factory, &config, &shutdown).expect("serve listener")
        })
        .expect("spawn server")
}

#[test]
fn concurrent_cap_reuses_a_slot_released_on_disconnect() {
    // GREEN (post-change): `Concurrent(1)` is an IN-FLIGHT cap, so the single
    // slot freed on each disconnect is reused — five SERIAL connections all
    // succeed. RED (pre-change): `max_connections` was a LIFETIME budget, so the
    // equivalent of 1 would have stopped accepting after the very first
    // connection and connections 2..5 would get nothing. That exact pre-change
    // budget behavior is pinned, now opt-in, by
    // `lifetime_cap_stops_after_n_total_connections` below — the RED baseline.
    let listener = localhost_listener();
    let addr = listener.local_addr().expect("addr");
    let shutdown = nb::ShutdownHandle::new();
    let server_shutdown = shutdown.clone();
    let (entered_tx, _entered_rx) = flume::unbounded::<()>();
    let (_release_tx, release_rx) = flume::unbounded::<()>();

    let config = nb::TcpServerConfig::default()
        .with_connection_limit(concurrent(1))
        .with_idle_sleep(Duration::from_millis(1));
    let handle = spawn_capped_server(listener, config, server_shutdown, entered_tx, release_rx);

    for round in 0..5 {
        let stream = connect(addr, Duration::from_secs(2));
        (&stream)
            .write_all(b"NETBAT/1 CALL ping 6869\n")
            .expect("write request");
        let response =
            read_line_with_timeout(&stream, Duration::from_secs(2)).expect("read response");
        assert_eq!(
            response, "OK 6869\n",
            "serial connection {round} must be served"
        );
        drop(stream);
    }

    shutdown.shutdown();
    let stats = handle.join().expect("server joins");
    assert_eq!(
        stats.served_requests, 5,
        "every serial connection must reuse the released slot; stats={stats:?}"
    );
    assert_eq!(stats.accepted_connections, 5);
}

#[test]
fn concurrent_cap_blocks_the_next_connection_until_a_slot_frees() {
    // Empty-pool semantics: BLOCK for a slot (back-pressure), not reject. With
    // `Concurrent(1)`, client A trips the gate handler and holds the only permit;
    // client B then connects and sends a ping, but the accept loop blocks in
    // permit acquisition, so B gets NO response while A holds the slot. Releasing
    // A frees the slot and B is served.
    let listener = localhost_listener();
    let addr = listener.local_addr().expect("addr");
    let shutdown = nb::ShutdownHandle::new();
    let server_shutdown = shutdown.clone();
    let (entered_tx, entered_rx) = flume::unbounded::<()>();
    let (release_tx, release_rx) = flume::unbounded::<()>();

    let config = nb::TcpServerConfig::default()
        .with_connection_limit(concurrent(1))
        .with_idle_sleep(Duration::from_millis(1));
    let handle = spawn_capped_server(listener, config, server_shutdown, entered_tx, release_rx);

    // A grabs the single permit and blocks inside the gate handler.
    let client_a = connect(addr, Duration::from_secs(2));
    (&client_a)
        .write_all(b"NETBAT/1 CALL gate 6869\n")
        .expect("write A");
    entered_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("gate handler must enter, holding the permit");

    // B connects and asks for a ping. The pool is empty, so admission blocks and
    // B receives nothing within the window.
    let client_b = connect(addr, Duration::from_millis(300));
    (&client_b)
        .write_all(b"NETBAT/1 CALL ping 6869\n")
        .expect("write B");
    let early = read_line_with_timeout(&client_b, Duration::from_millis(300));
    let b_blocked = early.as_ref().map_or(true, |line| line.is_empty());
    assert!(
        b_blocked,
        "B must be blocked on the permit while A holds it; got {early:?}"
    );

    // Release A → its slot frees → B is admitted and served.
    release_tx.send(()).expect("release A");
    let late = read_line_with_timeout(&client_b, Duration::from_secs(2)).expect("read B");
    assert_eq!(late, "OK 6869\n", "B must be served once the slot frees");

    drop(client_a);
    drop(client_b);
    shutdown.shutdown();
    let stats = handle.join().expect("server joins");
    assert!(stats.served_requests >= 1, "stats={stats:?}");
}

#[test]
fn concurrent_cap_releases_the_slot_when_a_worker_panics() {
    // `Concurrent(1)`: client A trips the panicking handler. The panic is caught
    // at the worker boundary (counted in worker_panics) and the permit — held
    // OUTSIDE the catch_unwind — still releases on the unwinding scope exit. So
    // the single slot is freed and client B's ping is served. If the permit
    // leaked on the panic path, `Concurrent(1)` would be permanently exhausted
    // and B would block forever (its read times out empty).
    let listener = localhost_listener();
    let addr = listener.local_addr().expect("addr");
    let shutdown = nb::ShutdownHandle::new();
    let server_shutdown = shutdown.clone();
    let (entered_tx, _entered_rx) = flume::unbounded::<()>();
    let (_release_tx, release_rx) = flume::unbounded::<()>();

    let config = nb::TcpServerConfig::default()
        .with_connection_limit(concurrent(1))
        .with_idle_sleep(Duration::from_millis(1));
    let handle = spawn_capped_server(listener, config, server_shutdown, entered_tx, release_rx);

    let client_a = connect(addr, Duration::from_secs(2));
    (&client_a)
        .write_all(b"NETBAT/1 CALL boom 00\n")
        .expect("write A");
    // Let A's worker run and unwind so the permit-release-on-panic happens before
    // B needs the slot.
    thread::sleep(Duration::from_millis(100));
    drop(client_a);

    let client_b = connect(addr, Duration::from_secs(2));
    (&client_b)
        .write_all(b"NETBAT/1 CALL ping 6869\n")
        .expect("write B");
    let response =
        read_line_with_timeout(&client_b, Duration::from_secs(2)).expect("read B response");
    assert_eq!(
        response, "OK 6869\n",
        "B must be served — the panicking worker must release its slot"
    );

    shutdown.shutdown();
    let stats = handle.join().expect("server joins");
    assert!(
        stats.worker_panics >= 1,
        "the panic must be caught and counted; stats={stats:?}"
    );
    assert!(stats.served_requests >= 1, "stats={stats:?}");
}

#[test]
fn lifetime_cap_stops_after_n_total_connections() {
    // `Lifetime(2)` is the pre-0.9 `max_connections` budget, now an explicit
    // opt-in: accept exactly two connections EVER, then stop and return — no
    // shutdown needed. This is the RED baseline that
    // `concurrent_cap_reuses_a_slot_released_on_disconnect` contrasts: under a
    // lifetime budget a freed slot is NEVER reused.
    let listener = localhost_listener();
    let addr = listener.local_addr().expect("addr");
    let shutdown = nb::ShutdownHandle::new();
    let server_shutdown = shutdown.clone();
    let (entered_tx, _entered_rx) = flume::unbounded::<()>();
    let (_release_tx, release_rx) = flume::unbounded::<()>();

    let config = nb::TcpServerConfig::default()
        .with_connection_limit(lifetime(2))
        .with_idle_sleep(Duration::from_millis(1));
    let handle = spawn_capped_server(listener, config, server_shutdown, entered_tx, release_rx);

    for round in 0..2 {
        let stream = connect(addr, Duration::from_secs(2));
        (&stream)
            .write_all(b"NETBAT/1 CALL ping 6869\n")
            .expect("write request");
        let response =
            read_line_with_timeout(&stream, Duration::from_secs(2)).expect("read response");
        assert_eq!(
            response, "OK 6869\n",
            "connection {round} within budget is served"
        );
        drop(stream);
        thread::sleep(Duration::from_millis(50));
    }

    // The listener accepted its 2-connection budget and exited on its own.
    let stats = handle
        .join()
        .expect("server joins on budget without shutdown");
    assert_eq!(stats.accepted_connections, 2, "stats={stats:?}");
    assert_eq!(stats.served_requests, 2, "stats={stats:?}");
    assert!(
        !stats.shutdown_requested,
        "budget exit, not shutdown; stats={stats:?}"
    );
    // A third connection cannot be served — the listening socket is gone.
    let third = TcpStream::connect(addr).and_then(|stream| {
        stream.set_read_timeout(Some(Duration::from_millis(300)))?;
        (&stream).write_all(b"NETBAT/1 CALL ping 6869\n")?;
        read_line_with_timeout(&stream, Duration::from_millis(300))
    });
    let third_refused = third.as_ref().map_or(true, |line| line.is_empty());
    assert!(
        third_refused,
        "the lifetime budget must refuse a third connection; got {third:?}"
    );
}
