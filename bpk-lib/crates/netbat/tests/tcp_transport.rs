//! PROVES: INV-NETBAT-LINE-PROTOCOL-STABLE, INV-NETBAT-BOUNDARY-THIN
//! CATCHES: TCP listener response-shape drift, limit accounting regressions, and shutdown-loop ownership bugs.
//! SEEDED: localhost listeners with fixed request frames.
#![allow(clippy::panic)]

use netbat as nb;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
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

struct PingHandler;

impl Handler for PingHandler {
    fn handle(&mut self, input: &[u8], _cx: &mut syncbat::Ctx<'_>) -> HandlerResult {
        Ok(input.to_vec())
    }
}

fn core_with_ping() -> Core {
    let mut builder = Core::builder();
    builder.register(PING, PingHandler).expect("register");
    builder.build().expect("core builds")
}

fn localhost_listener() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").expect("bind localhost listener")
}

fn connect_client(addr: std::net::SocketAddr) -> TcpStream {
    let stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set client read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .expect("set client write timeout");
    stream
}

fn spawn_server(
    name: &'static str,
    listener: TcpListener,
    config: nb::TcpServerConfig,
    shutdown: nb::ShutdownHandle,
) -> thread::JoinHandle<nb::TcpServeStats> {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let mut core = core_with_ping();
            nb::serve_tcp_listener(listener, &mut core, &config, &shutdown).expect("serve listener")
        })
        .expect("spawn tcp test server")
}

#[test]
fn tcp_listener_serves_one_real_socket_request() {
    let listener = localhost_listener();
    let addr = listener.local_addr().expect("listener addr");
    let shutdown = nb::ShutdownHandle::new();
    let server_shutdown = shutdown.clone();

    let config = nb::TcpServerConfig::default().with_max_connections(1);
    let handle = spawn_server("netbat-tcp-one", listener, config, server_shutdown);

    let mut stream = connect_client(addr);
    stream
        .write_all(b"NETBAT/1 CALL ping 6869\n")
        .expect("write request");
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .expect("read response");

    let stats = handle.join().expect("server thread joins");
    assert_eq!(response, "OK 6869\n");
    assert_eq!(stats.accepted_connections, 1);
    assert_eq!(stats.served_requests, 1);
    assert_eq!(stats.failed_requests, 0);
    assert_eq!(stats.malformed_requests, 0);
    assert_eq!(stats.limit_failures, 0);
    assert_eq!(stats.runtime_failures, 0);
    assert!(!stats.shutdown_requested);
}

#[test]
fn tcp_listener_enforces_request_limit_per_connection() {
    let listener = localhost_listener();
    let addr = listener.local_addr().expect("listener addr");
    let shutdown = nb::ShutdownHandle::new();
    let server_shutdown = shutdown.clone();

    let config = nb::TcpServerConfig::default()
        .with_max_connections(1)
        .with_max_requests_per_connection(1);
    let handle = spawn_server("netbat-tcp-limit", listener, config, server_shutdown);

    let mut stream = connect_client(addr);
    stream
        .write_all(b"CALL ping 6f6e65\nCALL ping 74776f\n")
        .expect("write requests");
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    let mut second = String::new();
    reader.read_line(&mut first).expect("read first response");
    let closed = match reader.read_line(&mut second) {
        Ok(0) => true,
        Ok(_) => false,
        Err(error) if error.kind() == io::ErrorKind::ConnectionReset => true,
        Err(error) => panic!("unexpected second-read error: {error}"),
    };

    let stats = handle.join().expect("server thread joins");
    assert_eq!(first, "OK 6f6e65\n");
    assert!(closed);
    assert!(second.is_empty());
    assert_eq!(stats.served_requests, 1);
}

#[test]
fn tcp_listener_writes_stable_error_response_for_bad_request() {
    let listener = localhost_listener();
    let addr = listener.local_addr().expect("listener addr");
    let shutdown = nb::ShutdownHandle::new();
    let server_shutdown = shutdown.clone();

    let config = nb::TcpServerConfig::default().with_max_connections(1);
    let handle = spawn_server("netbat-tcp-error", listener, config, server_shutdown);

    let mut stream = connect_client(addr);
    stream.write_all(b"NOPE ping 00\n").expect("write request");
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .expect("read response");

    let stats = handle.join().expect("server thread joins");
    assert!(response.starts_with("ERR malformed_request "));
    assert_eq!(stats.served_requests, 0);
    assert_eq!(stats.failed_requests, 1);
    assert_eq!(stats.malformed_requests, 1);
    assert_eq!(stats.limit_failures, 0);
    assert_eq!(stats.runtime_failures, 0);
}

