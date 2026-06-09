//! Handler integration tests extracted from `handlers.rs` to keep the
//! production module under the structural inline-test budget.

use std::sync::Arc;

use anyhow::{bail, Result};
use batpak::event::{Event, EventKind, JsonValueInput};
use batpak::store::{
    ChainWalkReportBody, ProjectionEvidenceRegistry, ProjectionRunReportBody, ReadWalkReportBody,
    Store, StoreConfig, StoreResourceReportBody,
};
use batpak::{EventPayload, EventSourced};
use hbat::bank::{
    BankCommitAck, BankCommitRequest, EventGetAck, EventGetRequest, BANK_COMMIT_DESCRIPTOR,
    EVENT_GET_DESCRIPTOR, EVENT_QUERY_DESCRIPTOR,
};
use hbat::evidence::{
    ChainWalkEvidenceAck, ChainWalkEvidenceRequest, ProjectionRunEvidenceAck,
    ProjectionRunEvidenceRequest, ReadWalkEvidenceAck, ReadWalkEvidenceRequest,
    StoreResourceEvidenceAck, StoreResourceEvidenceRequest, EVIDENCE_CHAIN_WALK_DESCRIPTOR,
    EVIDENCE_PROJECTION_RUN_DESCRIPTOR, EVIDENCE_READ_WALK_DESCRIPTOR,
    EVIDENCE_STORE_RESOURCE_DESCRIPTOR,
};
use hbat::handlers::{
    BankCommitHandler, ChainWalkEvidenceHandler, EventGetHandler, EventQueryHandler,
    EventWalkHandler, ProjectionRunEvidenceHandler, ReadWalkEvidenceHandler, ReceiptVerifyHandler,
    StoreResourceEvidenceHandler,
};
use hbat::heartbeat::SystemHeartbeatRequest;
use hbat::receipt::{ReceiptVerifyAck, ReceiptVerifyRequest, RECEIPT_VERIFY_DESCRIPTOR};
use hbat::walk::{EventWalkAck, EventWalkRequest, EVENT_WALK_DESCRIPTOR};
use hbat::EventPayloadFixture;
use netbat::{decode_hex_str, encode_hex_str};

/// Minimal fixture projection used to exercise the `evidence.projection_run`
/// registry dispatch path end to end.
#[derive(Default, Debug, serde::Serialize, serde::Deserialize)]
struct TestCounter {
    count: u64,
}

impl EventSourced for TestCounter {
    type Input = JsonValueInput;

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        (!events.is_empty()).then_some(Self {
            count: events.len() as u64,
        })
    }

    fn apply_event(&mut self, event: &Event<serde_json::Value>) {
        std::hint::black_box(event.event_kind());
        self.count += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        static KINDS: [EventKind; 1] = [EventKind::custom(0xF, 1)];
        &KINDS
    }
}

/// Stable id the test registry registers [`TestCounter`] under.
const TEST_PROJECTION_ID: &str = "test.counter";

fn fresh_store() -> Result<(Arc<Store>, tempfile::TempDir)> {
    let dir = tempfile::TempDir::new()?;
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )?;
    Ok((Arc::new(store), dir))
}

fn fresh_core(store: &Arc<Store>) -> Result<syncbat::Core> {
    let mut builder = syncbat::Core::builder();
    builder.register(
        BANK_COMMIT_DESCRIPTOR.clone(),
        BankCommitHandler {
            store: Arc::clone(store),
        },
    )?;
    builder.register(
        EVENT_GET_DESCRIPTOR.clone(),
        EventGetHandler {
            store: Arc::clone(store),
        },
    )?;
    builder.register(
        EVENT_QUERY_DESCRIPTOR.clone(),
        EventQueryHandler {
            store: Arc::clone(store),
        },
    )?;
    builder.register(
        RECEIPT_VERIFY_DESCRIPTOR.clone(),
        ReceiptVerifyHandler {
            store: Arc::clone(store),
        },
    )?;
    builder.register(
        EVENT_WALK_DESCRIPTOR.clone(),
        EventWalkHandler {
            store: Arc::clone(store),
        },
    )?;
    builder.register(
        EVIDENCE_CHAIN_WALK_DESCRIPTOR.clone(),
        ChainWalkEvidenceHandler {
            store: Arc::clone(store),
        },
    )?;
    builder.register(
        EVIDENCE_STORE_RESOURCE_DESCRIPTOR.clone(),
        StoreResourceEvidenceHandler {
            store: Arc::clone(store),
        },
    )?;
    builder.register(
        EVIDENCE_READ_WALK_DESCRIPTOR.clone(),
        ReadWalkEvidenceHandler {
            store: Arc::clone(store),
        },
    )?;
    let mut registry = ProjectionEvidenceRegistry::new();
    registry.register::<TestCounter>(TEST_PROJECTION_ID);
    builder.register(
        EVIDENCE_PROJECTION_RUN_DESCRIPTOR.clone(),
        ProjectionRunEvidenceHandler {
            store: Arc::clone(store),
            registry: Arc::new(registry),
        },
    )?;
    Ok(builder.build()?)
}

