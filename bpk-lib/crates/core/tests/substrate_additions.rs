// justifies: INV-TEST-PANIC-AS-ASSERTION; this integration harness uses panic! to surface structural regressions with explicit messages.
#![allow(clippy::panic, clippy::unwrap_used)]

use batpak::coordinate::{namespace_prefix_matches, Coordinate, Region};
use batpak::event::EventKind;
use batpak::guard::{
    Denial, DenialPayload, Gate, GateEvaluation, GateId, GateIdError, GateSet, Verdict,
};
use batpak::store::cold_start::rebuild::OpenIndexPath;
use batpak::store::MAX_CHECKPOINT_ID_LEN;
use batpak::store::{
    AppendOptions, AppendReceipt, BatchAppendItem, CausationRef, CheckpointId, CheckpointIdError,
    CursorGapConfig, EncodedBytes, ExtensionKey, ExtensionKeyError, GapObservation, IdempotencyKey,
    ReceiptExtensionKey, ReceiptExtensionNamespace, ReceiptExtensionValue, Store, StoreConfig,
};
#[cfg(feature = "blake3")]
use batpak::store::{DenialReceipt, SigningKey};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use tempfile::TempDir;

struct AllowAllGate;

struct AcmeNamespace;

impl ReceiptExtensionNamespace for AcmeNamespace {
    const PREFIX: &'static str = "acme";
}

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
    let _module_witness = batpak::store::delivery::observation::CheckpointId::new("module-path")
        .expect("valid checkpoint id");
    let checkpoint = CheckpointId::new("typed-checkpoint").expect("valid checkpoint id");
    let idempotency = IdempotencyKey::from_bytes([9; 32]);
    let raw_bytes = idempotency.as_bytes();
    let _at_least_once_type_witness: Option<batpak::store::AtLeastOnce> = None;
    let _observed_once_type_witness: Option<batpak::store::ObservedOnce> = None;

    assert_eq!(raw_bytes, &[9; 32]);
    assert_eq!(checkpoint.as_str(), "typed-checkpoint");
    assert_eq!(CheckpointId::new(""), Err(CheckpointIdError::Empty));
    assert!(MAX_CHECKPOINT_ID_LEN >= checkpoint.as_str().len());
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
fn caller_supplied_receipt_extensions_flow_through_append_paths() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )
    .expect("open store");
    let coord = Coordinate::new("extension:entity", "scope:test").expect("coord");
    let kind = EventKind::custom(0xA, 11);
    let commit_key = ExtensionKey::new("acme.commit").expect("extension key");
    let batch_key = ExtensionKey::new("acme.batch").expect("extension key");
    let batch_extra_key = ExtensionKey::new("acme.batch_extra").expect("extension key");
    let denial_key = ExtensionKey::new("acme.denial").expect("extension key");
    let typed_key =
        ReceiptExtensionKey::<AcmeNamespace>::new("typed").expect("typed extension key");
    assert_eq!(typed_key.as_key().as_str(), "acme.typed");
    let mut append_extensions = std::collections::BTreeMap::new();
    append_extensions.insert(commit_key.clone(), vec![1, 2, 3]);

    let append_receipt = store
        .append_with_options(
            &coord,
            kind,
            &serde_json::json!({"n": 1}),
            AppendOptions::new()
                .with_extensions(append_extensions)
                .with_receipt_extension(typed_key.clone(), ReceiptExtensionValue::new(vec![8])),
        )
        .expect("append with extension");
    assert_eq!(
        append_receipt.extensions.get(&commit_key),
        Some(&vec![1, 2, 3])
    );
    assert!(store.verify_append_receipt(&append_receipt));

    let mut batch_extensions = std::collections::BTreeMap::new();
    batch_extensions.insert(batch_key.clone(), vec![4, 5, 6]);
    let batch_item = BatchAppendItem::new(
        coord.clone(),
        kind,
        &serde_json::json!({"n": 2}),
        AppendOptions::new(),
        CausationRef::None,
    )
    .expect("batch item")
    .with_extensions(batch_extensions)
    .with_extension(batch_extra_key.clone(), vec![9])
    .with_receipt_extension(typed_key.clone(), ReceiptExtensionValue::new(vec![11]));
    let batch_receipts = store.append_batch(vec![batch_item]).expect("append batch");
    assert_eq!(
        batch_receipts[0].extensions.get(&batch_key),
        Some(&vec![4, 5, 6])
    );
    assert_eq!(
        batch_receipts[0].extensions.get(&batch_extra_key),
        Some(&vec![9])
    );
    assert_eq!(
        batch_receipts[0].extensions.get(typed_key.as_key()),
        Some(&vec![11])
    );

    let mut gates = GateSet::new();
    gates.push(AllowAllGate);
    gates.push(DenyGate);
    let failing = Denial::new("deny_gate", "blocked");
    let denial_receipt = store
        .append_denial(
            &coord,
            kind,
            &gates,
            &failing,
            Some([0xA5; 32]),
            Some("pipeline:extension".to_owned()),
            AppendOptions::new().with_extension(denial_key.clone(), vec![10]),
        )
        .expect("append denial");
    assert_eq!(denial_receipt.extensions.get(&denial_key), Some(&vec![10]));
    assert!(store.verify_denial_receipt(&denial_receipt));
}

