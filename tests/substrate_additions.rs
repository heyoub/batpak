// justifies: INV-TEST-PANIC-AS-ASSERTION; this integration harness uses panic! to surface structural regressions with explicit messages.
#![allow(clippy::panic, clippy::unwrap_used)]

use batpak::coordinate::{namespace_prefix_matches, Coordinate, Region};
use batpak::event::EventKind;
use batpak::guard::{
    Denial, DenialPayload, Gate, GateEvaluation, GateId, GateIdError, GateSet, Verdict,
};
use batpak::store::{
    CheckpointId, CursorGapConfig, EncodedBytes, ExtensionKey, ExtensionKeyError, GapObservation,
    IdempotencyKey, Store, StoreConfig,
};
#[cfg(feature = "blake3")]
use batpak::store::{DenialReceipt, SigningKey};
use std::path::PathBuf;
use tempfile::TempDir;

struct AllowAllGate;

impl Gate<()> for AllowAllGate {
    fn name(&self) -> &'static str {
        "allow_all"
    }

    fn evaluate(&self, _ctx: &()) -> Result<(), Denial> {
        Ok(())
    }
}

struct DenyGate;

impl Gate<()> for DenyGate {
    fn name(&self) -> &'static str {
        "deny_gate"
    }

    fn evaluate(&self, _ctx: &()) -> Result<(), Denial> {
        Err(Denial::new("deny_gate", "blocked")
            .with_code("BLOCKED")
            .with_context("reason", "test"))
    }
}

#[test]
fn encoding_surface_round_trips_and_canonical_alias_matches() {
    let payload = serde_json::json!({"hello": "world", "n": 7});
    let encoded = batpak::encoding::to_bytes(&payload).expect("encode payload");
    let alias_encoded = batpak::canonical::to_bytes(&payload).expect("encode payload via alias");
    let decoded: serde_json::Value =
        batpak::encoding::from_bytes(&encoded).expect("decode payload");

    assert_eq!(encoded, alias_encoded);
    assert_eq!(decoded, payload);
}

#[test]
fn observation_witnesses_round_trip() {
    let _module_witness = batpak::store::delivery::observation::CheckpointId::new("module-path");
    let checkpoint = CheckpointId::new("typed-checkpoint");
    let idempotency = IdempotencyKey::from_bytes([9; 32]);
    let raw_bytes = idempotency.as_bytes();
    let _at_least_once_type_witness: Option<batpak::store::AtLeastOnce> = None;
    let _observed_once_type_witness: Option<batpak::store::ObservedOnce> = None;

    assert_eq!(raw_bytes, &[9; 32]);
    assert_eq!(checkpoint.as_str(), "typed-checkpoint");
}

#[test]
fn extension_key_validates_namespace_rules() {
    let key = ExtensionKey::new("acme.trace_id").expect("valid extension key");
    let encoded: EncodedBytes = batpak::encoding::to_bytes(&serde_json::json!({"k": "v"}))
        .expect("encode extension payload");
    assert_eq!(key.as_str(), "acme.trace_id");
    assert!(!encoded.is_empty());
    assert_eq!(
        ExtensionKey::new("batpak.trace_id"),
        Err(ExtensionKeyError::ReservedNamespace)
    );
    assert_eq!(
        ExtensionKey::new("not-a-namespace"),
        Err(ExtensionKeyError::InvalidNamespaceFormat)
    );
}