#[test]
fn tcp_listener_keeps_connection_after_request_error_when_limit_allows() {
    let listener = localhost_listener();
    let addr = listener.local_addr().expect("listener addr");
    let shutdown = nb::ShutdownHandle::new();
    let server_shutdown = shutdown.clone();

    let config = nb::TcpServerConfig::default()
        .with_max_connections(1)
        .with_max_requests_per_connection(2);
    let handle = spawn_server(
        "netbat-tcp-keepalive-error",
        listener,
        config,
        server_shutdown,
    );

    let mut stream = connect_client(addr);
    stream
        .write_all(b"NOPE ping 00\nNETBAT/1 CALL ping 6869\n")
        .expect("write requests");
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    let mut second = String::new();
    reader.read_line(&mut first).expect("read first response");
    reader.read_line(&mut second).expect("read second response");

    let stats = handle.join().expect("server thread joins");
    assert!(first.starts_with("ERR malformed_request "));
    assert_eq!(second, "OK 6869\n");
    assert_eq!(stats.accepted_connections, 1);
    assert_eq!(stats.served_requests, 1);
    assert_eq!(stats.failed_requests, 1);
    assert_eq!(stats.malformed_requests, 1);
    assert_eq!(stats.limit_failures, 0);
    assert_eq!(stats.runtime_failures, 0);
}

#[test]
fn tcp_listener_rejects_unsupported_protocol_version() {
    let listener = localhost_listener();
    let addr = listener.local_addr().expect("listener addr");
    let shutdown = nb::ShutdownHandle::new();
    let server_shutdown = shutdown.clone();

    let config = nb::TcpServerConfig::default().with_max_connections(1);
    let handle = spawn_server("netbat-tcp-version", listener, config, server_shutdown);

    let mut stream = connect_client(addr);
    stream
        .write_all(b"NETBAT/2 CALL ping 6869\n")
        .expect("write request");
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .expect("read response");

    let stats = handle.join().expect("server thread joins");
    assert!(response.starts_with("ERR unsupported_protocol_version "));
    assert_eq!(stats.accepted_connections, 1);
    assert_eq!(stats.served_requests, 0);
    assert_eq!(stats.failed_requests, 1);
    assert_eq!(stats.malformed_requests, 1);
    assert_eq!(stats.limit_failures, 0);
    assert_eq!(stats.runtime_failures, 0);
}

#[test]
fn tcp_listener_accounts_limit_failures() {
    let listener = localhost_listener();
    let addr = listener.local_addr().expect("listener addr");
    let shutdown = nb::ShutdownHandle::new();
    let server_shutdown = shutdown.clone();

    let config = nb::TcpServerConfig::default()
        .with_max_connections(1)
        .with_limits(nb::Limits::default().with_max_line_bytes(8));
    let handle = spawn_server("netbat-tcp-line-limit", listener, config, server_shutdown);

    let mut stream = connect_client(addr);
    stream
        .write_all(b"NETBAT/1 CALL ping 6869\n")
        .expect("write request");
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .expect("read response");

    let stats = handle.join().expect("server thread joins");
    assert!(response.starts_with("ERR line_too_long "));
    assert_eq!(stats.accepted_connections, 1);
    assert_eq!(stats.served_requests, 0);
    assert_eq!(stats.failed_requests, 1);
    assert_eq!(stats.malformed_requests, 0);
    assert_eq!(stats.limit_failures, 1);
    assert_eq!(stats.runtime_failures, 0);
}

#[test]
fn tcp_listener_accounts_runtime_failures() {
    let listener = localhost_listener();
    let addr = listener.local_addr().expect("listener addr");
    let shutdown = nb::ShutdownHandle::new();
    let server_shutdown = shutdown.clone();

    let config = nb::TcpServerConfig::default().with_max_connections(1);
    let handle = spawn_server("netbat-tcp-runtime", listener, config, server_shutdown);

    let mut stream = connect_client(addr);
    stream
        .write_all(b"NETBAT/1 CALL missing 00\n")
        .expect("write request");
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .expect("read response");

    let stats = handle.join().expect("server thread joins");
    assert!(response.starts_with("ERR unknown_operation "));
    assert_eq!(stats.accepted_connections, 1);
    assert_eq!(stats.served_requests, 0);
    assert_eq!(stats.failed_requests, 1);
    assert_eq!(stats.malformed_requests, 0);
    assert_eq!(stats.limit_failures, 0);
    assert_eq!(stats.runtime_failures, 1);
}