#[test]
fn idempotency_replay_uses_committed_receipt_extensions() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )
    .expect("open store");
    let coord = Coordinate::new("extension:idempotent", "scope:test").expect("coord");
    let kind = EventKind::custom(0xA, 12);
    let key = ExtensionKey::new("acme.idem").expect("extension key");

    let first = store
        .append_with_options(
            &coord,
            kind,
            &serde_json::json!({"n": 1}),
            AppendOptions::new()
                .with_idempotency(0xA11CE)
                .with_extension(key.clone(), vec![7, 8, 9]),
        )
        .expect("first append");
    let replay = store
        .append_with_options(
            &coord,
            kind,
            &serde_json::json!({"n": 99}),
            AppendOptions::new()
                .with_idempotency(0xA11CE)
                .with_extension(key.clone(), vec![0]),
        )
        .expect("idempotent replay");

    assert_eq!(replay.event_id, first.event_id);
    assert_eq!(replay.extensions.get(&key), Some(&vec![7, 8, 9]));
    assert!(store.verify_append_receipt(&replay));
}

#[test]
fn receipt_extensions_count_against_single_append_limit() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false)
            .with_single_append_max_bytes(64),
    )
    .expect("open store");
    let coord = Coordinate::new("extension:limit", "scope:test").expect("coord");
    let kind = EventKind::custom(0xA, 18);
    let key = ExtensionKey::new("acme.large").expect("extension key");

    let result = store.append_with_options(
        &coord,
        kind,
        &serde_json::json!({"n": 1}),
        AppendOptions::new().with_extension(key, vec![0xAB; 128]),
    );

    match result {
        Ok(_) => panic!("PROPERTY: extension bytes must count against single append limit"),
        Err(err) => assert!(
            matches!(err, batpak::store::StoreError::Configuration(ref message) if message.contains("single append bytes")),
            "wrong error: {err:?}"
        ),
    }
}

#[test]
fn receipt_extensions_count_against_batch_limits() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false)
            .with_single_append_max_bytes(64)
            .with_batch_max_bytes(1024),
    )
    .expect("open store");
    let coord = Coordinate::new("extension:batch-limit", "scope:test").expect("coord");
    let kind = EventKind::custom(0xA, 19);
    let key = ExtensionKey::new("acme.large").expect("extension key");
    let per_item = BatchAppendItem::new(
        coord.clone(),
        kind,
        &serde_json::json!({"n": 1}),
        AppendOptions::new().with_extension(key.clone(), vec![0xCD; 128]),
        CausationRef::None,
    )
    .expect("batch item");

    match store.append_batch(vec![per_item]) {
        Ok(_) => panic!("PROPERTY: extension bytes must count against batch per-item limit"),
        Err(err) => assert!(
            matches!(err, batpak::store::StoreError::BatchItemTooLarge { .. }),
            "wrong error: {err:?}"
        ),
    }

    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false)
            .with_single_append_max_bytes(1024)
            .with_batch_max_bytes(200),
    )
    .expect("open store");
    let make_item = |n: u32| {
        BatchAppendItem::new(
            coord.clone(),
            kind,
            &serde_json::json!({"n": n}),
            AppendOptions::new().with_extension(key.clone(), vec![0xEF; 128]),
            CausationRef::None,
        )
        .expect("batch item")
    };

    match store.append_batch(vec![make_item(1), make_item(2)]) {
        Ok(_) => panic!("PROPERTY: extension bytes must count against batch total limit"),
        Err(err) => assert!(
            matches!(err, batpak::store::StoreError::BatchFailed { .. }),
            "wrong error: {err:?}"
        ),
    }
}