#[test]
fn namespace_prefix_query_excludes_adjacent_namespaces() {
    assert!(namespace_prefix_matches("alice", "alice:child"));
    assert!(!namespace_prefix_matches("alice", "alice2"));

    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )
    .expect("open store");
    let kind = EventKind::custom(0xA, 1);
    let alice = Coordinate::new("alice", "scope:test").expect("alice coord");
    let alice_child = Coordinate::new("alice:child", "scope:test").expect("alice child coord");
    let alice2 = Coordinate::new("alice2", "scope:test").expect("alice2 coord");

    store
        .append(&alice, kind, &serde_json::json!({"n": 1}))
        .unwrap();
    store
        .append(&alice_child, kind, &serde_json::json!({"n": 2}))
        .unwrap();
    store
        .append(&alice2, kind, &serde_json::json!({"n": 3}))
        .unwrap();

    let hits = store.query(&Region::entity("alice"));
    let entities = hits
        .into_iter()
        .map(|entry| entry.coord.entity().to_owned())
        .collect::<Vec<_>>();

    assert_eq!(entities, vec!["alice".to_owned(), "alice:child".to_owned()]);
    store.close().expect("close store");
}

#[test]
fn cursor_gap_accessor_drains_once() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )
    .expect("open store");
    let kind = EventKind::custom(0xA, 2);
    let coord = Coordinate::new("gap:test", "scope:test").expect("gap coord");

    let receipt0 = store
        .append(&coord, kind, &serde_json::json!({"n": 0}))
        .expect("append first");
    let fence = store.begin_visibility_fence().expect("begin fence");
    let _fenced_ticket = fence
        .submit(&coord, kind, &serde_json::json!({"n": 1}))
        .expect("submit fenced");
    fence.cancel().expect("cancel fence");
    let receipt2 = store
        .append(&coord, kind, &serde_json::json!({"n": 2}))
        .expect("append third");

    let mut cursor = store
        .cursor_guaranteed(&Region::entity("gap:test"))
        .with_gap_config(CursorGapConfig {
            enabled: true,
            buffer_capacity: 4,
        });
    let delivered = cursor.poll_batch(8);
    let gaps: Vec<GapObservation> = cursor.take_gaps();

    assert_eq!(delivered.len(), 2);
    assert_eq!(delivered[0].global_sequence, receipt0.sequence);
    assert_eq!(delivered[1].global_sequence, receipt2.sequence);
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].expected_sequence, receipt0.sequence + 1);
    assert_eq!(gaps[0].delivered_sequence, receipt2.sequence);
    assert!(
        cursor.take_gaps().is_empty(),
        "take_gaps must drain observations"
    );

    store.close().expect("close store");
}

#[test]
fn cursor_gap_accessor_is_quiet_when_disabled() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )
    .expect("open store");
    let kind = EventKind::custom(0xA, 3);
    let coord = Coordinate::new("gap:disabled", "scope:test").expect("gap coord");

    let _receipt0 = store
        .append(&coord, kind, &serde_json::json!({"n": 0}))
        .expect("append first");
    let fence = store.begin_visibility_fence().expect("begin fence");
    let _fenced_ticket = fence
        .submit(&coord, kind, &serde_json::json!({"n": 1}))
        .expect("submit fenced");
    fence.cancel().expect("cancel fence");
    let _receipt2 = store
        .append(&coord, kind, &serde_json::json!({"n": 2}))
        .expect("append third");

    let mut cursor = store
        .cursor_guaranteed(&Region::entity("gap:disabled"))
        .with_gap_config(CursorGapConfig {
            enabled: false,
            buffer_capacity: 4,
        });
    let _delivered = cursor.poll_batch(8);

    assert!(
        cursor.take_gaps().is_empty(),
        "disabled gap tracking must not retain observations"
    );

    store.close().expect("close store");
}

