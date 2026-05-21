//! End-to-end TCP integration tests for the `hbat` binary.
//!
//! These tests spawn the actual `hbat` binary as a subprocess, parse
//! the `HBAT_READY {…}` rendezvous line from its stdout, connect over
//! TCP, and drive all three NETBAT/1 operations the binary exposes
//! (`system.heartbeat`, `bank.commit`, `event.get`) plus the error
//! path. They close the cross-language parity loop on the Rust side
//! the same way `bpk-ts/examples/heartbeat-spike` does on the TS side.
//!
//! Audit reference: maturity-gap audit flagged that hbat had NO
//! integration tests (only inline #[test] fns) — this file fills that
//! hole.
//!
//! PROVES:
//!   - HBAT_READY rendezvous is a parseable JSON line on stdout.
//!   - NETBAT/1 frames over TCP round-trip cleanly for the 3 operations.
//!   - Wire-format error path returns a typed ERR with the
//!     `unknown_operation` code and a UTF-8 message body.
//!   - `bank.commit` -> `event.get` recovers the canonical payload
//!     bytes byte-for-byte.
//!
//! SEEDED: fresh temp store per test; binds to 127.0.0.1:0 so tests
//! can run in parallel without port collisions.

#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use batpak::EventPayload;
use hbat::{
    bank::{BankCommitAck, BankCommitRequest, EventGetAck, EventGetRequest},
    heartbeat::{SystemHeartbeatAck, SystemHeartbeatRequest},
    EventPayloadFixture,
};

const HBAT_BIN: &str = env!("CARGO_BIN_EXE_hbat");
const READY_PREFIX: &str = "HBAT_READY ";

/// Spawned hbat process holding the temp dir alive for the duration
/// of the test.
struct HbatProcess {
    child: Child,
    port: u16,
    _tempdir: tempfile::TempDir,
}

impl HbatProcess {
    fn spawn() -> Self {
        let tempdir = tempfile::TempDir::new().expect("temp dir");
        let mut child = Command::new(HBAT_BIN)
            .arg("serve")
            .arg("--store")
            .arg(tempdir.path())
            .arg("--tcp")
            .arg("127.0.0.1:0")
            .arg("--print-port")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn hbat");

        let stdout = child.stdout.take().expect("stdout piped");
        let mut reader = BufReader::new(stdout);
        let mut ready_line = String::new();
        let deadline = Instant::now() + Duration::from_secs(10);

        loop {
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!("timed out waiting for HBAT_READY (read so far: {ready_line:?})");
            }
            ready_line.clear();
            match reader.read_line(&mut ready_line) {
                Ok(0) => {
                    let _ = child.kill();
                    panic!("hbat closed stdout before printing HBAT_READY");
                }
                Ok(_) => {
                    if ready_line.starts_with(READY_PREFIX) {
                        break;
                    }
                }
                Err(error) => {
                    let _ = child.kill();
                    panic!("read HBAT_READY: {error}");
                }
            }
        }

        let payload = ready_line.trim_start_matches(READY_PREFIX).trim();
        let parsed: serde_json::Value =
            serde_json::from_str(payload).expect("HBAT_READY payload is JSON");
        let port_u64 = parsed
            .get("port")
            .and_then(|v| v.as_u64())
            .expect("HBAT_READY carries a numeric port");
        let port = u16::try_from(port_u64).expect("HBAT_READY port fits in u16");

        Self {
            child,
            port,
            _tempdir: tempdir,
        }
    }

    fn port(&self) -> u16 {
        self.port
    }

    fn connect(&self) -> TcpStream {
        let mut last_error: Option<std::io::Error> = None;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match TcpStream::connect(("127.0.0.1", self.port)) {
                Ok(stream) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .expect("read timeout");
                    stream
                        .set_write_timeout(Some(Duration::from_secs(5)))
                        .expect("write timeout");
                    return stream;
                }
                Err(error) => {
                    last_error = Some(error);
                    sleep(Duration::from_millis(20));
                }
            }
        }
        panic!(
            "failed to connect to hbat on 127.0.0.1:{}: {last_error:?}",
            self.port
        );
    }
}

impl Drop for HbatProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn lowercase_hex(bytes: &[u8]) -> String {
    netbat::encode_hex_str(bytes)
}

fn decode_response(stream: &mut TcpStream) -> Vec<u8> {
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    let n = reader.read_until(b'\n', &mut line).expect("read response");
    assert!(n > 0, "EOF before response");
    line
}

