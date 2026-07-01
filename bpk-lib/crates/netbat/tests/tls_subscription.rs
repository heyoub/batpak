//! PROVES: NETBAT-TLS-SUBSCRIPTION-ENCRYPTED-STREAM,
//! NETBAT-TLS-SUBSCRIPTION-CONTROL-HONORED, NETBAT-TLS-SUBSCRIPTION-NO-CLEARTEXT.
//! CATCHES: a TLS subscription listener that falls back to plaintext, a session
//! that cannot stream events over the encrypted stream, a client control frame
//! (SUB_CANCEL) that the single-threaded TLS session loop fails to honor, or a
//! cleartext peer being served by a TLS subscription listener.
//! SEEDED: localhost listeners with the committed test PKI (CA + server leaf),
//! and a runtime whose session delivers one SUB_EVENT then stays open until the
//! client cancels (whereupon it emits SUB_END).
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
use syncbat::{
    RuntimeCursor, SessionControl, SessionDelivery, SessionEnd, SessionEventDelivery, SessionPoll,
    SubscriptionRuntimeError, SubscriptionSession, SubscriptionSessionFactory,
};

/// Committed throwaway localhost test PKI (server identity material only).
const CA_PEM: &[u8] = include_bytes!("fixtures/tls_test_ca_cert.pem");
const CERT_PEM: &[u8] = include_bytes!("fixtures/tls_test_cert.pem");
const KEY_PEM: &[u8] = include_bytes!("fixtures/tls_test_key.pem");

const WIRE_SCHEMA: &str = "batpak.event-stream-envelope.v1";
const SUBSCRIBE_LINE: &[u8] = b"NETBAT/2 SUBSCRIBE orders.open.v1 - 128\n";
const CANCEL_LINE: &[u8] = b"NETBAT/2 SUB_CANCEL orders.open.v1 client.cancel\n";

/// Opens a long-lived session: it delivers exactly one `SUB_EVENT`, then stays
/// open until the client cancels (or disconnects). On `Cancel` it emits a
/// terminal `SUB_END`, which the client reads back over the encrypted stream —
/// the observable proof the control frame was received, decoded, and honored by
/// the single-threaded TLS session loop.
struct LiveRuntime;

impl SubscriptionSessionFactory for LiveRuntime {
    fn open_session(
        &self,
        subscription_id: &str,
        _resume_cursor: Option<&[u8]>,
        _client_window: u32,
        control_rx: flume::Receiver<SessionControl>,
    ) -> Result<Box<dyn SubscriptionSession>, SubscriptionRuntimeError> {
        Ok(Box::new(LiveSession {
            subscription_id: subscription_id.to_owned(),
            event: Some(event_delivery(subscription_id)),
            control_rx,
        }))
    }
}

struct LiveSession {
    subscription_id: String,
    event: Option<SessionDelivery>,
    control_rx: flume::Receiver<SessionControl>,
}

impl LiveSession {
    fn end_frame(&self) -> SessionDelivery {
        SessionDelivery::End(SessionEnd {
            subscription_id: self.subscription_id.clone(),
            reason_code: "client_cancelled",
            cursor_after: None,
        })
    }
}

impl SubscriptionSession for LiveSession {
    fn poll(&mut self, timeout: Duration) -> Result<SessionPoll, SubscriptionRuntimeError> {
        if let Some(event) = self.event.take() {
            return Ok(SessionPoll::Delivery(event));
        }
        match self.control_rx.try_recv() {
            Ok(SessionControl::Cancel) => return Ok(SessionPoll::Delivery(self.end_frame())),
            Ok(SessionControl::Disconnected) => return Ok(SessionPoll::Ended),
            Ok(_) => {}
            Err(flume::TryRecvError::Disconnected) => return Ok(SessionPoll::Ended),
            Err(flume::TryRecvError::Empty) => {}
        }
        // Stay open: block on the control lane for the poll window, then report
        // Blocked. The client's cancel (forwarded by the drain) ends the session.
        match self.control_rx.recv_timeout(timeout) {
            Ok(SessionControl::Cancel) => Ok(SessionPoll::Delivery(self.end_frame())),
            Ok(SessionControl::Disconnected) => Ok(SessionPoll::Ended),
            Ok(_) => Ok(SessionPoll::Blocked),
            Err(flume::RecvTimeoutError::Timeout) => Ok(SessionPoll::Blocked),
            Err(flume::RecvTimeoutError::Disconnected) => Ok(SessionPoll::Ended),
        }
    }
}

fn event_delivery(subscription_id: &str) -> SessionDelivery {
    SessionDelivery::Event(SessionEventDelivery {
        subscription_id: subscription_id.to_owned(),
        delivery_index: 1,
        cursor_before: RuntimeCursor::from_bytes(vec![0]),
        cursor_after: RuntimeCursor::from_bytes(vec![1]),
        wire_payload_schema_ref: WIRE_SCHEMA.to_owned(),
        envelope_bytes: b"canonical-envelope-fixture".to_vec(),
    })
}

fn lifetime(value: usize) -> nb::ConnectionLimit {
    nb::ConnectionLimit::Lifetime(NonZeroUsize::new(value).expect("nonzero connection limit"))
}