#[test]
fn cursor_gap_accessor_uses_bounded_ring_buffer() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )
    .expect("open store");
    let kind = EventKind::custom(0xA, 4);
    let coord = Coordinate::new("gap:ring", "scope:test").expect("gap coord");

    let receipt0 = store
        .append(&coord, kind, &serde_json::json!({"n": 0}))
        .expect("append visible 0");
    let fence1 = store.begin_visibility_fence().expect("begin first fence");
    let _ticket1 = fence1
        .submit(&coord, kind, &serde_json::json!({"n": 1}))
        .expect("submit first hidden event");
    fence1.cancel().expect("cancel first fence");
    let receipt2 = store
        .append(&coord, kind, &serde_json::json!({"n": 2}))
        .expect("append visible 2");
    let fence2 = store.begin_visibility_fence().expect("begin second fence");
    let _ticket2 = fence2
        .submit(&coord, kind, &serde_json::json!({"n": 3}))
        .expect("submit second hidden event");
    fence2.cancel().expect("cancel second fence");
    let receipt4 = store
        .append(&coord, kind, &serde_json::json!({"n": 4}))
        .expect("append visible 4");

    let mut cursor = store
        .cursor_guaranteed(&Region::entity("gap:ring"))
        .with_gap_config(CursorGapConfig {
            enabled: true,
            buffer_capacity: 1,
        });
    let delivered = cursor.poll_batch(8);
    let gaps = cursor.take_gaps();

    assert_eq!(delivered.len(), 3);
    assert_eq!(delivered[0].global_sequence, receipt0.sequence);
    assert_eq!(delivered[1].global_sequence, receipt2.sequence);
    assert_eq!(delivered[2].global_sequence, receipt4.sequence);
    assert_eq!(gaps.len(), 1, "bounded ring buffer must evict oldest gaps");
    assert_eq!(gaps[0].expected_sequence, receipt2.sequence + 1);
    assert_eq!(gaps[0].delivered_sequence, receipt4.sequence);
    assert_eq!(
        gaps[0].cancelled_ranges,
        vec![(receipt2.sequence + 1, receipt4.sequence)],
    );

    store.close().expect("close store");
}

#[test]
fn guarded_prefix_sites_no_longer_use_raw_starts_with() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let coordinate = std::fs::read_to_string(root.join("src/coordinate/mod.rs")).unwrap();
    let query = std::fs::read_to_string(root.join("src/store/index/query.rs")).unwrap();

    assert!(
        !coordinate.contains("entity.starts_with(prefix.as_ref())"),
        "Region::matches_event must use the canonical namespace predicate",
    );
    assert!(
        !query.contains("starts_with(prefix.as_ref())"),
        "index query candidate scans must use the canonical namespace predicate",
    );
}

#[test]
fn denial_accessor_public_surface_is_named_in_tests() {
    let _evaluations: fn(&DenialPayload) -> &[GateEvaluation] = DenialPayload::evaluations;
    let _pipeline_id: fn(&DenialPayload) -> Option<&str> = DenialPayload::pipeline_id;
    let _proposed_kind: fn(&DenialPayload) -> EventKind = DenialPayload::proposed_kind;
    let _proposed_content_hash: fn(&DenialPayload) -> Option<[u8; 32]> =
        DenialPayload::proposed_content_hash;
    let _gate_id: fn(&GateEvaluation) -> &GateId = GateEvaluation::gate_id;
    let _verdict: fn(&GateEvaluation) -> &Verdict = GateEvaluation::verdict;
    let _evidence_hash: fn(&GateEvaluation) -> Option<[u8; 32]> = GateEvaluation::evidence_hash;
}