fn receipt_extension_restore_config(
    path: &std::path::Path,
    enable_checkpoint: bool,
    enable_mmap_index: bool,
) -> StoreConfig {
    StoreConfig::new(path)
        .with_enable_checkpoint(enable_checkpoint)
        .with_enable_mmap_index(enable_mmap_index)
}

fn assert_receipt_extensions_survive_close_reopen_case(
    enable_checkpoint: bool,
    enable_mmap_index: bool,
    expected_path: OpenIndexPath,
) {
    let dir = TempDir::new().expect("temp dir");
    let coord = Coordinate::new("extension:reopen", "scope:test").expect("coord");
    let kind = EventKind::custom(0xA, 13);
    let append_key = ExtensionKey::new("acme.reopen_append").expect("extension key");
    let batch_key = ExtensionKey::new("acme.reopen_batch").expect("extension key");
    let denial_key = ExtensionKey::new("acme.reopen_denial").expect("extension key");

    {
        let store = Store::open(receipt_extension_restore_config(
            dir.path(),
            enable_checkpoint,
            enable_mmap_index,
        ))
        .expect("open store");
        store
            .append_with_options(
                &coord,
                kind,
                &serde_json::json!({"n": 1}),
                AppendOptions::new()
                    .with_idempotency(0xE1)
                    .with_extension(append_key.clone(), vec![1]),
            )
            .expect("append");
        let batch_item = BatchAppendItem::new(
            coord.clone(),
            kind,
            &serde_json::json!({"n": 2}),
            AppendOptions::new()
                .with_idempotency(0xE2)
                .with_extension(batch_key.clone(), vec![2]),
            CausationRef::None,
        )
        .expect("batch item");
        store.append_batch(vec![batch_item]).expect("append batch");

        let mut gates = GateSet::new();
        gates.push(DenyGate);
        let failing = Denial::new("deny_gate", "blocked");
        store
            .append_denial(
                &coord,
                kind,
                &gates,
                &failing,
                Some([0x5A; 32]),
                Some("pipeline:reopen".to_owned()),
                AppendOptions::new()
                    .with_idempotency(0xE3)
                    .with_extension(denial_key.clone(), vec![3]),
            )
            .expect("append denial");
        store.close().expect("close store");
    }

    let reopened = Store::open(receipt_extension_restore_config(
        dir.path(),
        enable_checkpoint,
        enable_mmap_index,
    ))
    .expect("reopen store");
    let open_report = reopened
        .diagnostics()
        .open_report
        .expect("reopen diagnostics should include open report");
    assert_eq!(
        open_report.path, expected_path,
        "restore-path coverage drifted for checkpoint={enable_checkpoint} mmap={enable_mmap_index}"
    );
    let append_replay = reopened
        .append_with_options(
            &coord,
            kind,
            &serde_json::json!({"n": 99}),
            AppendOptions::new()
                .with_idempotency(0xE1)
                .with_extension(append_key.clone(), vec![9]),
        )
        .expect("replay append");
    assert_eq!(append_replay.extensions.get(&append_key), Some(&vec![1]));
    assert!(reopened.verify_append_receipt(&append_replay));

    let batch_replay_item = BatchAppendItem::new(
        coord.clone(),
        kind,
        &serde_json::json!({"n": 100}),
        AppendOptions::new()
            .with_idempotency(0xE2)
            .with_extension(batch_key.clone(), vec![9]),
        CausationRef::None,
    )
    .expect("batch replay item");
    let batch_replay = reopened
        .append_batch(vec![batch_replay_item])
        .expect("replay batch");
    assert_eq!(batch_replay[0].extensions.get(&batch_key), Some(&vec![2]));

    let mut gates = GateSet::new();
    gates.push(DenyGate);
    let failing = Denial::new("deny_gate", "blocked");
    let denial_replay = reopened
        .append_denial(
            &coord,
            kind,
            &gates,
            &failing,
            Some([0x5A; 32]),
            Some("pipeline:reopen".to_owned()),
            AppendOptions::new()
                .with_idempotency(0xE3)
                .with_extension(denial_key.clone(), vec![9]),
        )
        .expect("replay denial");
    assert_eq!(denial_replay.extensions.get(&denial_key), Some(&vec![3]));
    assert!(reopened.verify_denial_receipt(&denial_replay));
}

