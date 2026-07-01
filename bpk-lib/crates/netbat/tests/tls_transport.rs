//! PROVES: NETBAT-TLS-ENCRYPTED-ROUND-TRIP, NETBAT-TLS-NO-CLEARTEXT.
//! CATCHES: a TLS listener that falls back to plaintext, a handshake that
//! blocks accepts, or a cleartext peer being served by a TLS listener.
//! SEEDED: localhost listeners with a committed test PKI (CA + server leaf).
//!
//! Whole file is gated on `feature = "tls"`: under the default `cargo test
//! -p netbat` it compiles to nothing (rustls is absent from that build).
#![cfg(feature = "tls")]

use netbat as nb;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use syncbat::{Core, EffectClass, Handler, HandlerResult, OperationDescriptor};

/// Committed throwaway localhost test PKI (100-year validity). Server identity
/// material only — never a production credential.
///   * CA_PEM    — the test CA, used as the client's trust anchor.
///   * CERT_PEM  — the server leaf (CN/SAN `localhost`, signed by the CA).
///   * KEY_PEM   — the server leaf's private key.
const CA_PEM: &[u8] = include_bytes!("fixtures/tls_test_ca_cert.pem");
const CERT_PEM: &[u8] = include_bytes!("fixtures/tls_test_cert.pem");
const KEY_PEM: &[u8] = include_bytes!("fixtures/tls_test_key.pem");

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
    builder.register(PING, PingHandler).expect("register ping");
    builder.without_receipts();
    builder.build().expect("core builds")
}

fn lifetime(value: usize) -> nb::ConnectionLimit {
    nb::ConnectionLimit::Lifetime(NonZeroUsize::new(value).expect("nonzero connection limit"))
}

/// Build the server-only TLS config from the committed PEM bytes.
fn tls_server_config() -> nb::TlsServerConfig {
    nb::TlsServerConfig::from_pem(CERT_PEM, KEY_PEM).expect("build TlsServerConfig from PEM")
}

/// Spawn a one-shot TLS request listener (Lifetime(1)) and return its stats
/// handle. A short read/write timeout guards the worker so a stalled handshake
/// can never wedge the listener's join.
fn spawn_tls_server(
    listener: TcpListener,
    shutdown: nb::ShutdownHandle,
) -> JoinHandle<nb::TcpServeStats> {
    let security = nb::TransportSecurity::Tls(tls_server_config());
    let config = nb::TcpServerConfig::default()
        .with_connection_limit(lifetime(1))
        .with_idle_sleep(Duration::from_millis(1))
        .with_timeouts(
            nb::IoTimeouts::default()
                .with_read(Some(Duration::from_secs(3)))
                .with_write(Some(Duration::from_secs(3))),
        );
    thread::Builder::new()
        .name("netbat-tls-server".to_owned())
        .spawn(move || {
            let factory = || core_with_ping();
            nb::serve_tcp_listener_secured(listener, factory, &config, &security, &shutdown)
                .expect("serve tls listener")
        })
        .expect("spawn tls server")
}

/// Connect a real rustls client that trusts the committed test cert.
fn tls_client(addr: std::net::SocketAddr) -> StreamOwned<ClientConnection, TcpStream> {
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(CA_PEM) {
        roots
            .add(cert.expect("parse fixture CA cert"))
            .expect("add fixture CA cert to client roots");
    }
    let config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("client protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
    let server_name = ServerName::try_from("localhost").expect("server name");
    let conn = ClientConnection::new(Arc::new(config), server_name).expect("client connection");
    let sock = TcpStream::connect(addr).expect("connect tls client");
    sock.set_read_timeout(Some(Duration::from_secs(3)))
        .expect("client read timeout");
    sock.set_write_timeout(Some(Duration::from_secs(3)))
        .expect("client write timeout");
    StreamOwned::new(conn, sock)
}

#[test]
fn tls_listener_serves_request_over_encrypted_stream() {
    // GREEN half: a rustls client completes a handshake and a full request
    // round-trip over the encrypted stream against a TLS listener.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind tls listener");
    let addr = listener.local_addr().expect("listener addr");
    let shutdown = nb::ShutdownHandle::new();
    let handle = spawn_tls_server(listener, shutdown);

    let mut client = tls_client(addr);
    client
        .write_all(b"NETBAT/1 CALL ping 6869\n")
        .expect("write request over tls");
    client.flush().expect("flush tls request");

    let mut response = String::new();
    {
        let mut reader = BufReader::new(&mut client);
        reader.read_line(&mut response).expect("read tls response");
    }
    assert_eq!(response, "OK 6869\n");

    // CONFIRMS the round-trip actually exercised rustls (not a plaintext
    // fallback): the client negotiated a concrete TLS protocol version, which
    // is only ever Some once a real handshake completed.
    assert!(
        client.conn.protocol_version().is_some(),
        "client must have negotiated a TLS version; got {:?}",
        client.conn.protocol_version()
    );

    let stats = handle.join().expect("server thread joins");
    assert_eq!(stats.accepted_connections, 1);
    assert_eq!(stats.served_requests, 1);
    assert_eq!(stats.tls_handshake_failures, 0);
}

#[test]
fn tls_listener_rejects_cleartext_client() {
    // RED-anchoring half: a PLAINTEXT client against the TLS listener must NOT
    // be served. Its cleartext bytes are not a valid ClientHello, so the
    // handshake fails, the connection is dropped, and the failure is counted.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind tls listener");
    let addr = listener.local_addr().expect("listener addr");
    let shutdown = nb::ShutdownHandle::new();
    let handle = spawn_tls_server(listener, shutdown);

    let mut cleartext = TcpStream::connect(addr).expect("connect cleartext client");
    cleartext
        .set_read_timeout(Some(Duration::from_secs(3)))
        .expect("cleartext read timeout");
    cleartext
        .write_all(b"NETBAT/1 CALL ping 6869\n")
        .expect("write cleartext request");

    // The TLS listener answers a cleartext peer with a TLS alert and/or an
    // immediate close — never an application response frame.
    let mut buf = [0_u8; 64];
    let received = match cleartext.read(&mut buf) {
        Ok(n) => &buf[..n],
        Err(_) => &buf[..0],
    };
    assert!(
        !received.starts_with(b"OK "),
        "TLS listener must not serve a cleartext peer; got {received:?}"
    );

    let stats = handle.join().expect("server thread joins");
    assert_eq!(stats.accepted_connections, 1);
    assert_eq!(stats.served_requests, 0);
    assert_eq!(
        stats.tls_handshake_failures, 1,
        "the cleartext peer's failed handshake must be counted; stats={stats:?}"
    );
}

#[test]
fn tls_server_config_loads_from_pem_files() {
    // Witness the from_pem_files (path-based) constructor and TransportSecurity
    // wrapping; a successful build proves the committed fixture round-trips
    // through the pki-types PEM path parsing.
    let dir = env!("CARGO_MANIFEST_DIR");
    let config = nb::TlsServerConfig::from_pem_files(
        format!("{dir}/tests/fixtures/tls_test_cert.pem"),
        format!("{dir}/tests/fixtures/tls_test_key.pem"),
    )
    .expect("build TlsServerConfig from PEM files");
    let security = nb::TransportSecurity::Tls(config);
    assert!(matches!(security, nb::TransportSecurity::Tls(_)));
}