fn commit_heartbeat(core: &mut syncbat::Core, entity: &str) -> Result<BankCommitAck> {
    let heartbeat = SystemHeartbeatRequest::fixture_value();
    let heartbeat_bytes = batpak::encoding::to_bytes(&heartbeat)?;
    let request = BankCommitRequest {
        entity: entity.to_owned(),
        scope: "test-scope".to_owned(),
        kind_category: SystemHeartbeatRequest::KIND.category(),
        kind_type_id: SystemHeartbeatRequest::KIND.type_id(),
        payload_hex: encode_hex_str(&heartbeat_bytes),
    };
    let request_bytes = batpak::encoding::to_bytes(&request)?;
    let result = core.invoke("bank.commit", request_bytes)?;
    Ok(batpak::encoding::from_bytes(result.output())?)
}

fn invoke_expect_err(core: &mut syncbat::Core, op: &str, input: Vec<u8>) -> Result<String> {
    match core.invoke(op, input) {
        Ok(_) => bail!("{op} must fail but returned Ok"),
        Err(err) => Ok(err.to_string()),
    }
}

#[test]
fn bank_commit_appends_a_heartbeat_request_event() -> Result<()> {
    let (store, _dir) = fresh_store()?;
    let mut core = fresh_core(&store)?;

    let heartbeat = SystemHeartbeatRequest::fixture_value();
    let heartbeat_bytes = batpak::encoding::to_bytes(&heartbeat)?;

    let request = BankCommitRequest {
        entity: "test:bank".to_owned(),
        scope: "test-scope".to_owned(),
        kind_category: SystemHeartbeatRequest::KIND.category(),
        kind_type_id: SystemHeartbeatRequest::KIND.type_id(),
        payload_hex: encode_hex_str(&heartbeat_bytes),
    };
    let request_bytes = batpak::encoding::to_bytes(&request)?;

    let result = core.invoke("bank.commit", request_bytes)?;

    let ack: BankCommitAck = batpak::encoding::from_bytes(result.output())?;
    assert_eq!(ack.event_id_hex.len(), 32);
    assert_eq!(ack.content_hash_hex.len(), 64);
    assert_eq!(ack.key_id_hex.len(), 64);
    assert!(ack.sequence >= 1);
    Ok(())
}

#[test]
fn event_get_returns_what_bank_commit_wrote() -> Result<()> {
    let (store, _dir) = fresh_store()?;
    let mut core = fresh_core(&store)?;

    let heartbeat = SystemHeartbeatRequest::fixture_value();
    let heartbeat_bytes = batpak::encoding::to_bytes(&heartbeat)?;
    let request = BankCommitRequest {
        entity: "test:bank".to_owned(),
        scope: "test-scope".to_owned(),
        kind_category: SystemHeartbeatRequest::KIND.category(),
        kind_type_id: SystemHeartbeatRequest::KIND.type_id(),
        payload_hex: encode_hex_str(&heartbeat_bytes),
    };
    let request_bytes = batpak::encoding::to_bytes(&request)?;
    let commit_result = core.invoke("bank.commit", request_bytes)?;
    let ack: BankCommitAck = batpak::encoding::from_bytes(commit_result.output())?;

    let get_request = EventGetRequest {
        event_id_hex: ack.event_id_hex.clone(),
    };
    let get_bytes = batpak::encoding::to_bytes(&get_request)?;
    let get_result = core.invoke("event.get", get_bytes)?;
    let event: EventGetAck = batpak::encoding::from_bytes(get_result.output())?;

    assert_eq!(event.event_id_hex, ack.event_id_hex);
    assert_eq!(event.entity, "test:bank");
    assert_eq!(event.scope, "test-scope");
    assert_eq!(event.kind_category, SystemHeartbeatRequest::KIND.category());
    assert_eq!(event.kind_type_id, SystemHeartbeatRequest::KIND.type_id());

    let payload_bytes = decode_hex_str(&event.payload_hex)?;
    let decoded: SystemHeartbeatRequest = batpak::encoding::from_bytes(&payload_bytes)?;
    assert_eq!(decoded, heartbeat);
    Ok(())
}