#[test]
fn receipt_extensions_survive_close_reopen_restore_paths() {
    for (enable_checkpoint, enable_mmap_index, expected_path) in [
        (false, false, OpenIndexPath::Rebuild),
        (true, false, OpenIndexPath::Checkpoint),
        (true, true, OpenIndexPath::Mmap),
    ] {
        assert_receipt_extensions_survive_close_reopen_case(
            enable_checkpoint,
            enable_mmap_index,
            expected_path,
        );
    }
}

#[cfg(feature = "blake3")]
#[test]
fn signed_unknown_extensions_survive_reopen_and_verify() {
    let dir = TempDir::new().expect("temp dir");
    let key = SigningKey::from_bytes([0x44; 32]);
    let coord = Coordinate::new("extension:signed", "scope:test").expect("coord");
    let kind = EventKind::custom(0xA, 14);
    let pcp_key = ExtensionKey::new("pcp.receipt").expect("pcp extension key");
    let app_key = ExtensionKey::new("acme.receipt").expect("app extension key");

    {
        let store = Store::open(
            StoreConfig::new(dir.path())
                .with_enable_checkpoint(true)
                .with_enable_mmap_index(true)
                .with_signing_key(key.clone()),
        )
        .expect("open signed store");
        let receipt = store
            .append_with_options(
                &coord,
                kind,
                &serde_json::json!({"n": 1}),
                AppendOptions::new()
                    .with_idempotency(0x51_6E_D0)
                    .with_extension(pcp_key.clone(), vec![0x50, 0x43, 0x50])
                    .with_extension(app_key.clone(), vec![0x41, 0x50, 0x50]),
            )
            .expect("append signed extension receipt");
        assert_eq!(
            receipt.extensions.get(&pcp_key),
            Some(&vec![0x50, 0x43, 0x50])
        );
        assert_eq!(
            receipt.extensions.get(&app_key),
            Some(&vec![0x41, 0x50, 0x50])
        );
        assert!(store.verify_append_receipt(&receipt));
        store.close().expect("close signed store");
    }

    let reopened = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(true)
            .with_enable_mmap_index(true)
            .with_signing_key(key),
    )
    .expect("reopen signed store");
    let replay = reopened
        .append_with_options(
            &coord,
            kind,
            &serde_json::json!({"n": 2}),
            AppendOptions::new()
                .with_idempotency(0x51_6E_D0)
                .with_extension(pcp_key.clone(), vec![0])
                .with_extension(app_key.clone(), vec![0]),
        )
        .expect("idempotent replay");

    assert_eq!(
        replay.extensions.get(&pcp_key),
        Some(&vec![0x50, 0x43, 0x50])
    );
    assert_eq!(
        replay.extensions.get(&app_key),
        Some(&vec![0x41, 0x50, 0x50])
    );
    assert!(reopened.verify_append_receipt(&replay));

    let mut tampered = replay.clone();
    tampered.extensions.insert(pcp_key, vec![0x66]);
    assert!(
        !reopened.verify_append_receipt(&tampered),
        "pcp.* bytes are opaque substrate cargo and must still be covered by the signature"
    );
}