#[test]
fn shutdown_handle_stops_idle_listener() {
    let listener = localhost_listener();
    let shutdown = nb::ShutdownHandle::new();
    let server_shutdown = shutdown.clone();

    let config = nb::TcpServerConfig::default().with_idle_sleep(Duration::from_millis(1));
    let handle = spawn_server("netbat-tcp-shutdown", listener, config, server_shutdown);

    thread::sleep(Duration::from_millis(20));
    shutdown.shutdown();
    let stats = handle.join().expect("server thread joins");

    assert_eq!(stats.accepted_connections, 0);
    assert_eq!(stats.served_requests, 0);
    assert!(stats.shutdown_requested);
}

#[test]
fn connect_and_close_does_not_kill_the_listener() {
    // REGRESSION: serve_stream used to write an ERR frame for every
    // read_line failure, including EmptyStream. Writing to a
    // peer-closed socket returns BrokenPipe (NetbatError::Io), which
    // serve_tcp_connection treated as fatal — a single
    // connect-and-close client would terminate the whole listener.
    // Now: EmptyStream short-circuits the write and bubbles cleanly
    // through serve_tcp_connection's graceful arm. The other read
    // failure paths use a best-effort write that swallows any
    // resulting BrokenPipe so they can't escalate to fatal either.
    let listener = localhost_listener();
    let addr = listener.local_addr().expect("listener addr");
    let shutdown = nb::ShutdownHandle::new();
    let server_shutdown = shutdown.clone();

    let config = nb::TcpServerConfig::default()
        .with_max_connections(3)
        .with_idle_sleep(Duration::from_millis(1));
    let handle = spawn_server("netbat-empty-stream", listener, config, server_shutdown);

    // Connect and close, twice. If the first connection-close had
    // killed the listener, the second connect would error before
    // the server's next accept.
    for _ in 0..2 {
        let stream = connect_client(addr);
        drop(stream);
        thread::sleep(Duration::from_millis(20));
    }

    // A real request must still go through afterwards — proves the
    // listener survived both hostile clients and still serves.
    let mut real = connect_client(addr);
    real.write_all(b"NETBAT/1 CALL ping 6869\n")
        .expect("write request");
    let mut response = String::new();
    BufReader::new(real)
        .read_line(&mut response)
        .expect("read response");
    assert_eq!(response, "OK 6869\n");

    shutdown.shutdown();
    let stats = handle.join().expect("server thread joins");
    assert_eq!(stats.served_requests, 1);
    assert_eq!(stats.accepted_connections, 3);
    // EmptyStream peers are NOT counted as failures — they're a
    // normal lifecycle event (TCP keepalive probes, eager TLS probes,
    // misbehaving health checks, etc.).
    assert_eq!(stats.failed_requests, 0);
    assert_eq!(stats.malformed_requests, 0);
}

