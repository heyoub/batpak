//! PROVES: diff-scoped mutation survivors in the netbat transport layer are
//! killed by behavioural assertions on the public API.
//! CATCHES: drift in limit constants, subscription/reason/schema grammar
//! bounds, cursor round-trips, stream-line length checks, and the NETBAT/2
//! subscription serve/listen counters.

use std::io::Cursor;
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use netbat as nb;
use syncbat::{
    SessionPoll, SubscriptionRuntimeError, SubscriptionSession, SubscriptionSessionFactory,
};

// ---------------------------------------------------------------------------
// limits.rs: default byte budgets are products, not sums.
// ---------------------------------------------------------------------------

#[test]
fn default_stream_limit_constants_are_products() {
    // KILLS limits.rs:12/14/16 (`*` -> `+`). 4 + 1024 = 1028 and 32 + 1024 =
    // 1056 would survive a sum; only the product yields these values.
    assert_eq!(nb::DEFAULT_MAX_CURSOR_BYTES, 4 * 1024);
    assert_eq!(nb::DEFAULT_MAX_CURSOR_BYTES, 4096);
    assert_eq!(nb::DEFAULT_MAX_STREAM_PAYLOAD_BYTES, 32 * 1024);
    assert_eq!(nb::DEFAULT_MAX_STREAM_PAYLOAD_BYTES, 32768);
    assert_eq!(nb::DEFAULT_MAX_STREAM_ERROR_MESSAGE_BYTES, 4 * 1024);
    assert_eq!(nb::DEFAULT_MAX_STREAM_ERROR_MESSAGE_BYTES, 4096);
}

// ---------------------------------------------------------------------------
// stream_frame.rs: CursorBytes round-trip and grammar/length validators.
// ---------------------------------------------------------------------------

#[test]
fn cursor_bytes_into_bytes_returns_wrapped_bytes() {
    // KILLS stream_frame.rs:73 (-> vec![0] / vec![1] / vec![]).
    let bytes = nb::CursorBytes::new(vec![3, 1, 4, 1, 5]);
    assert_eq!(bytes.into_bytes(), vec![3, 1, 4, 1, 5]);
}

fn limits() -> nb::Limits {
    nb::Limits::default()
}

#[test]
fn decode_stream_line_accepts_line_at_exact_limit() {
    // KILLS stream_frame.rs:300 (`>` -> `>=`). A line whose length is exactly
    // max_line_bytes must decode; under `>=` it is rejected as too long.
    let line = b"NETBAT/2 SUB_CANCEL orders.open.v1 client.cancel\n";
    let lim = limits().with_max_line_bytes(line.len());
    let frame = nb::decode_stream_line(line, &lim).expect("exact-length frame decodes");
    assert!(matches!(frame, nb::StreamFrame::SubCancel(_)));
}

#[test]
fn decode_stream_line_lossy_hex_version_token() {
    // KILLS stream_frame.rs:660 (encode_hex_into_lossy -> "xyzzy" / "").
    // A non-UTF8 version token is rendered as its lowercase hex.
    let err = nb::decode_stream_line(b"\xff\xfe SUBSCRIBE x\n", &limits())
        .expect_err("non-utf8 version is unsupported");
    assert_eq!(
        err,
        nb::NetbatError::UnsupportedProtocolVersion {
            version: "0xfffe".to_owned()
        }
    );
}

#[test]
fn subscription_id_length_boundary_is_inclusive() {
    // KILLS stream_frame.rs:669 (`>` -> `>=` and `>` -> `==`). 128 bytes is the
    // last accepted length; 129 must be rejected. A generous transport limit
    // ensures the grammar's own 128-byte rule is what fires.
    let lim = limits().with_max_subscription_id_bytes(1000);
    let ok = format!("{}.v1", "a".repeat(125)); // 128 bytes
    assert_eq!(ok.len(), 128);
    nb::SubscriptionToken::new(ok, &lim).expect("128-byte id is valid");

    let too_long = format!("{}.v1", "a".repeat(126)); // 129 bytes
    assert_eq!(too_long.len(), 129);
    let err = nb::SubscriptionToken::new(too_long, &lim).expect_err("129-byte id is rejected");
    assert_eq!(
        err,
        nb::NetbatError::MalformedStreamFrame {
            reason: "subscription id longer than 128 bytes"
        }
    );
}

#[test]
fn subscription_id_leading_dot_reported_distinctly() {
    // KILLS stream_frame.rs:672 (`||` -> `&&`). A trailing-dot-only id must be
    // rejected with the leading/trailing-dot reason; under `&&` the check is
    // skipped and a different reason surfaces.
    let err =
        nb::SubscriptionToken::new("abc.", &limits()).expect_err("trailing-dot id is rejected");
    assert_eq!(
        err,
        nb::NetbatError::MalformedStreamFrame {
            reason: "subscription id has a leading or trailing '.'"
        }
    );
}