#[test]
fn namespace_prefix_query_excludes_adjacent_namespaces() {
    let child_matches = namespace_prefix_matches("alice", "alice:child");
    let adjacent_matches = namespace_prefix_matches("alice", "alice2");
    assert!(child_matches);
    assert!(!adjacent_matches);

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
        .map(|entry| entry.coord().entity().to_owned())
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
        .with_gap_config(CursorGapConfig::Enabled {
            capacity: NonZeroUsize::new(4).expect("nonzero capacity"),
        });
    let delivered = cursor.poll_batch(8);
    let gaps: Vec<GapObservation> = cursor.take_gaps();

    assert_eq!(delivered.len(), 2);
    assert_eq!(delivered[0].global_sequence(), receipt0.sequence);
    assert_eq!(delivered[1].global_sequence(), receipt2.sequence);
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
        .with_gap_config(CursorGapConfig::Disabled);
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
        .with_gap_config(CursorGapConfig::Enabled {
            capacity: NonZeroUsize::new(1).expect("nonzero capacity"),
        });
    let delivered = cursor.poll_batch(8);
    let gaps = cursor.take_gaps();

    assert_eq!(delivered.len(), 3);
    assert_eq!(delivered[0].global_sequence(), receipt0.sequence);
    assert_eq!(delivered[1].global_sequence(), receipt2.sequence);
    assert_eq!(delivered[2].global_sequence(), receipt4.sequence);
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
            .read_raw(denial_receipt.event_id)
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

#[cfg(feature = "blake3")]
#[test]
fn receipt_verification_rejects_stripped_signature_and_index_field_tampering() {
    let dir = TempDir::new().expect("temp dir");
    let key = SigningKey::from_bytes([0x66; 32]);
    let coord = Coordinate::new("signed:verify", "scope:test").expect("coord");
    let kind = EventKind::custom(0xA, 18);
    let ext_key = ExtensionKey::new("acme.verify").expect("extension key");

    let store =
        Store::open(StoreConfig::new(dir.path()).with_signing_key(key)).expect("open signed store");
    let receipt = store
        .append_with_options(
            &coord,
            kind,
            &serde_json::json!({"n": 1}),
            AppendOptions::new().with_extension(ext_key.clone(), vec![1, 2, 3]),
        )
        .expect("append signed event");

    assert!(
        store.verify_append_receipt(&receipt),
        "fresh signed receipt should verify"
    );

    let mut stripped = receipt.clone();
    stripped.key_id = [0; 32];
    stripped.signature = None;
    assert!(
        !store.verify_append_receipt(&stripped),
        "stripping a signature must fail when the store has a verifying key registry"
    );

    let mut wrong_sequence = receipt.clone();
    wrong_sequence.sequence += 1;
    assert!(
        !store.verify_append_receipt(&wrong_sequence),
        "sequence must match the committed index entry, not just the signature cover"
    );

    let mut wrong_content_hash = receipt.clone();
    wrong_content_hash.content_hash[0] ^= 0xFF;
    assert!(
        !store.verify_append_receipt(&wrong_content_hash),
        "content hash must match the committed index entry"
    );

    let mut wrong_extensions = receipt.clone();
    wrong_extensions.extensions.insert(ext_key, vec![9]);
    assert!(
        !store.verify_append_receipt(&wrong_extensions),
        "receipt extensions must match the committed index entry"
    );
}

#[test]
fn unsigned_receipt_verification_still_checks_committed_index_state() {
    let dir = TempDir::new().expect("temp dir");
    let coord = Coordinate::new("unsigned:verify", "scope:test").expect("coord");
    let kind = EventKind::custom(0xA, 19);
    let store = Store::open(StoreConfig::new(dir.path())).expect("open unsigned store");
    let receipt = store
        .append(&coord, kind, &serde_json::json!({"n": 1}))
        .expect("append unsigned event");

    assert!(
        store.verify_append_receipt(&receipt),
        "unsigned receipts still verify when signing is not configured"
    );

    let mut wrong_sequence: AppendReceipt = receipt.clone();
    wrong_sequence.sequence += 1;
    assert!(
        !store.verify_append_receipt(&wrong_sequence),
        "unsigned verification must still reject receipts whose index fields do not match"
    );
}
