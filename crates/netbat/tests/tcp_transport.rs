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
    fn handle(&mut self, input: &[u8], _cx: &mut syncbat::Cx<'_>) -> HandlerResult {
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

    let config = nb::TcpServerConfig {
        max_connections: 1,
        ..nb::TcpServerConfig::default()
    };
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

    let config = nb::TcpServerConfig {
        max_connections: 1,
        max_requests_per_connection: 1,
        ..nb::TcpServerConfig::default()
    };
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

    let config = nb::TcpServerConfig {
        max_connections: 1,
        ..nb::TcpServerConfig::default()
    };
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
fn tcp_listener_rejects_unsupported_protocol_version() {
    let listener = localhost_listener();
    let addr = listener.local_addr().expect("listener addr");
    let shutdown = nb::ShutdownHandle::new();
    let server_shutdown = shutdown.clone();

    let config = nb::TcpServerConfig {
        max_connections: 1,
        ..nb::TcpServerConfig::default()
    };
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

    let config = nb::TcpServerConfig {
        max_connections: 1,
        limits: nb::Limits {
            max_line_bytes: 8,
            ..nb::Limits::default()
        },
        ..nb::TcpServerConfig::default()
    };
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

    let config = nb::TcpServerConfig {
        max_connections: 1,
        ..nb::TcpServerConfig::default()
    };
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

    let config = nb::TcpServerConfig {
        idle_sleep: Duration::from_millis(1),
        ..nb::TcpServerConfig::default()
    };
    let handle = spawn_server("netbat-tcp-shutdown", listener, config, server_shutdown);

    thread::sleep(Duration::from_millis(20));
    shutdown.shutdown();
    let stats = handle.join().expect("server thread joins");

    assert_eq!(stats.accepted_connections, 0);
    assert_eq!(stats.served_requests, 0);
    assert!(stats.shutdown_requested);
}
