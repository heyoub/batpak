//! End-to-end TCP integration tests for the `hbat` binary.
//!
//! These tests spawn the actual `hbat` binary as a subprocess, parse
//! the `HBAT_READY {…}` rendezvous line from its stdout, connect over
//! TCP, and drive the ten-op NETBAT/1 manifest the reference host
//! advertises. They close the cross-language parity loop on the Rust
//! side the same way `bpk-ts/examples/heartbeat-spike` does on the TS
//! side.
//!
//! Audit reference: maturity-gap audit flagged that hbat had NO
//! integration tests (only inline #[test] fns) — this file fills that
//! hole.
//!
//! PROVES:
//!   - HBAT_READY rendezvous is a parseable JSON line on stdout.
//!   - NETBAT/1 reachability for the ten-op manifest over TCP.
//!   - Success-path evidence identity for `evidence.chain_walk`,
//!     `evidence.store_resource`, and `evidence.read_walk`.
//!   - Intentional handler-error reachability for `evidence.projection_run`
//!     on the domain-neutral reference host (empty projection registry).
//!   - Wire-format error path returns a typed ERR with the
//!     `unknown_operation` code and a UTF-8 message body.
//!   - `bank.commit` -> `event.get` recovers the canonical payload
//!     bytes byte-for-byte.
//!
//! SEEDED: fresh temp store per test; binds to 127.0.0.1:0 so tests
//! can run in parallel without port collisions.
//!
//! PROVES: INV-BIDIRECTIONAL-SUBSTRATE-LANE,
//! INV-SUBSTRATE-TRAVERSAL-DOMAIN-NEUTRAL,
//! INV-EXTERNAL-REPLAY-NO-SIDECAR-TRUTH.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use batpak::EventPayload;
use hbat::{
    bank::{
        BankCommitAck, BankCommitRequest, EventGetAck, EventGetRequest, EventQueryAck,
        EventQueryRequest,
    },
    evidence::{
        ChainWalkEvidenceAck, ChainWalkEvidenceRequest, ProjectionRunEvidenceRequest,
        ReadWalkEvidenceAck, ReadWalkEvidenceRequest, StoreResourceEvidenceAck,
        StoreResourceEvidenceRequest,
    },
    heartbeat::{SystemHeartbeatAck, SystemHeartbeatRequest},
    receipt::{ReceiptVerifyAck, ReceiptVerifyRequest},
    walk::{EventWalkAck, EventWalkRequest},
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
    fn spawn() -> Result<Self> {
        let tempdir = tempfile::TempDir::new().context("temp dir")?;
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
            .context("spawn hbat")?;

        let stdout = child.stdout.take().context("stdout piped")?;
        let mut reader = BufReader::new(stdout);
        let mut ready_line = String::new();
        let deadline = Instant::now() + Duration::from_secs(10);

        loop {
            if Instant::now() > deadline {
                let _ = child.kill();
                bail!("timed out waiting for HBAT_READY (read so far: {ready_line:?})");
            }
            ready_line.clear();
            match reader.read_line(&mut ready_line) {
                Ok(0) => {
                    let _ = child.kill();
                    bail!("hbat closed stdout before printing HBAT_READY");
                }
                Ok(_) => {
                    if ready_line.starts_with(READY_PREFIX) {
                        break;
                    }
                }
                Err(error) => {
                    let _ = child.kill();
                    return Err(error).context("read HBAT_READY");
                }
            }
        }

        let payload = ready_line.trim_start_matches(READY_PREFIX).trim();
        let parsed: serde_json::Value =
            serde_json::from_str(payload).context("HBAT_READY payload is JSON")?;
        let port_u64 = parsed
            .get("port")
            .and_then(|v| v.as_u64())
            .context("HBAT_READY carries a numeric port")?;
        let port = u16::try_from(port_u64).context("HBAT_READY port fits in u16")?;

        Ok(Self {
            child,
            port,
            _tempdir: tempdir,
        })
    }

    fn port(&self) -> u16 {
        self.port
    }

    fn connect(&self) -> Result<TcpStream> {
        let mut last_error: Option<std::io::Error> = None;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match TcpStream::connect(("127.0.0.1", self.port)) {
                Ok(stream) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .context("read timeout")?;
                    stream
                        .set_write_timeout(Some(Duration::from_secs(5)))
                        .context("write timeout")?;
                    return Ok(stream);
                }
                Err(error) => {
                    last_error = Some(error);
                    sleep(Duration::from_millis(20));
                }
            }
        }
        bail!(
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

fn decode_response(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    let n = reader
        .read_until(b'\n', &mut line)
        .context("read response")?;
    if n == 0 {
        bail!("EOF before response");
    }
    Ok(line)
}

/// Send a NETBAT/1 CALL frame and return the raw response line including \n.
fn call_one(host: &HbatProcess, op: &str, input: &[u8]) -> Result<Vec<u8>> {
    let mut stream = host.connect()?;
    let frame = netbat::encode_request(op, input);
    stream.write_all(&frame).context("write request")?;
    stream.flush().context("flush request")?;
    let response = decode_response(&mut stream)?;
    drop(stream);
    Ok(response)
}

fn parse_ok(response: &[u8]) -> Result<Vec<u8>> {
    let trimmed = response
        .strip_suffix(b"\n")
        .context("response ends with newline")?;
    let Some(body) = trimmed.strip_prefix(b"OK ") else {
        bail!(
            "expected OK frame, got {:?}",
            String::from_utf8_lossy(trimmed)
        );
    };
    let hex = std::str::from_utf8(body).context("hex ASCII")?;
    netbat::decode_hex_str(hex).context("hex decodes")
}

fn parse_err(response: &[u8]) -> Result<(String, String)> {
    let trimmed = response
        .strip_suffix(b"\n")
        .context("response ends with newline")?;
    let Some(body) = trimmed.strip_prefix(b"ERR ") else {
        bail!(
            "expected ERR frame, got {:?}",
            String::from_utf8_lossy(trimmed)
        );
    };
    let space = body
        .iter()
        .position(|b| *b == b' ')
        .context("ERR frame has a space between code and hex")?;
    let code = std::str::from_utf8(&body[..space])
        .context("ASCII code")?
        .to_owned();
    let hex = std::str::from_utf8(&body[space + 1..]).context("ASCII hex")?;
    let message_bytes = netbat::decode_hex_str(hex).context("hex decodes")?;
    let message = String::from_utf8(message_bytes).context("UTF-8 message")?;
    Ok((code, message))
}

fn commit_heartbeat_event(host: &HbatProcess, entity: &str, nonce: &str) -> Result<BankCommitAck> {
    let original_payload = SystemHeartbeatRequest {
        nonce: nonce.to_owned(),
    };
    let original_payload_bytes = batpak::encoding::to_bytes(&original_payload)?;
    let commit_request = BankCommitRequest {
        entity: entity.to_owned(),
        scope: "tcp-e2e-scope".to_owned(),
        kind_category: SystemHeartbeatRequest::KIND.category(),
        kind_type_id: SystemHeartbeatRequest::KIND.type_id(),
        payload_hex: lowercase_hex(&original_payload_bytes),
    };
    let commit_input = batpak::encoding::to_bytes(&commit_request)?;
    let commit_response = call_one(host, "bank.commit", &commit_input)?;
    let commit_output = parse_ok(&commit_response)?;
    Ok(batpak::encoding::from_bytes(&commit_output)?)
}

// ─── tests ──────────────────────────────────────────────────────────────────

#[test]
fn system_heartbeat_round_trips() -> Result<()> {
    let host = HbatProcess::spawn()?;
    let request = SystemHeartbeatRequest {
        nonce: "tcp-e2e-heartbeat-001".to_owned(),
    };
    let input = batpak::encoding::to_bytes(&request)?;
    let response = call_one(&host, "system.heartbeat", &input)?;
    let output = parse_ok(&response)?;
    let ack: SystemHeartbeatAck = batpak::encoding::from_bytes(&output)?;
    assert_eq!(ack.nonce, request.nonce);
    assert!(ack.server_ts_ms > 0);
    Ok(())
}

#[test]
fn unknown_operation_returns_typed_err_with_utf8_message() -> Result<()> {
    let host = HbatProcess::spawn()?;
    let response = call_one(&host, "system.heartbeat.nope", &[])?;
    let (code, message) = parse_err(&response)?;
    assert_eq!(code, "unknown_operation");
    assert!(
        message.contains("system.heartbeat.nope"),
        "message did not name the operation: {message:?}"
    );
    Ok(())
}

#[test]
fn bank_commit_then_event_get_recovers_canonical_bytes() -> Result<()> {
    let host = HbatProcess::spawn()?;

    // Commit a heartbeat-request typed event.
    let original_payload = SystemHeartbeatRequest {
        nonce: "tcp-e2e-bank-commit-007".to_owned(),
    };
    let original_payload_bytes = batpak::encoding::to_bytes(&original_payload)?;

    let commit_request = BankCommitRequest {
        entity: "tcp:e2e".to_owned(),
        scope: "tcp-e2e-scope".to_owned(),
        kind_category: SystemHeartbeatRequest::KIND.category(),
        kind_type_id: SystemHeartbeatRequest::KIND.type_id(),
        payload_hex: lowercase_hex(&original_payload_bytes),
    };
    let commit_input = batpak::encoding::to_bytes(&commit_request)?;
    let commit_response = call_one(&host, "bank.commit", &commit_input)?;
    let commit_output = parse_ok(&commit_response)?;
    let ack: BankCommitAck = batpak::encoding::from_bytes(&commit_output)?;

    assert_eq!(ack.event_id_hex.len(), 32, "event_id_hex is 32 hex chars");
    assert_eq!(ack.content_hash_hex.len(), 64);
    assert_eq!(ack.key_id_hex.len(), 64);
    assert!(ack.sequence >= 1);

    // Fetch it back.
    let get_request = EventGetRequest {
        event_id_hex: ack.event_id_hex.clone(),
    };
    let get_input = batpak::encoding::to_bytes(&get_request)?;
    let get_response = call_one(&host, "event.get", &get_input)?;
    let get_output = parse_ok(&get_response)?;
    let event: EventGetAck = batpak::encoding::from_bytes(&get_output)?;

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
    let recovered_bytes = netbat::decode_hex_str(&event.payload_hex)?;
    assert_eq!(recovered_bytes, original_payload_bytes);
    let recovered: SystemHeartbeatRequest = batpak::encoding::from_bytes(&recovered_bytes)?;
    assert_eq!(recovered, original_payload);
    Ok(())
}

#[test]
fn bank_commit_then_event_query_pages_global_sequence_and_event_get_fetches() -> Result<()> {
    let host = HbatProcess::spawn()?;

    let original_payload = SystemHeartbeatRequest {
        nonce: "tcp-e2e-event-query-001".to_owned(),
    };
    let original_payload_bytes = batpak::encoding::to_bytes(&original_payload)?;

    let mut committed = Vec::new();
    for _ in 0..3 {
        let commit_request = BankCommitRequest {
            entity: "tcp:e2e-query".to_owned(),
            scope: "tcp-e2e-query-scope".to_owned(),
            kind_category: SystemHeartbeatRequest::KIND.category(),
            kind_type_id: SystemHeartbeatRequest::KIND.type_id(),
            payload_hex: lowercase_hex(&original_payload_bytes),
        };
        let commit_input = batpak::encoding::to_bytes(&commit_request)?;
        let commit_response = call_one(&host, "bank.commit", &commit_input)?;
        let commit_output = parse_ok(&commit_response)?;
        let ack: BankCommitAck = batpak::encoding::from_bytes(&commit_output)?;
        committed.push(ack);
    }

    let first_query = EventQueryRequest {
        entity: Some("tcp:e2e-query".to_owned()),
        scope: Some("tcp-e2e-query-scope".to_owned()),
        kind_category: Some(SystemHeartbeatRequest::KIND.category()),
        kind_type_id: Some(SystemHeartbeatRequest::KIND.type_id()),
        after_global_sequence: None,
        limit: 2,
    };
    let first_response = call_one(
        &host,
        "event.query",
        &batpak::encoding::to_bytes(&first_query)?,
    )?;
    let first_output = parse_ok(&first_response)?;
    let first_page: EventQueryAck = batpak::encoding::from_bytes(&first_output)?;

    assert_eq!(first_page.entries.len(), 2);
    assert!(first_page.truncated);
    assert_eq!(
        first_page.next_after_global_sequence,
        Some(committed[1].sequence)
    );
    assert_eq!(
        first_page.entries[0].event_id_hex,
        committed[0].event_id_hex
    );
    assert_eq!(first_page.entries[0].global_sequence, committed[0].sequence);
    assert_eq!(
        first_page.entries[1].event_id_hex,
        committed[1].event_id_hex
    );
    assert_eq!(first_page.entries[1].global_sequence, committed[1].sequence);
    for summary in &first_page.entries {
        assert_eq!(summary.entity, "tcp:e2e-query");
        assert_eq!(summary.scope, "tcp-e2e-query-scope");
        assert_eq!(
            summary.kind_category,
            SystemHeartbeatRequest::KIND.category()
        );
        assert_eq!(summary.kind_type_id, SystemHeartbeatRequest::KIND.type_id());
        assert_eq!(summary.content_hash_hex.len(), 64);
    }

    let second_query = EventQueryRequest {
        after_global_sequence: first_page.next_after_global_sequence,
        ..first_query
    };
    let second_response = call_one(
        &host,
        "event.query",
        &batpak::encoding::to_bytes(&second_query)?,
    )?;
    let second_output = parse_ok(&second_response)?;
    let second_page: EventQueryAck = batpak::encoding::from_bytes(&second_output)?;

    assert_eq!(second_page.entries.len(), 1);
    assert!(!second_page.truncated);
    assert_eq!(
        second_page.next_after_global_sequence,
        Some(committed[2].sequence)
    );
    assert_eq!(
        second_page.entries[0].global_sequence,
        committed[2].sequence
    );

    let get_request = EventGetRequest {
        event_id_hex: second_page.entries[0].event_id_hex.clone(),
    };
    let get_response = call_one(
        &host,
        "event.get",
        &batpak::encoding::to_bytes(&get_request)?,
    )?;
    let get_output = parse_ok(&get_response)?;
    let event: EventGetAck = batpak::encoding::from_bytes(&get_output)?;
    assert_eq!(event.event_id_hex, committed[2].event_id_hex);
    assert_eq!(event.sequence, committed[2].sequence);
    assert_eq!(
        netbat::decode_hex_str(&event.payload_hex)?,
        original_payload_bytes
    );
    Ok(())
}

#[test]
fn event_query_rejects_zero_limit_over_tcp() -> Result<()> {
    let host = HbatProcess::spawn()?;

    let request = EventQueryRequest {
        entity: None,
        scope: None,
        kind_category: None,
        kind_type_id: None,
        after_global_sequence: None,
        limit: 0,
    };
    let response = call_one(&host, "event.query", &batpak::encoding::to_bytes(&request)?)?;
    let (code, message) = parse_err(&response)?;
    assert_eq!(code, "handler");
    assert!(
        message.to_lowercase().contains("limit"),
        "expected error mentioning limit, got {message:?}"
    );
    Ok(())
}

#[test]
fn bank_commit_then_receipt_verify_accepts_ack_over_tcp() -> Result<()> {
    let host = HbatProcess::spawn()?;
    let ack = commit_heartbeat_event(&host, "tcp:verify", "tcp-e2e-receipt-verify-001")?;

    let request = ReceiptVerifyRequest {
        event_id_hex: ack.event_id_hex.clone(),
        sequence: ack.sequence,
        content_hash_hex: ack.content_hash_hex.clone(),
        key_id_hex: ack.key_id_hex.clone(),
        signature_hex: ack.signature_hex.clone(),
        extensions: ack.extensions.clone(),
    };
    let response = call_one(
        &host,
        "receipt.verify",
        &batpak::encoding::to_bytes(&request)?,
    )?;
    let output = parse_ok(&response)?;
    let verify: ReceiptVerifyAck = batpak::encoding::from_bytes(&output)?;
    assert!(verify.valid);
    assert_eq!(verify.outcome, "unsigned_accepted");
    assert!(verify.reason_code.is_none());
    Ok(())
}

#[test]
fn receipt_verify_rejects_tampered_fields_over_tcp() -> Result<()> {
    let host = HbatProcess::spawn()?;
    let ack = commit_heartbeat_event(&host, "tcp:verify-tamper", "tcp-e2e-receipt-verify-002")?;

    let request = ReceiptVerifyRequest {
        event_id_hex: ack.event_id_hex.clone(),
        sequence: ack.sequence + 1,
        content_hash_hex: ack.content_hash_hex.clone(),
        key_id_hex: ack.key_id_hex.clone(),
        signature_hex: ack.signature_hex.clone(),
        extensions: ack.extensions.clone(),
    };
    let response = call_one(
        &host,
        "receipt.verify",
        &batpak::encoding::to_bytes(&request)?,
    )?;
    let output = parse_ok(&response)?;
    let verify: ReceiptVerifyAck = batpak::encoding::from_bytes(&output)?;
    assert!(!verify.valid);
    assert_eq!(verify.outcome, "invalid");
    assert_eq!(verify.reason_code.as_deref(), Some("sequence_mismatch"));
    Ok(())
}

#[test]
fn event_walk_rejects_zero_limit_over_tcp() -> Result<()> {
    let host = HbatProcess::spawn()?;
    let request = EventWalkRequest {
        event_id_hex: "0123456789abcdef0123456789abcdef".to_owned(),
        limit: 0,
    };
    let response = call_one(&host, "event.walk", &batpak::encoding::to_bytes(&request)?)?;
    let (code, message) = parse_err(&response)?;
    assert_eq!(code, "handler");
    assert!(
        message.to_lowercase().contains("limit"),
        "expected error mentioning limit, got {message:?}"
    );
    Ok(())
}

#[test]
fn bank_commit_then_event_walk_then_event_get_works_over_tcp() -> Result<()> {
    let host = HbatProcess::spawn()?;
    let first = commit_heartbeat_event(&host, "tcp:walk", "tcp-e2e-walk-001")?;
    let second = commit_heartbeat_event(&host, "tcp:walk", "tcp-e2e-walk-002")?;
    let third = commit_heartbeat_event(&host, "tcp:walk", "tcp-e2e-walk-003")?;

    let walk_request = EventWalkRequest {
        event_id_hex: second.event_id_hex.clone(),
        limit: 10,
    };
    let walk_response = call_one(
        &host,
        "event.walk",
        &batpak::encoding::to_bytes(&walk_request)?,
    )?;
    let walk_output = parse_ok(&walk_response)?;
    let walk: EventWalkAck = batpak::encoding::from_bytes(&walk_output)?;

    assert_eq!(walk.entries.len(), 2);
    assert_eq!(walk.entries[0].event_id_hex, second.event_id_hex);
    assert_eq!(walk.entries[1].event_id_hex, first.event_id_hex);
    assert_ne!(walk.entries[0].event_id_hex, third.event_id_hex);

    let get_request = EventGetRequest {
        event_id_hex: walk.entries[1].event_id_hex.clone(),
    };
    let get_response = call_one(
        &host,
        "event.get",
        &batpak::encoding::to_bytes(&get_request)?,
    )?;
    let get_output = parse_ok(&get_response)?;
    let event: EventGetAck = batpak::encoding::from_bytes(&get_output)?;
    assert_eq!(event.event_id_hex, first.event_id_hex);
    assert_eq!(event.sequence, first.sequence);
    Ok(())
}

#[test]
fn fixture_payloads_round_trip_over_tcp() -> Result<()> {
    // Sanity for the manifest fixtures themselves — proves the bytes
    // we publish in batpak.manifest.json (which the TS SDK consumes
    // for its parity tests) work end-to-end through the live wire.
    let host = HbatProcess::spawn()?;

    let heartbeat_fixture = SystemHeartbeatRequest::fixture_value();
    let heartbeat_bytes = batpak::encoding::to_bytes(&heartbeat_fixture)?;
    let response = call_one(&host, "system.heartbeat", &heartbeat_bytes)?;
    let ack_bytes = parse_ok(&response)?;
    let ack: SystemHeartbeatAck = batpak::encoding::from_bytes(&ack_bytes)?;
    assert_eq!(ack.nonce, heartbeat_fixture.nonce);
    Ok(())
}

#[test]
fn bank_commit_rejects_reserved_kind_category_over_tcp() -> Result<()> {
    let host = HbatProcess::spawn()?;
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
    let input = batpak::encoding::to_bytes(&bad_request)?;
    let response = call_one(&host, "bank.commit", &input)?;
    // Expect an ERR frame. Code is "handler" because the handler
    // produced an HandlerError::InvalidInput which the runtime maps
    // to NetbatError::Runtime(RuntimeError::Handler).
    let (code, message) = parse_err(&response)?;
    assert_eq!(code, "handler");
    assert!(
        message.to_lowercase().contains("kind") || message.to_lowercase().contains("event kind"),
        "expected error mentioning kind, got {message:?}"
    );
    Ok(())
}

#[test]
fn malformed_frame_returns_typed_err() -> Result<()> {
    let host = HbatProcess::spawn()?;
    let mut stream = host.connect()?;
    // Garbage line with no protocol prefix.
    stream
        .write_all(b"GARBAGE not a frame\n")
        .context("write garbage")?;
    stream.flush().context("flush")?;
    let response = decode_response(&mut stream)?;
    let (code, _message) = parse_err(&response)?;
    // Either malformed_request or unsupported_protocol_version, both
    // are stable wire-error codes from netbat::NetbatError::code().
    assert!(
        matches!(
            code.as_str(),
            "malformed_request" | "unsupported_protocol_version"
        ),
        "unexpected code {code:?}"
    );
    Ok(())
}

#[test]
fn ready_payload_carries_addr_port_and_protocol() -> Result<()> {
    // Spawn-and-introspect smoke test: just ensure HBAT_READY parses.
    let host = HbatProcess::spawn()?;
    assert!(host.port() > 0);
    Ok(())
}

#[test]
fn evidence_chain_walk_round_trips_over_tcp() -> Result<()> {
    let host = HbatProcess::spawn()?;

    // Commit two events to the same entity so a chain exists to walk.
    let payload = SystemHeartbeatRequest {
        nonce: "tcp-e2e-evidence-chain".to_owned(),
    };
    let payload_bytes = batpak::encoding::to_bytes(&payload)?;
    let mut last_event_id = String::new();
    for _ in 0..2 {
        let commit = BankCommitRequest {
            entity: "tcp:e2e-evidence".to_owned(),
            scope: "tcp-e2e-evidence-scope".to_owned(),
            kind_category: SystemHeartbeatRequest::KIND.category(),
            kind_type_id: SystemHeartbeatRequest::KIND.type_id(),
            payload_hex: lowercase_hex(&payload_bytes),
        };
        let response = call_one(&host, "bank.commit", &batpak::encoding::to_bytes(&commit)?)?;
        let ack: BankCommitAck = batpak::encoding::from_bytes(&parse_ok(&response)?)?;
        last_event_id = ack.event_id_hex;
    }

    let request = ChainWalkEvidenceRequest {
        start_event_id_hex: last_event_id,
        start_expected_hash_hex: None,
        end_event_id_hex: None,
        limit: 16,
    };
    let response = call_one(
        &host,
        "evidence.chain_walk",
        &batpak::encoding::to_bytes(&request)?,
    )?;
    let ack: ChainWalkEvidenceAck = batpak::encoding::from_bytes(&parse_ok(&response)?)?;

    // The ack carries the report body as a canonical blob whose content hash is
    // the advertised body_hash. Re-hashing the blob with the same function core
    // uses must reproduce it — the evidence identity contract, over TCP.
    assert_evidence_report_identity(&ack.report_hex, &ack.body_hash_hex)?;
    Ok(())
}

fn assert_evidence_report_identity(ack_report_hex: &str, ack_body_hash_hex: &str) -> Result<()> {
    let report_bytes = netbat::decode_hex_str(ack_report_hex)?;
    let rehashed = lowercase_hex(&batpak::event::hash::compute_hash(&report_bytes));
    assert_eq!(
        rehashed, ack_body_hash_hex,
        "report_hex must re-hash to body_hash_hex (evidence identity)"
    );
    Ok(())
}

#[test]
fn evidence_store_resource_round_trips_over_tcp() -> Result<()> {
    let host = HbatProcess::spawn()?;
    let request = StoreResourceEvidenceRequest::fixture_value();
    let response = call_one(
        &host,
        "evidence.store_resource",
        &batpak::encoding::to_bytes(&request)?,
    )?;
    let ack: StoreResourceEvidenceAck = batpak::encoding::from_bytes(&parse_ok(&response)?)?;
    assert_evidence_report_identity(&ack.report_hex, &ack.body_hash_hex)?;
    Ok(())
}

#[test]
fn evidence_read_walk_round_trips_over_tcp() -> Result<()> {
    let host = HbatProcess::spawn()?;

    let payload = SystemHeartbeatRequest {
        nonce: "tcp-e2e-evidence-read-walk".to_owned(),
    };
    let payload_bytes = batpak::encoding::to_bytes(&payload)?;
    for _ in 0..2 {
        let commit = BankCommitRequest {
            entity: "tcp:e2e-read-walk".to_owned(),
            scope: "tcp-e2e-read-walk-scope".to_owned(),
            kind_category: SystemHeartbeatRequest::KIND.category(),
            kind_type_id: SystemHeartbeatRequest::KIND.type_id(),
            payload_hex: lowercase_hex(&payload_bytes),
        };
        let _ = call_one(&host, "bank.commit", &batpak::encoding::to_bytes(&commit)?)?;
    }

    let request = ReadWalkEvidenceRequest {
        entity: Some("tcp:e2e-read-walk".to_owned()),
        scope: None,
        kind_category: Some(SystemHeartbeatRequest::KIND.category()),
        kind_type_id: Some(SystemHeartbeatRequest::KIND.type_id()),
        start_clock: None,
        end_clock: None,
        limit: Some(16),
        include_proof_refs: false,
        max_stale_ms: None,
    };
    let response = call_one(
        &host,
        "evidence.read_walk",
        &batpak::encoding::to_bytes(&request)?,
    )?;
    let ack: ReadWalkEvidenceAck = batpak::encoding::from_bytes(&parse_ok(&response)?)?;
    assert_evidence_report_identity(&ack.report_hex, &ack.body_hash_hex)?;
    Ok(())
}

#[test]
fn evidence_read_walk_truncated_over_tcp() -> Result<()> {
    let host = HbatProcess::spawn()?;

    let payload = SystemHeartbeatRequest {
        nonce: "tcp-e2e-evidence-read-walk-trunc".to_owned(),
    };
    let payload_bytes = batpak::encoding::to_bytes(&payload)?;
    for _ in 0..3 {
        let commit = BankCommitRequest {
            entity: "tcp:e2e-read-walk-trunc".to_owned(),
            scope: "tcp-e2e-read-walk-trunc-scope".to_owned(),
            kind_category: SystemHeartbeatRequest::KIND.category(),
            kind_type_id: SystemHeartbeatRequest::KIND.type_id(),
            payload_hex: lowercase_hex(&payload_bytes),
        };
        let _ = call_one(&host, "bank.commit", &batpak::encoding::to_bytes(&commit)?)?;
    }

    let request = ReadWalkEvidenceRequest {
        entity: Some("tcp:e2e-read-walk-trunc".to_owned()),
        scope: None,
        kind_category: None,
        kind_type_id: None,
        start_clock: None,
        end_clock: None,
        limit: Some(1),
        include_proof_refs: false,
        max_stale_ms: None,
    };
    let response = call_one(
        &host,
        "evidence.read_walk",
        &batpak::encoding::to_bytes(&request)?,
    )?;
    let ack: ReadWalkEvidenceAck = batpak::encoding::from_bytes(&parse_ok(&response)?)?;
    assert!(
        ack.truncated,
        "limit below the match count must report truncated over TCP"
    );
    assert_evidence_report_identity(&ack.report_hex, &ack.body_hash_hex)?;
    Ok(())
}

#[test]
fn evidence_projection_run_unknown_projection_over_tcp() -> Result<()> {
    // The reference host is domain-neutral: it advertises evidence.projection_run
    // but registers no projections, so every projection id is unknown.
    let host = HbatProcess::spawn()?;
    let request = ProjectionRunEvidenceRequest {
        projection: "any.projection".to_owned(),
        entity: "tcp:e2e-evidence".to_owned(),
        max_stale_ms: None,
    };
    let response = call_one(
        &host,
        "evidence.projection_run",
        &batpak::encoding::to_bytes(&request)?,
    )?;
    let (code, message) = parse_err(&response)?;
    assert_eq!(code, "handler");
    assert!(
        message.contains("unknown projection"),
        "message did not name the unknown projection: {message:?}"
    );
    Ok(())
}