#[test]
fn denial_trace_round_trip() {
    let gate_id = GateId::new("manual_gate").expect("gate id");
    let evaluation = GateEvaluation::new(gate_id.clone(), Verdict::Permit, None);
    assert_eq!(gate_id.as_str(), "manual_gate");
    assert_eq!(GateId::new(""), Err(GateIdError::Empty));
    let _evaluation_witness: GateEvaluation = evaluation;

    let mut gates = GateSet::new();
    gates.push(AllowAllGate);
    gates.push(DenyGate);
    let failing = Denial::new("deny_gate", "blocked")
        .with_code("BLOCKED")
        .with_context("reason", "test");
    let traced: DenialPayload = gates.trace_denial(
        &failing,
        EventKind::custom(0xA, 9),
        Some([0xCD; 32]),
        Some("pipeline:test".to_owned()),
    );
    assert_eq!(traced.pipeline_id(), Some("pipeline:test"));
    assert_eq!(traced.proposed_kind(), EventKind::custom(0xA, 9));
    assert_eq!(traced.proposed_content_hash(), Some([0xCD; 32]));
    assert_eq!(traced.evaluations().len(), 2);
    assert_eq!(traced.evaluations()[0].gate_id().as_str(), "allow_all");
    assert!(matches!(traced.evaluations()[0].verdict(), Verdict::Permit));
    assert_eq!(traced.evaluations()[0].evidence_hash(), None);
    assert_eq!(traced.evaluations()[1].gate_id().as_str(), "deny_gate");
    assert!(matches!(
        traced.evaluations()[1].verdict(),
        Verdict::Deny { code, message, context }
            if code == "BLOCKED"
                && message == "blocked"
                && context == &vec![("reason".to_owned(), "test".to_owned())]
    ));
    let _payload_witness: DenialPayload = traced;
}

#[cfg(feature = "blake3")]
#[test]
fn signed_receipts_round_trip() {
    let dir = TempDir::new().expect("temp dir");
    let key1 = SigningKey::from_bytes([1; 32]);
    let key2 = SigningKey::from_bytes([2; 32]);
    let coord = Coordinate::new("signed:entity", "scope:test").expect("coord");
    let kind = EventKind::custom(0xA, 10);
    let mut gates = GateSet::new();
    gates.push(AllowAllGate);
    gates.push(DenyGate);
    let failing = Denial::new("deny_gate", "blocked")
        .with_code("BLOCKED")
        .with_context("reason", "test");

    let receipt1 = {
        let store = Store::open(
            StoreConfig::new(dir.path())
                .with_enable_checkpoint(false)
                .with_enable_mmap_index(false)
                .with_signing_key(key1.clone()),
        )
        .expect("open signed store");
        let receipt = store
            .append(&coord, kind, &serde_json::json!({"n": 1}))
            .expect("append signed event");
        let verified_receipt = store.verify_append_receipt(&receipt);
        assert_ne!(receipt.key_id, [0; 32]);
        assert!(receipt.signature.is_some());
        assert!(verified_receipt);
        store.close().expect("close signed store");
        receipt
    };

    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false)
            .with_signing_key(key1)
            .with_signing_key(key2),
    )
    .expect("reopen signed store with rotated key");
    let verified_old_receipt = store.verify_append_receipt(&receipt1);
    assert!(
        verified_old_receipt,
        "older signed receipts must verify after key rotation"
    );

    let receipt2 = store
        .append(&coord, kind, &serde_json::json!({"n": 2}))
        .expect("append second signed event");
    let verified_new_receipt = store.verify_append_receipt(&receipt2);
    assert!(verified_new_receipt);

    let mut tampered = receipt2.clone();
    tampered.extensions.insert(
        ExtensionKey::new("acme.trace").expect("extension key"),
        vec![1, 2, 3],
    );
    let verified_tampered_receipt = store.verify_append_receipt(&tampered);
    assert!(
        !verified_tampered_receipt,
        "tampering with receipt extensions must invalidate the signature"
    );

    let denial_receipt: DenialReceipt = store
        .append_denial(
            &coord,
            kind,
            &gates,
            &failing,
            Some([0xEF; 32]),
            Some("pipeline:test".to_owned()),
            batpak::store::AppendOptions::new(),
        )
        .expect("append denial");
    assert_eq!(
        store
            .get_raw(denial_receipt.event_id)
            .expect("read denial")
            .event
            .header
            .event_kind,
        EventKind::SYSTEM_DENIAL
    );
    let verified_denial_receipt = store.verify_denial_receipt(&denial_receipt);
    assert!(verified_denial_receipt);

    store.close().expect("close rotated store");
}