#[test]
fn subscription_id_version_zero_is_rejected() {
    // KILLS stream_frame.rs:703 (`||` -> `&&`). Version "0" must be rejected;
    // under `&&` the zero check is dead and "a.v0" would be accepted.
    let err = nb::SubscriptionToken::new("a.v0", &limits()).expect_err("version 0 is rejected");
    assert_eq!(
        err,
        nb::NetbatError::MalformedStreamFrame {
            reason: "subscription id version must start with 1-9"
        }
    );
}

#[test]
fn reason_code_length_boundary_is_inclusive() {
    // KILLS stream_frame.rs:716 (`>` -> `>=` and `>` -> `==`).
    let ok = "a".repeat(128);
    nb::StreamReasonCode::new(ok).expect("128-byte reason code is valid");
    let too_long = "a".repeat(129);
    let err = nb::StreamReasonCode::new(too_long).expect_err("129-byte reason code is rejected");
    assert_eq!(
        err,
        nb::NetbatError::MalformedStreamFrame {
            reason: "reason code longer than 128 bytes"
        }
    );
}

#[test]
fn payload_schema_ref_validates_and_bounds_length() {
    // KILLS stream_frame.rs:729 (-> Ok(())) and 732 (`>` -> `>=` and `==`).
    let empty_err = nb::PayloadSchemaRef::new("").expect_err("empty schema ref is rejected");
    assert_eq!(
        empty_err,
        nb::NetbatError::MalformedStreamFrame {
            reason: "empty payload schema ref"
        }
    );
    let ok = "a".repeat(256);
    nb::PayloadSchemaRef::new(ok).expect("256-byte schema ref is valid");
    let too_long = "a".repeat(257);
    let too_long_err =
        nb::PayloadSchemaRef::new(too_long).expect_err("257-byte schema ref is rejected");
    assert_eq!(
        too_long_err,
        nb::NetbatError::MalformedStreamFrame {
            reason: "payload schema ref longer than 256 bytes"
        }
    );
}

// ---------------------------------------------------------------------------
// stream_tcp.rs: serve_subscription_stream counters over in-memory handles.
// ---------------------------------------------------------------------------

enum Mode {
    End,
    Unknown,
}

struct FakeRuntime(Mode);

impl SubscriptionSessionFactory for FakeRuntime {
    fn open_session(
        &self,
        subscription_id: &str,
        _resume_cursor: Option<&[u8]>,
        _client_window: u32,
        _control_rx: flume::Receiver<syncbat::SessionControl>,
    ) -> Result<Box<dyn SubscriptionSession>, SubscriptionRuntimeError> {
        match self.0 {
            Mode::End => Ok(Box::new(EndSession)),
            Mode::Unknown => Err(SubscriptionRuntimeError::UnknownSubscription {
                id: subscription_id.to_owned(),
            }),
        }
    }
}

struct EndSession;

impl SubscriptionSession for EndSession {
    fn poll(&mut self, _timeout: Duration) -> Result<SessionPoll, SubscriptionRuntimeError> {
        Ok(SessionPoll::Ended)
    }
}

const SUBSCRIBE_LINE: &[u8] = b"NETBAT/2 SUBSCRIBE orders.open.v1 - 128\n";

#[test]
fn serve_subscription_stream_counts_served() -> Result<(), Box<dyn std::error::Error>> {
    // KILLS stream_tcp.rs:133 (`+=` -> `*=`/`-=` on served_subscriptions).
    let reader = Cursor::new(SUBSCRIBE_LINE.to_vec());
    let stats = nb::serve_subscription_stream(
        reader,
        std::io::sink(),
        &FakeRuntime(Mode::End),
        &nb::Limits::default(),
    )?;
    assert_eq!(stats.served_subscriptions, 1);
    assert_eq!(stats.failed_subscriptions, 0);
    Ok(())
}

#[test]
fn serve_subscription_stream_counts_open_failure() -> Result<(), Box<dyn std::error::Error>> {
    // KILLS stream_tcp.rs:120 (`+=` -> `*=`/`-=` on failed_subscriptions in the
    // open-session error arm).
    let reader = Cursor::new(SUBSCRIBE_LINE.to_vec());
    let stats = nb::serve_subscription_stream(
        reader,
        std::io::sink(),
        &FakeRuntime(Mode::Unknown),
        &nb::Limits::default(),
    )?;
    assert_eq!(stats.failed_subscriptions, 1);
    assert_eq!(stats.served_subscriptions, 0);
    Ok(())
}

#[test]
fn serve_subscription_stream_counts_malformed_pre_subscribe(
) -> Result<(), Box<dyn std::error::Error>> {
    // KILLS stream_tcp.rs:101/102 (`+=` -> `*=`/`-=` on failed_subscriptions
    // and malformed_pre_subscribe when the first frame is not SUBSCRIBE).
    let reader = Cursor::new(b"NETBAT/2 SUB_CANCEL orders.open.v1 client.cancel\n".to_vec());
    let stats = nb::serve_subscription_stream(
        reader,
        std::io::sink(),
        &FakeRuntime(Mode::End),
        &nb::Limits::default(),
    )?;
    assert_eq!(stats.failed_subscriptions, 1);
    assert_eq!(stats.malformed_pre_subscribe, 1);
    Ok(())
}