#[test]
fn line_too_long_closes_connection_to_keep_framing_synchronized() {
    // REGRESSION (Codex P2): read_line returns LineTooLong as soon as
    // the buffer overflows `max_line_bytes`, without consuming through
    // the terminating `\n`. The unread tail is still on the wire — if
    // the connection loop kept iterating on `max_requests_per_connection
    // > 1`, the next read would start MID-FRAME and either re-emit
    // garbage ERR responses or mis-decode a fragment of the truncated
    // line as a fresh frame. Now we close the connection after a
    // LineTooLong: the ERR frame is delivered, the failure is counted,
    // and the framing window resyncs on the next CONNECTION (the only
    // safe boundary after a truncated line).
    let listener = localhost_listener();
    let addr = listener.local_addr().expect("listener addr");
    let shutdown = nb::ShutdownHandle::new();
    let server_shutdown = shutdown.clone();

    let tiny_limits = nb::Limits::default().with_max_line_bytes(32);
    let config = nb::TcpServerConfig::default()
        .with_limits(tiny_limits)
        .with_max_connections(1)
        .with_max_requests_per_connection(5);
    let handle = spawn_server("netbat-line-too-long", listener, config, server_shutdown);

    // First "line" overflows the 32-byte cap before any newline. The
    // trailing bytes after the overflow point would, under the old
    // behavior, get re-parsed as a fresh request on the same
    // connection.
    let oversize_then_pipelined =
        b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA-NETBAT/1 CALL ping 6869\n";
    let mut stream = connect_client(addr);
    stream
        .write_all(oversize_then_pipelined)
        .expect("write oversize line");

    let mut response = String::new();
    BufReader::new(&stream)
        .read_line(&mut response)
        .expect("read first response");
    assert!(
        response.starts_with("ERR line_too_long "),
        "expected line_too_long; got {response:?}",
    );

    // Server must close after the ERR. A read on a closed connection
    // returns 0 bytes (Ok(0)) or a connection-aborted error; either
    // way, no further response frame follows on this socket.
    let mut tail = String::new();
    let _ = BufReader::new(stream).read_line(&mut tail);
    assert!(
        tail.is_empty(),
        "server must close after LineTooLong; got trailing bytes {tail:?}",
    );

    shutdown.shutdown();
    let stats = handle.join().expect("server thread joins");
    assert_eq!(stats.accepted_connections, 1);
    assert_eq!(stats.served_requests, 0);
    assert_eq!(stats.failed_requests, 1);
    assert_eq!(stats.limit_failures, 1);
    assert_eq!(stats.malformed_requests, 0);
    assert_eq!(stats.runtime_failures, 0);
}

#[test]
fn peer_close_mid_response_does_not_kill_the_listener() {
    // REGRESSION: previously, `serve_stream`'s `stream.write_all(...)?`
    // would propagate BrokenPipe / ConnectionReset on the response
    // write as NetbatError::Io, and `serve_tcp_connection` escalated
    // that to the whole listener. A client that sent a valid request
    // and then immediately closed (or RST'd) without reading the
    // response could tear down the accept loop — a trivial remote
    // DoS. Now per-connection IO failures are dropped silently and
    // counted in `TcpServeStats::connection_io_failures`; the listener
    // continues accepting. The deterministic unit-level witness lives
    // at `tcp.rs::tests::peer_io_failure_does_not_propagate_from_connection`;
    // this end-to-end test confirms a subsequent clean request still
    // succeeds after the misbehaved peer.
    let listener = localhost_listener();
    let addr = listener.local_addr().expect("listener addr");
    let shutdown = nb::ShutdownHandle::new();
    let server_shutdown = shutdown.clone();

    let config = nb::TcpServerConfig::default()
        .with_max_connections(2)
        .with_idle_sleep(Duration::from_millis(1));
    let handle = spawn_server(
        "netbat-peer-close-mid-response",
        listener,
        config,
        server_shutdown,
    );

    // Misbehaved peer: send a valid request, then half-close both
    // directions and drop without reading the response. Whether the
    // server's write_all eventually surfaces BrokenPipe depends on
    // kernel buffer state; either way the listener must survive.
    let mut misbehaved = connect_client(addr);
    misbehaved
        .write_all(b"NETBAT/1 CALL ping 6869\n")
        .expect("write request from misbehaved peer");
    let _ = misbehaved.shutdown(std::net::Shutdown::Both);
    drop(misbehaved);
    thread::sleep(Duration::from_millis(30));

    // Clean peer afterward — proves the listener is still serving.
    let mut clean = connect_client(addr);
    clean
        .write_all(b"NETBAT/1 CALL ping 6869\n")
        .expect("write request from clean peer");
    let mut response = String::new();
    BufReader::new(clean)
        .read_line(&mut response)
        .expect("read response");
    assert_eq!(response, "OK 6869\n");

    shutdown.shutdown();
    let stats = handle.join().expect("server thread joins");
    assert_eq!(stats.accepted_connections, 2);
    // The clean peer must have been served. The misbehaved peer may
    // have been counted as a served request OR a connection IO
    // failure depending on kernel timing — both outcomes preserve
    // the invariant under test: the listener didn't die.
    assert!(
        stats.served_requests >= 1,
        "clean peer must be served; stats={stats:?}"
    );
    assert_eq!(stats.malformed_requests, 0);
    assert_eq!(stats.runtime_failures, 0);
}