fn tls_server_config() -> nb::TlsServerConfig {
    nb::TlsServerConfig::from_pem(CERT_PEM, KEY_PEM).expect("build TlsServerConfig from PEM")
}

/// Spawn a one-shot TLS subscription listener (`Lifetime(1)`). Short read/write
/// timeouts guard the worker so a stalled handshake cannot wedge the join.
fn spawn_tls_subscription_server(
    listener: TcpListener,
    shutdown: nb::ShutdownHandle,
) -> JoinHandle<nb::TcpSubscriptionServeStats> {
    let security = nb::TransportSecurity::Tls(tls_server_config());
    let mut config = nb::TcpSubscriptionServerConfig::default();
    config.connection_limit = lifetime(1);
    config.idle_sleep = Duration::from_millis(1);
    config.timeouts = nb::IoTimeouts::default()
        .with_read(Some(Duration::from_secs(3)))
        .with_write(Some(Duration::from_secs(3)));
    thread::Builder::new()
        .name("netbat-tls-sub-server".to_owned())
        .spawn(move || {
            nb::serve_tcp_subscription_listener_secured(
                listener,
                LiveRuntime,
                &config,
                &security,
                &shutdown,
            )
            .expect("serve tls subscription listener")
        })
        .expect("spawn tls subscription server")
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
fn tls_subscription_streams_event_and_honors_cancel_over_encrypted_stream() {
    // GREEN: a rustls client subscribes over TLS, receives its streamed
    // SUB_EVENT over the encrypted stream, then cancels — and the
    // single-threaded TLS session loop reads that control frame and honors it,
    // emitting the terminal SUB_END the client reads back.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind tls sub listener");
    let addr = listener.local_addr().expect("listener addr");
    let shutdown = nb::ShutdownHandle::new();
    let handle = spawn_tls_subscription_server(listener, shutdown);

    let mut reader = BufReader::new(tls_client(addr));
    reader
        .get_mut()
        .write_all(SUBSCRIBE_LINE)
        .expect("subscribe over tls");
    reader.get_mut().flush().expect("flush subscribe");

    let mut event_line = String::new();
    reader.read_line(&mut event_line).expect("read SUB_EVENT");
    assert!(
        event_line.contains("SUB_EVENT"),
        "client must receive its streamed event over TLS; got {event_line:?}"
    );

    // CONFIRMS the stream actually exercised rustls (not a plaintext fallback):
    // a concrete TLS version is negotiated only once a real handshake completed.
    assert!(
        reader.get_ref().conn.protocol_version().is_some(),
        "client must have negotiated a TLS version; got {:?}",
        reader.get_ref().conn.protocol_version()
    );

    // Send a control frame over the encrypted stream and observe it honored.
    reader
        .get_mut()
        .write_all(CANCEL_LINE)
        .expect("cancel over tls");
    reader.get_mut().flush().expect("flush cancel");

    let mut end_line = String::new();
    reader.read_line(&mut end_line).expect("read SUB_END");
    assert!(
        end_line.contains("SUB_END"),
        "the TLS control drain must honor SUB_CANCEL with a terminal SUB_END; got {end_line:?}"
    );

    let stats = handle.join().expect("server thread joins");
    assert_eq!(stats.accepted_connections, 1);
    assert!(
        stats.served_subscriptions >= 1,
        "the subscription must be served; stats={stats:?}"
    );
    assert_eq!(
        stats.tls_handshake_failures, 0,
        "the trusting rustls client handshakes cleanly; stats={stats:?}"
    );
    assert_eq!(
        stats.worker_panics, 0,
        "no session panicked; stats={stats:?}"
    );
}

#[test]
fn tls_subscription_rejects_cleartext_client() {
    // RED-anchoring: a PLAINTEXT client against the TLS subscription listener
    // must NOT be served. Its cleartext SUBSCRIBE bytes are not a valid
    // ClientHello, so the handshake fails, the session is dropped, and the
    // failure is counted — never an application SUB_EVENT/SUB_END frame.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind tls sub listener");
    let addr = listener.local_addr().expect("listener addr");
    let shutdown = nb::ShutdownHandle::new();
    let handle = spawn_tls_subscription_server(listener, shutdown);

    let mut cleartext = TcpStream::connect(addr).expect("connect cleartext client");
    cleartext
        .set_read_timeout(Some(Duration::from_secs(3)))
        .expect("cleartext read timeout");
    cleartext
        .write_all(SUBSCRIBE_LINE)
        .expect("write cleartext subscribe");

    // The TLS listener answers a cleartext peer with a TLS alert and/or an
    // immediate close — never a NETBAT/2 stream frame.
    let mut buf = [0_u8; 64];
    let received = match cleartext.read(&mut buf) {
        Ok(n) => &buf[..n],
        Err(_) => &buf[..0],
    };
    assert!(
        !received.starts_with(b"NETBAT/2"),
        "TLS listener must not serve a cleartext peer; got {received:?}"
    );

    let stats = handle.join().expect("server thread joins");
    assert_eq!(stats.accepted_connections, 1);
    assert_eq!(stats.served_subscriptions, 0);
    assert_eq!(
        stats.tls_handshake_failures, 1,
        "the cleartext peer's failed handshake must be counted; stats={stats:?}"
    );
}