// ---------------------------------------------------------------------------
// stream_tcp.rs: serve_tcp_subscription_listener accept loop + counters.
// ---------------------------------------------------------------------------

#[test]
fn listener_serves_one_connection_then_exits_on_budget() -> Result<(), Box<dyn std::error::Error>> {
    // KILLS stream_tcp.rs:150 (fn -> Ok(Default)), 155 (`+=` on
    // accepted_connections), 152 (the `while` guard: delete `!`, `&&` -> `||`,
    // `<` -> `<=`/`==`/`>`), 181 (connection fn -> Ok(Default)), and 164
    // (WouldBlock guard -> false / `==` -> `!=`). The 30ms pre-connect delay
    // guarantees the nonblocking accept loop spins on WouldBlock first.
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    let mut config = nb::TcpSubscriptionServerConfig::default();
    config.max_connections = 1;
    let shutdown = nb::ShutdownHandle::new();
    let server_shutdown = shutdown.clone();
    let (tx, rx) = flume::bounded(1);
    let server = std::thread::Builder::new()
        .name("mk-listener-budget".to_owned())
        .spawn(move || {
            let result = nb::serve_tcp_subscription_listener(
                listener,
                &FakeRuntime(Mode::End),
                &config,
                &server_shutdown,
            );
            let _ = tx.send(result);
        })?;

    std::thread::sleep(Duration::from_millis(30));
    if let Ok(mut client) = TcpStream::connect(addr) {
        client.set_read_timeout(Some(Duration::from_secs(2)))?;
        let _ = std::io::Write::write_all(&mut client, SUBSCRIBE_LINE);
        let _ = std::io::Read::read(&mut client, &mut [0_u8; 64]);
    }

    let outcome = rx.recv_timeout(Duration::from_secs(3));
    shutdown.shutdown();
    let _ = server.join();

    let mut failures = Vec::new();
    match outcome {
        Ok(Ok(stats)) => {
            if stats.accepted_connections != 1 {
                failures.push(format!(
                    "accepted_connections = {}",
                    stats.accepted_connections
                ));
            }
            if stats.served_subscriptions != 1 {
                failures.push(format!(
                    "served_subscriptions = {}",
                    stats.served_subscriptions
                ));
            }
            if stats.shutdown_requested {
                failures.push("shutdown_requested unexpectedly true".to_owned());
            }
        }
        Ok(Err(error)) => failures.push(format!("listener returned error: {error:?}")),
        Err(_) => failures.push("listener did not exit within timeout".to_owned()),
    }
    assert!(failures.is_empty(), "{failures:?}");
    Ok(())
}

#[test]
fn listener_counts_idle_read_timeout_as_io_failure() -> Result<(), Box<dyn std::error::Error>> {
    // KILLS stream_tcp.rs:160 (`+=` -> `*=`/`-=` on connection_io_failures). An
    // idle peer that connects but never sends drives the per-connection read to
    // a timeout (an Io error), which the listener counts as a connection IO
    // failure.
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    let mut config = nb::TcpSubscriptionServerConfig::default();
    config.max_connections = 1;
    config.timeouts = nb::IoTimeouts::default()
        .with_read(Some(Duration::from_millis(100)))
        .with_write(Some(Duration::from_secs(2)));
    let shutdown = nb::ShutdownHandle::new();
    let server_shutdown = shutdown.clone();
    let (tx, rx) = flume::bounded(1);
    let server = std::thread::Builder::new()
        .name("mk-listener-idle".to_owned())
        .spawn(move || {
            let result = nb::serve_tcp_subscription_listener(
                listener,
                &FakeRuntime(Mode::End),
                &config,
                &server_shutdown,
            );
            let _ = tx.send(result);
        })?;

    // Connect and stay idle: send nothing, hold the socket open.
    let client = TcpStream::connect(addr)?;

    let outcome = rx.recv_timeout(Duration::from_secs(3));
    shutdown.shutdown();
    drop(client);
    let _ = server.join();

    let mut failures = Vec::new();
    match outcome {
        Ok(Ok(stats)) => {
            if stats.accepted_connections != 1 {
                failures.push(format!(
                    "accepted_connections = {}",
                    stats.accepted_connections
                ));
            }
            if stats.connection_io_failures != 1 {
                failures.push(format!(
                    "connection_io_failures = {}",
                    stats.connection_io_failures
                ));
            }
        }
        Ok(Err(error)) => failures.push(format!("listener returned error: {error:?}")),
        Err(_) => failures.push("listener did not exit within timeout".to_owned()),
    }
    assert!(failures.is_empty(), "{failures:?}");
    Ok(())
}