#[test]
fn bank_commit_rejects_reserved_kind_category() -> Result<()> {
    let (store, _dir) = fresh_store()?;
    let mut core = fresh_core(&store)?;
    let request = BankCommitRequest {
        entity: "test:bank".to_owned(),
        scope: "test-scope".to_owned(),
        kind_category: 0x0,
        kind_type_id: 0xA01,
        payload_hex: "81a0".to_owned(),
    };
    let request_bytes = batpak::encoding::to_bytes(&request)?;
    let msg = invoke_expect_err(&mut core, "bank.commit", request_bytes)?;
    assert!(
        msg.contains("kind") || msg.contains("invalid_input"),
        "expected error mentioning invalid kind, got {msg:?}"
    );
    Ok(())
}

#[test]
fn bank_commit_rejects_invalid_coordinate() -> Result<()> {
    let (store, _dir) = fresh_store()?;
    let mut core = fresh_core(&store)?;
    let request = BankCommitRequest {
        entity: "".to_owned(),
        scope: "test-scope".to_owned(),
        kind_category: 0xF,
        kind_type_id: 0xA01,
        payload_hex: "81a0".to_owned(),
    };
    let request_bytes = batpak::encoding::to_bytes(&request)?;
    let msg = invoke_expect_err(&mut core, "bank.commit", request_bytes)?;
    assert!(
        msg.contains("coordinate") || msg.contains("invalid_input"),
        "expected error mentioning coordinate, got {msg:?}"
    );
    Ok(())
}

#[test]
fn event_get_returns_failed_for_unknown_id() -> Result<()> {
    let (store, _dir) = fresh_store()?;
    let mut core = fresh_core(&store)?;
    let request = EventGetRequest {
        event_id_hex: "deadbeefdeadbeefdeadbeefdeadbeef".to_owned(),
    };
    let request_bytes = batpak::encoding::to_bytes(&request)?;
    let _ = invoke_expect_err(&mut core, "event.get", request_bytes)?;
    Ok(())
}

#[test]
fn event_get_rejects_malformed_event_id_hex() -> Result<()> {
    let (store, _dir) = fresh_store()?;
    let mut core = fresh_core(&store)?;
    let request = EventGetRequest {
        event_id_hex: "not-hex".to_owned(),
    };
    let request_bytes = batpak::encoding::to_bytes(&request)?;
    let _ = invoke_expect_err(&mut core, "event.get", request_bytes)?;
    Ok(())
}

#[test]
fn receipt_verify_accepts_fresh_bank_commit_ack() -> Result<()> {
    let (store, _dir) = fresh_store()?;
    let mut core = fresh_core(&store)?;
    let ack = commit_heartbeat(&mut core, "test:verify")?;

    let request = ReceiptVerifyRequest {
        event_id_hex: ack.event_id_hex.clone(),
        sequence: ack.sequence,
        content_hash_hex: ack.content_hash_hex.clone(),
        key_id_hex: ack.key_id_hex.clone(),
        signature_hex: ack.signature_hex.clone(),
        extensions: ack.extensions.clone(),
    };
    let request_bytes = batpak::encoding::to_bytes(&request)?;
    let result = core.invoke("receipt.verify", request_bytes)?;
    let verify: ReceiptVerifyAck = batpak::encoding::from_bytes(result.output())?;
    assert!(verify.valid);
    assert_eq!(verify.outcome, "unsigned_accepted");
    assert!(verify.reason_code.is_none());
    Ok(())
}