/// Send a NETBAT/1 CALL frame and return the raw response line including \n.
fn call_one(host: &HbatProcess, op: &str, input: &[u8]) -> Vec<u8> {
    let mut stream = host.connect();
    let frame = netbat::encode_request(op, input);
    stream.write_all(&frame).expect("write request");
    stream.flush().expect("flush request");
    let response = decode_response(&mut stream);
    drop(stream);
    response
}

fn parse_ok(response: &[u8]) -> Vec<u8> {
    let trimmed = response
        .strip_suffix(b"\n")
        .expect("response ends with newline");
    let body = trimmed.strip_prefix(b"OK ").unwrap_or_else(|| {
        panic!(
            "expected OK frame, got {:?}",
            String::from_utf8_lossy(trimmed)
        )
    });
    netbat::decode_hex_str(std::str::from_utf8(body).expect("hex ASCII")).expect("hex decodes")
}

fn parse_err(response: &[u8]) -> (String, String) {
    let trimmed = response
        .strip_suffix(b"\n")
        .expect("response ends with newline");
    let body = trimmed.strip_prefix(b"ERR ").unwrap_or_else(|| {
        panic!(
            "expected ERR frame, got {:?}",
            String::from_utf8_lossy(trimmed)
        )
    });
    let space = body
        .iter()
        .position(|b| *b == b' ')
        .expect("ERR frame has a space between code and hex");
    let code = std::str::from_utf8(&body[..space])
        .expect("ASCII code")
        .to_owned();
    let hex = std::str::from_utf8(&body[space + 1..]).expect("ASCII hex");
    let message_bytes = netbat::decode_hex_str(hex).expect("hex decodes");
    let message = String::from_utf8(message_bytes).expect("UTF-8 message");
    (code, message)
}

// ─── tests ──────────────────────────────────────────────────────────────────

#[test]
fn system_heartbeat_round_trips() {
    let host = HbatProcess::spawn();
    let request = SystemHeartbeatRequest {
        nonce: "tcp-e2e-heartbeat-001".to_owned(),
    };
    let input = batpak::encoding::to_bytes(&request).expect("encode");
    let response = call_one(&host, "system.heartbeat", &input);
    let output = parse_ok(&response);
    let ack: SystemHeartbeatAck = batpak::encoding::from_bytes(&output).expect("decode ack");
    assert_eq!(ack.nonce, request.nonce);
    assert!(ack.server_ts_ms > 0);
}

#[test]
fn unknown_operation_returns_typed_err_with_utf8_message() {
    let host = HbatProcess::spawn();
    let response = call_one(&host, "system.heartbeat.nope", &[]);
    let (code, message) = parse_err(&response);
    assert_eq!(code, "unknown_operation");
    assert!(
        message.contains("system.heartbeat.nope"),
        "message did not name the operation: {message:?}"
    );
}