#[test]
fn receipt_verify_rejects_tampered_sequence() -> Result<()> {
    let (store, _dir) = fresh_store()?;
    let mut core = fresh_core(&store)?;
    let ack = commit_heartbeat(&mut core, "test:verify-tamper")?;

    let request = ReceiptVerifyRequest {
        event_id_hex: ack.event_id_hex.clone(),
        sequence: ack.sequence + 1,
        content_hash_hex: ack.content_hash_hex.clone(),
        key_id_hex: ack.key_id_hex.clone(),
        signature_hex: ack.signature_hex.clone(),
        extensions: ack.extensions.clone(),
    };
    let request_bytes = batpak::encoding::to_bytes(&request)?;
    let result = core.invoke("receipt.verify", request_bytes)?;
    let verify: ReceiptVerifyAck = batpak::encoding::from_bytes(result.output())?;
    assert!(!verify.valid);
    assert_eq!(verify.outcome, "invalid");
    assert_eq!(verify.reason_code.as_deref(), Some("sequence_mismatch"));
    Ok(())
}

#[test]
fn event_walk_rejects_zero_limit() -> Result<()> {
    let (store, _dir) = fresh_store()?;
    let mut core = fresh_core(&store)?;
    let request = EventWalkRequest {
        event_id_hex: "0123456789abcdef0123456789abcdef".to_owned(),
        limit: 0,
    };
    let request_bytes = batpak::encoding::to_bytes(&request)?;
    let _ = invoke_expect_err(&mut core, "event.walk", request_bytes)?;
    Ok(())
}

#[test]
fn event_walk_returns_bounded_ancestry_in_order() -> Result<()> {
    let (store, _dir) = fresh_store()?;
    let mut core = fresh_core(&store)?;
    let first = commit_heartbeat(&mut core, "test:walk")?;
    let second = commit_heartbeat(&mut core, "test:walk")?;
    let third = commit_heartbeat(&mut core, "test:walk")?;

    let request = EventWalkRequest {
        event_id_hex: second.event_id_hex.clone(),
        limit: 10,
    };
    let request_bytes = batpak::encoding::to_bytes(&request)?;
    let result = core.invoke("event.walk", request_bytes)?;
    let walk: EventWalkAck = batpak::encoding::from_bytes(result.output())?;

    assert_eq!(walk.entries.len(), 2);
    assert_eq!(walk.entries[0].event_id_hex, second.event_id_hex);
    assert_eq!(walk.entries[1].event_id_hex, first.event_id_hex);
    assert_ne!(walk.entries[0].event_id_hex, third.event_id_hex);
    assert!(
        walk.entries[0].global_sequence > walk.entries[1].global_sequence,
        "anchor-first walk order is relation order, not ascending global_sequence"
    );
    Ok(())
}

// ─── evidence.* ops ───────────────────────────────────────────────────────────

#[test]
fn evidence_chain_walk_ack_carries_report_body_and_matching_identity() -> Result<()> {
    let (store, _dir) = fresh_store()?;
    let mut core = fresh_core(&store)?;
    let _first = commit_heartbeat(&mut core, "test:evidence-chain")?;
    let second = commit_heartbeat(&mut core, "test:evidence-chain")?;

    let request = ChainWalkEvidenceRequest {
        start_event_id_hex: second.event_id_hex.clone(),
        start_expected_hash_hex: None,
        end_event_id_hex: None,
        limit: 16,
    };
    let result = core.invoke("evidence.chain_walk", batpak::encoding::to_bytes(&request)?)?;
    let ack: ChainWalkEvidenceAck = batpak::encoding::from_bytes(result.output())?;

    // report_hex decodes to the real report body, and body_hash_hex is that
    // body's identity hash — byte-for-byte equal to a direct typed call.
    let wire_body: ChainWalkReportBody =
        batpak::encoding::from_bytes(&decode_hex_str(&ack.report_hex)?)?;
    let direct = store.chain_walk_evidence(&request.to_core()?)?;
    assert_eq!(wire_body, direct.body);
    assert_eq!(ack.body_hash_hex, encode_hex_str(&direct.body_hash));
    Ok(())
}

#[test]
fn evidence_store_resource_ack_carries_snapshot() -> Result<()> {
    let (store, _dir) = fresh_store()?;
    let mut core = fresh_core(&store)?;
    let _ = commit_heartbeat(&mut core, "test:evidence-store")?;

    let request = StoreResourceEvidenceRequest {};
    let result = core.invoke(
        "evidence.store_resource",
        batpak::encoding::to_bytes(&request)?,
    )?;
    let ack: StoreResourceEvidenceAck = batpak::encoding::from_bytes(result.output())?;

    let wire_body: StoreResourceReportBody =
        batpak::encoding::from_bytes(&decode_hex_str(&ack.report_hex)?)?;
    let direct = store.store_resource_evidence_report()?;
    assert_eq!(wire_body, direct.body);
    assert_eq!(ack.body_hash_hex, encode_hex_str(&direct.body_hash));
    assert!(!ack.truncated);
    Ok(())
}