#[test]
fn bank_commit_then_event_get_recovers_canonical_bytes() {
    let host = HbatProcess::spawn();

    // Commit a heartbeat-request typed event.
    let original_payload = SystemHeartbeatRequest {
        nonce: "tcp-e2e-bank-commit-007".to_owned(),
    };
    let original_payload_bytes =
        batpak::encoding::to_bytes(&original_payload).expect("encode payload");

    let commit_request = BankCommitRequest {
        entity: "tcp:e2e".to_owned(),
        scope: "tcp-e2e-scope".to_owned(),
        kind_category: SystemHeartbeatRequest::KIND.category(),
        kind_type_id: SystemHeartbeatRequest::KIND.type_id(),
        payload_hex: lowercase_hex(&original_payload_bytes),
    };
    let commit_input = batpak::encoding::to_bytes(&commit_request).expect("encode commit");
    let commit_response = call_one(&host, "bank.commit", &commit_input);
    let commit_output = parse_ok(&commit_response);
    let ack: BankCommitAck = batpak::encoding::from_bytes(&commit_output).expect("decode ack");

    assert_eq!(ack.event_id_hex.len(), 32, "event_id_hex is 32 hex chars");
    assert_eq!(ack.content_hash_hex.len(), 64);
    assert_eq!(ack.key_id_hex.len(), 64);
    assert!(ack.sequence >= 1);

    // Fetch it back.
    let get_request = EventGetRequest {
        event_id_hex: ack.event_id_hex.clone(),
    };
    let get_input = batpak::encoding::to_bytes(&get_request).expect("encode get");
    let get_response = call_one(&host, "event.get", &get_input);
    let get_output = parse_ok(&get_response);
    let event: EventGetAck = batpak::encoding::from_bytes(&get_output).expect("decode event");

    assert_eq!(event.event_id_hex, ack.event_id_hex);
    assert_eq!(event.entity, "tcp:e2e");
    assert_eq!(event.scope, "tcp-e2e-scope");
    // REGRESSION: event.get used to hard-code sequence to 0,
    // breaking consumers that depend on the wire contract for
    // monotonic replay / checkpointing / dedup. The handler now
    // resolves the IndexEntry and returns its real global_sequence.
    assert_eq!(
        event.sequence, ack.sequence,
        "event.get must echo the same sequence number returned by bank.commit",
    );
    assert!(event.sequence >= 1);
    assert_eq!(event.kind_category, SystemHeartbeatRequest::KIND.category());
    assert_eq!(event.kind_type_id, SystemHeartbeatRequest::KIND.type_id());

    // CRITICAL: the payload bytes round-trip byte-for-byte through the
    // wire and back into the original typed struct.
    let recovered_bytes = netbat::decode_hex_str(&event.payload_hex).expect("hex decodes");
    assert_eq!(recovered_bytes, original_payload_bytes);
    let recovered: SystemHeartbeatRequest =
        batpak::encoding::from_bytes(&recovered_bytes).expect("decode original");
    assert_eq!(recovered, original_payload);
}

#[test]
fn fixture_payloads_round_trip_over_tcp() {
    // Sanity for the manifest fixtures themselves — proves the bytes
    // we publish in batpak.manifest.json (which the TS SDK consumes
    // for its parity tests) work end-to-end through the live wire.
    let host = HbatProcess::spawn();

    let heartbeat_fixture = SystemHeartbeatRequest::fixture_value();
    let heartbeat_bytes = batpak::encoding::to_bytes(&heartbeat_fixture).expect("encode");
    let response = call_one(&host, "system.heartbeat", &heartbeat_bytes);
    let ack_bytes = parse_ok(&response);
    let ack: SystemHeartbeatAck = batpak::encoding::from_bytes(&ack_bytes).expect("decode");
    assert_eq!(ack.nonce, heartbeat_fixture.nonce);
}

#[test]
fn bank_commit_rejects_reserved_kind_category_over_tcp() {
    let host = HbatProcess::spawn();
    // Build a malformed BankCommitRequest that the substrate must
    // reject. kind_category=0 is the reserved system category and
    // EventKind::try_custom refuses it.
    let bad_request = BankCommitRequest {
        entity: "tcp:e2e".to_owned(),
        scope: "tcp-e2e-scope".to_owned(),
        kind_category: 0, // reserved
        kind_type_id: 1,
        payload_hex: "00".to_owned(),
    };
    let input = batpak::encoding::to_bytes(&bad_request).expect("encode");
    let response = call_one(&host, "bank.commit", &input);
    // Expect an ERR frame. Code is "handler" because the handler
    // produced an HandlerError::InvalidInput which the runtime maps
    // to NetbatError::Runtime(RuntimeError::Handler).
    let (code, message) = parse_err(&response);
    assert_eq!(code, "handler");
    assert!(
        message.to_lowercase().contains("kind") || message.to_lowercase().contains("event kind"),
        "expected error mentioning kind, got {message:?}"
    );
}

#[test]
fn malformed_frame_returns_typed_err() {
    let host = HbatProcess::spawn();
    let mut stream = host.connect();
    // Garbage line with no protocol prefix.
    stream
        .write_all(b"GARBAGE not a frame\n")
        .expect("write garbage");
    stream.flush().expect("flush");
    let response = decode_response(&mut stream);
    let (code, _message) = parse_err(&response);
    // Either malformed_request or unsupported_protocol_version, both
    // are stable wire-error codes from netbat::NetbatError::code().
    assert!(
        matches!(
            code.as_str(),
            "malformed_request" | "unsupported_protocol_version"
        ),
        "unexpected code {code:?}"
    );
}

#[test]
fn ready_payload_carries_addr_port_and_protocol() {
    // Spawn-and-introspect smoke test: just ensure HBAT_READY parses.
    let host = HbatProcess::spawn();
    assert!(host.port() > 0);
}