#[test]
fn evidence_read_walk_ack_carries_report_body() -> Result<()> {
    let (store, _dir) = fresh_store()?;
    let mut core = fresh_core(&store)?;
    let _ = commit_heartbeat(&mut core, "test:evidence-read")?;

    let request = ReadWalkEvidenceRequest {
        entity: Some("test:evidence-read".to_owned()),
        scope: None,
        limit: Some(32),
        include_proof_refs: false,
    };
    let result = core.invoke("evidence.read_walk", batpak::encoding::to_bytes(&request)?)?;
    let ack: ReadWalkEvidenceAck = batpak::encoding::from_bytes(result.output())?;

    let wire_body: ReadWalkReportBody =
        batpak::encoding::from_bytes(&decode_hex_str(&ack.report_hex)?)?;
    let (_entries, direct) = store.query_with_read_walk_evidence(&request.to_core()?)?;
    assert_eq!(wire_body, direct.body);
    assert_eq!(ack.body_hash_hex, encode_hex_str(&direct.body_hash));
    Ok(())
}

#[test]
fn evidence_projection_run_dispatches_registered_projection() -> Result<()> {
    let (store, _dir) = fresh_store()?;
    let mut core = fresh_core(&store)?;
    let _ = commit_heartbeat(&mut core, "test:evidence-proj")?;

    let request = ProjectionRunEvidenceRequest {
        projection: TEST_PROJECTION_ID.to_owned(),
        entity: "test:evidence-proj".to_owned(),
        max_stale_ms: None,
    };
    let result = core.invoke(
        "evidence.projection_run",
        batpak::encoding::to_bytes(&request)?,
    )?;
    let ack: ProjectionRunEvidenceAck = batpak::encoding::from_bytes(result.output())?;

    let wire_body: ProjectionRunReportBody =
        batpak::encoding::from_bytes(&decode_hex_str(&ack.report_hex)?)?;
    // The report identifies the registered projection by its substrate id.
    assert!(wire_body.projection_id.contains("test:evidence-proj"));
    assert_ne!(ack.body_hash_hex, "0".repeat(64));
    Ok(())
}

#[test]
fn evidence_projection_run_unknown_projection_errors() -> Result<()> {
    let (store, _dir) = fresh_store()?;
    let mut core = fresh_core(&store)?;

    let request = ProjectionRunEvidenceRequest {
        projection: "nope.not.registered".to_owned(),
        entity: "test:evidence-proj".to_owned(),
        max_stale_ms: None,
    };
    let message = invoke_expect_err(
        &mut core,
        "evidence.projection_run",
        batpak::encoding::to_bytes(&request)?,
    )?;
    assert!(
        message.contains("unknown projection"),
        "expected unknown-projection error, got: {message}"
    );
    Ok(())
}

#[test]
fn evidence_read_walk_truncated_reflects_limit_drops() -> Result<()> {
    let (store, _dir) = fresh_store()?;
    let mut core = fresh_core(&store)?;
    for _ in 0..3 {
        let _ = commit_heartbeat(&mut core, "test:rw-trunc")?;
    }

    // A limit below the match count drops entries by limit -> truncated.
    let limited = ReadWalkEvidenceRequest {
        entity: Some("test:rw-trunc".to_owned()),
        scope: None,
        limit: Some(2),
        include_proof_refs: false,
    };
    let result = core.invoke("evidence.read_walk", batpak::encoding::to_bytes(&limited)?)?;
    let ack: ReadWalkEvidenceAck = batpak::encoding::from_bytes(result.output())?;
    assert!(
        ack.truncated,
        "limit below the match count must report truncated"
    );

    // A limit above the match count drops nothing -> not truncated.
    let full = ReadWalkEvidenceRequest {
        entity: Some("test:rw-trunc".to_owned()),
        scope: None,
        limit: Some(10),
        include_proof_refs: false,
    };
    let result = core.invoke("evidence.read_walk", batpak::encoding::to_bytes(&full)?)?;
    let ack: ReadWalkEvidenceAck = batpak::encoding::from_bytes(result.output())?;
    assert!(
        !ack.truncated,
        "limit above the match count must not report truncated"
    );
    Ok(())
}
