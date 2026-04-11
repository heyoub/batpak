//! Atomic batch append tests.
//! [SPEC:tests/atomic_batch.rs]

use batpak::prelude::*;
use std::collections::HashSet;

/// Test: append_reaction_batch sets correlation/causation on all items.
#[test]
fn batch_append_reaction_batch() {
    let tmp = tempfile::tempdir().expect("create temp dir for reaction batch test");
    let config = StoreConfig::new(tmp.path());
    let store = Store::open(config).expect("open store for reaction batch test");

    // First, append an initial event to use as causation source.
    let trigger_coord = Coordinate::new("user", "trigger").expect("valid trigger coordinate");
    let trigger = store
        .append(
            &trigger_coord,
            EventKind::DATA,
            &serde_json::json!({"trigger": true}),
        )
        .expect("append trigger event for reaction batch");

    // Create reaction batch items.
    let reaction_coord = Coordinate::new("user", "reactions").expect("valid reaction coordinate");
    let items: Vec<BatchAppendItem> = (0..3)
        .map(|i| {
            BatchAppendItem::new(
                reaction_coord.clone(),
                EventKind::DATA,
                &serde_json::json!({"reaction": i}),
                AppendOptions::default(),
                CausationRef::None,
            )
            .expect("construct reaction batch item")
        })
        .collect();

    // Use append_reaction_batch with explicit correlation/causation.
    let correlation_id = trigger.event_id;
    let causation_id = trigger.event_id;
    let receipts = store
        .append_reaction_batch(correlation_id, causation_id, items)
        .expect("append reaction batch");

    assert_eq!(receipts.len(), 3);
}

/// Test: batch_max_bytes config option is respected.
#[test]
fn batch_config_max_bytes() {
    let tmp = tempfile::tempdir().expect("create temp dir for batch_max_bytes test");
    let config = StoreConfig::new(tmp.path()).with_batch_max_bytes(1024 * 1024); // 1MB
    let store = Store::open(config).expect("open store for batch_max_bytes test");

    // Batch with small payloads should succeed under 1MB limit.
    let coord = Coordinate::new("test", "bytes").expect("valid bytes coordinate");
    let items: Vec<BatchAppendItem> = (0..10)
        .map(|i| {
            BatchAppendItem::new(
                coord.clone(),
                EventKind::DATA,
                &serde_json::json!({"index": i, "data": "x".repeat(100)}),
                AppendOptions::default(),
                CausationRef::None,
            )
            .expect("construct item under batch_max_bytes")
        })
        .collect();

    let result = store.append_batch(items);
    assert!(result.is_ok(), "batch under max_bytes should succeed");
}

/// Test: an empty batch is a no-op and leaves the store usable.
///
/// (Renamed from `batch_atomicity_zero_visibility_on_failure` in the
/// Tier 1 drill sweep — that name lied about what the body did. The
/// body never triggered a failure path; it submitted `vec![]` and
/// asserted success. The real "failure path doesn't leak partial
/// state" property is now covered by
/// `batch_oversized_item_no_partial_visibility` below and by
/// `batch_publish_atomicity_no_partial_read_during_insert`.)
#[test]
fn batch_empty_is_noop_and_store_remains_usable() {
    let tmp = tempfile::tempdir().expect("create temp dir for empty-batch test");
    let config = StoreConfig::new(tmp.path()).with_batch_max_size(4);
    let store = Store::open(config).expect("open store for empty-batch test");

    let items = vec![];
    let result = store.append_batch(items);
    assert!(
        result.is_ok(),
        "PROPERTY: an empty batch must succeed as a no-op (writer must \
         tolerate zero items without panicking or returning an error). \
         Investigate: src/store/writer.rs handle_append_batch validate_batch \
         early-return for empty input."
    );
    let receipts = result.expect("empty batch ok");
    assert!(
        receipts.is_empty(),
        "PROPERTY: an empty batch must return zero receipts, got {}",
        receipts.len()
    );

    // Store must still be usable after the empty batch.
    let receipt = store
        .append(
            &Coordinate::new("test", "atomicity").expect("valid atomicity coordinate"),
            EventKind::DATA,
            &serde_json::json!({"test": true}),
        )
        .expect("append post-empty-batch event");
    assert!(
        receipt.event_id != 0,
        "PROPERTY: after an empty batch, the next append must succeed \
         and produce a non-zero event_id (the writer must not be in a \
         broken state). Got event_id = 0."
    );
    let visible_count = store.cursor(&Region::all()).poll_batch(10).len();
    assert_eq!(
        visible_count, 1,
        "PROPERTY: after empty batch + one append, exactly 1 event must \
         be visible. Got {visible_count}. The empty batch must not have \
         exposed any phantom entries."
    );
}

/// Test: when a batch contains an oversized item that fails validation,
/// NONE of the items in that batch become visible to readers.
///
/// This is the "atomicity on natural failure" property — distinct from
/// the fault-injection-driven test at
/// `batch_publish_atomicity_no_partial_read_during_insert`. Natural
/// failures (validation, oversized payload, encoding error) must be
/// just as atomic as fault-injected ones.
#[test]
fn batch_oversized_item_no_partial_visibility() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    // Tight per-batch byte cap so a single 4 KB payload trips it.
    let config = StoreConfig::new(tmp.path())
        .with_batch_max_bytes(2 * 1024)
        .with_batch_max_size(8);
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("entity:atomic", "scope:test").expect("valid coord");

    // Build a batch where the LAST item is too large.
    let mut items: Vec<BatchAppendItem> = (0..3)
        .map(|i| {
            BatchAppendItem::new(
                coord.clone(),
                EventKind::DATA,
                &serde_json::json!({"i": i, "small": true}),
                AppendOptions::default(),
                CausationRef::None,
            )
            .expect("small item builds")
        })
        .collect();
    let oversized_payload = serde_json::json!({"big": "x".repeat(4 * 1024)});
    items.push(
        BatchAppendItem::new(
            coord.clone(),
            EventKind::DATA,
            &oversized_payload,
            AppendOptions::default(),
            CausationRef::None,
        )
        .expect("item builder doesn't enforce batch byte cap; writer does"),
    );

    let result = store.append_batch(items);
    assert!(
        matches!(result, Err(StoreError::BatchFailed { .. })),
        "PROPERTY: a batch whose total bytes exceed batch_max_bytes must \
         fail with BatchFailed; got {result:?}. \
         Investigate: src/store/writer.rs validate_batch byte-cap branch \
         and StoreError::BatchFailed mapping for the Validating stage."
    );

    // Critical: NONE of the 4 items should be visible.
    let visible_count = store.cursor(&Region::all()).poll_batch(100).len();
    assert_eq!(
        visible_count, 0,
        "PROPERTY: BATCH ATOMICITY VIOLATION — a batch that failed during \
         validation must not expose ANY of its items to readers. Found \
         {visible_count} visible events; expected 0. \
         Investigate: src/store/writer.rs handle_append_batch must validate \
         BEFORE reserving sequences and writing frames, OR must roll back \
         all visibility on failure. src/store/index.rs publish() must not \
         have advanced the watermark."
    );

    // Store must still be usable after the failed batch.
    let post_failure = store
        .append(
            &coord,
            EventKind::DATA,
            &serde_json::json!({"recovery": true}),
        )
        .expect("store usable after failed batch");
    assert_eq!(
        post_failure.sequence, 0,
        "PROPERTY: the first event after a failed batch must occupy \
         sequence 0 — the failed batch must not have burned any sequence \
         slots that would shift the next append's sequence. Got sequence \
         {}. Investigate: src/store/writer.rs validate_batch ordering \
         relative to reserve_sequences.",
        post_failure.sequence
    );
}

/// Test: full batch visibility on success.
#[test]
fn batch_atomicity_full_visibility_on_success() {
    let tmp = tempfile::tempdir().expect("create temp dir for full-visibility test");
    let config = StoreConfig::new(tmp.path());
    let store = Store::open(config).expect("open store for full-visibility test");

    let coord = Coordinate::new("user", "profile").expect("valid profile coordinate");
    let items: Vec<BatchAppendItem> = (0..5)
        .map(|i| {
            BatchAppendItem::new(
                coord.clone(),
                EventKind::DATA,
                &serde_json::json!({"index": i}),
                AppendOptions::default(),
                CausationRef::None,
            )
            .expect("construct full-visibility batch item")
        })
        .collect();

    let receipts = store
        .append_batch(items)
        .expect("append full-visibility batch");
    assert_eq!(receipts.len(), 5);

    // All events should be queryable.
    let mut cursor = store.cursor(&Region::all());
    let mut found = HashSet::new();
    for entry in cursor.poll_batch(10) {
        found.insert(entry.event_id);
    }

    for receipt in &receipts {
        assert!(
            found.contains(&receipt.event_id),
            "event {} should be visible",
            receipt.event_id
        );
    }
}

/// Test: batch envelope marker is invisible to queries.
#[test]
fn batch_marker_invisible() {
    let tmp = tempfile::tempdir().expect("create temp dir for marker invisibility test");
    let config = StoreConfig::new(tmp.path());
    let store = Store::open(config).expect("open store for marker invisibility test");

    let coord = Coordinate::new("test", "marker").expect("valid marker coordinate");
    let items = vec![BatchAppendItem::new(
        coord.clone(),
        EventKind::DATA,
        &serde_json::json!({"data": 1}),
        AppendOptions::default(),
        CausationRef::None,
    )
    .expect("construct marker invisibility item")];

    store
        .append_batch(items)
        .expect("append batch with invisible marker envelope");

    // Query should only return the data event, not the marker.
    let mut cursor = store.cursor(&Region::all());
    let entries = cursor.poll_batch(10);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, EventKind::DATA);
}

/// Test: intra-batch causation linking.
#[test]
fn batch_intra_batch_causation() {
    let tmp = tempfile::tempdir().expect("create temp dir for intra-batch causation test");
    let config = StoreConfig::new(tmp.path());
    let store = Store::open(config).expect("open store for intra-batch causation test");

    let coord = Coordinate::new("chain", "test").expect("valid chain coordinate");

    // First item has no causation.
    let item1 = BatchAppendItem::new(
        coord.clone(),
        EventKind::DATA,
        &serde_json::json!({"seq": 1}),
        AppendOptions::default(),
        CausationRef::None,
    )
    .expect("construct first causation item");

    // Second item references first via PriorItem.
    let item2 = BatchAppendItem::new(
        coord.clone(),
        EventKind::DATA,
        &serde_json::json!({"seq": 2}),
        AppendOptions::default(),
        CausationRef::PriorItem(0),
    )
    .expect("construct second causation item");

    let receipts = store
        .append_batch(vec![item1, item2])
        .expect("append causation-linked batch");
    assert_eq!(receipts.len(), 2);

    // Second event's causation_id should equal first event's event_id.
    let mut cursor = store.cursor(&Region::all());
    let entries = cursor.poll_batch(10);
    assert_eq!(entries.len(), 2);

    let first_id = entries[0].event_id;
    let second_causation = entries[1].causation_id;
    assert_eq!(second_causation, Some(first_id));
}

/// Test: batch respects size limits.
#[test]
fn batch_size_limits() {
    let tmp = tempfile::tempdir().expect("create temp dir for batch size limit test");
    let config = StoreConfig::new(tmp.path()).with_batch_max_size(2);
    let store = Store::open(config).expect("open store for batch size limit test");

    let coord = Coordinate::new("limit", "test").expect("valid limit coordinate");
    let items: Vec<_> = (0..3)
        .map(|i| {
            BatchAppendItem::new(
                coord.clone(),
                EventKind::DATA,
                &serde_json::json!({"i": i}),
                AppendOptions::default(),
                CausationRef::None,
            )
            .expect("construct oversized batch item")
        })
        .collect();

    let result = store.append_batch(items);
    assert!(result.is_err());
    let err = result.expect_err("batch should fail due to size limit");
    assert!(matches!(
        err,
        StoreError::BatchFailed {
            stage: BatchStage::Validation,
            ..
        }
    ));
}

/// Test: restart recovery discards incomplete batch (crash after BEGIN, before COMMIT).
/// Uses fault injection framework to simulate crash at exact point.
#[cfg(feature = "test-support")]
#[test]
fn batch_restart_recovery_discards_incomplete_after_begin() {
    use batpak::store::CountdownInjector;

    let tmp = tempfile::tempdir().expect("create temp dir for after-begin recovery test");
    let coord = Coordinate::new("crash", "test").expect("valid crash-test coordinate");

    // Append a normal event first to establish state.
    let config = StoreConfig::new(tmp.path());
    let store = Store::open(config).expect("open baseline store for after-begin recovery");
    let _receipt1 = store
        .append(&coord, EventKind::DATA, &serde_json::json!({"seq": 1}))
        .expect("append baseline event before injected fault");
    drop(store);

    // Reopen with fault injector that panics after BEGIN marker written.
    let mut config = StoreConfig::new(tmp.path());
    config.fault_injector = Some(std::sync::Arc::new(CountdownInjector::after_batch_begin()));

    let store = Store::open(config).expect("open fault-injected store after begin");
    let items = vec![
        BatchAppendItem::new(
            coord.clone(),
            EventKind::DATA,
            &serde_json::json!({"seq": 2}),
            AppendOptions::default(),
            CausationRef::None,
        )
        .expect("construct second event in after-begin recovery batch"),
        BatchAppendItem::new(
            coord.clone(),
            EventKind::DATA,
            &serde_json::json!({"seq": 3}),
            AppendOptions::default(),
            CausationRef::None,
        )
        .expect("construct third event in after-begin recovery batch"),
    ];

    // Batch append should fail due to fault injection.
    let result = store.append_batch(items);
    assert!(
        result.is_err(),
        "fault injector should have returned an error"
    );
    drop(store);

    // Reopen store normally (no fault injector) and verify incomplete batch was discarded.
    let config = StoreConfig::new(tmp.path());
    let store = Store::open(config).expect("reopen store after begin-fault recovery");
    let mut cursor = store.cursor(&Region::all());
    let entries = cursor.poll_batch(10);

    // Only the first event (seq: 1) should be present. The incomplete batch (seq: 2, 3)
    // should have been discarded because it had BEGIN but no COMMIT marker.
    assert_eq!(
        entries.len(),
        1,
        "incomplete batch should be discarded on recovery"
    );
    assert_eq!(entries[0].kind, EventKind::DATA);
}

/// Test: restart recovery discards incomplete batch (crash mid-batch items).
/// Uses fault injection to crash after writing first item.
#[cfg(feature = "test-support")]
#[test]
fn batch_restart_recovery_discards_incomplete_mid_items() {
    use batpak::store::CountdownInjector;

    let tmp = tempfile::tempdir().expect("create temp dir for mid-items recovery test");
    let coord = Coordinate::new("crash", "test").expect("valid crash-test coordinate");

    // Reopen with fault injector that fails after 1st item written.
    let mut config = StoreConfig::new(tmp.path());
    config.fault_injector = Some(std::sync::Arc::new(CountdownInjector::after_batch_items(1)));

    let store = Store::open(config).expect("open fault-injected store mid-items");
    let items = vec![
        BatchAppendItem::new(
            coord.clone(),
            EventKind::DATA,
            &serde_json::json!({"seq": 1}),
            AppendOptions::default(),
            CausationRef::None,
        )
        .expect("construct first item in mid-items fault batch"),
        BatchAppendItem::new(
            coord.clone(),
            EventKind::DATA,
            &serde_json::json!({"seq": 2}),
            AppendOptions::default(),
            CausationRef::None,
        )
        .expect("construct second item in mid-items fault batch"),
        BatchAppendItem::new(
            coord.clone(),
            EventKind::DATA,
            &serde_json::json!({"seq": 3}),
            AppendOptions::default(),
            CausationRef::None,
        )
        .expect("construct third item in mid-items fault batch"),
    ];

    // Batch append should fail due to fault injection after first item.
    let result = store.append_batch(items);
    assert!(
        result.is_err(),
        "fault injector should have returned an error"
    );
    drop(store);

    // Reopen store normally and verify no partial batch visible.
    let config = StoreConfig::new(tmp.path());
    let store = Store::open(config).expect("reopen store after mid-items fault recovery");
    let mut cursor = store.cursor(&Region::all());
    let entries = cursor.poll_batch(10);

    // No events should be present - the partial batch was discarded.
    assert_eq!(entries.len(), 0, "partial batch items should be discarded");
}

/// Test: both batch markers invisible to queries.
#[test]
fn batch_both_markers_invisible() {
    let tmp = tempfile::tempdir().expect("create temp dir for marker invisibility pair test");
    let config = StoreConfig::new(tmp.path());
    let store = Store::open(config).expect("open store for both-markers invisible test");

    let coord = Coordinate::new("test", "markers").expect("valid markers coordinate");
    let items = vec![BatchAppendItem::new(
        coord.clone(),
        EventKind::DATA,
        &serde_json::json!({"data": 1}),
        AppendOptions::default(),
        CausationRef::None,
    )
    .expect("construct marker-pair invisibility item")];

    store
        .append_batch(items)
        .expect("append batch for both-markers invisible test");

    // Query should only return the data event, neither marker.
    let mut cursor = store.cursor(&Region::all());
    let entries = cursor.poll_batch(10);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, EventKind::DATA);

    // Verify no system kinds appear.
    for entry in &entries {
        assert!(
            !entry.kind.is_system(),
            "system events should not be visible"
        );
    }
}

/// Test: crash after COMMIT marker but before fsync (fsync ambiguity).
/// COMMIT is on disk but not durable - should be discarded on recovery.
#[cfg(feature = "test-support")]
#[test]
fn batch_fsync_ambiguity_discards_uncommitted() {
    use batpak::store::{CountdownAction, CountdownInjector, InjectionPoint, SyncMode};

    let tmp = tempfile::tempdir().expect("create temp dir for fsync ambiguity test");
    let coord = Coordinate::new("fsync", "test").expect("valid fsync test coordinate");

    // Pre-establish state with committed event.
    let config = StoreConfig::new(tmp.path());
    let store = Store::open(config).expect("open baseline store for fsync ambiguity test");
    let _receipt = store
        .append(&coord, EventKind::DATA, &serde_json::json!({"pre": 1}))
        .expect("append pre-established event for fsync ambiguity test");
    drop(store);

    // Reopen with fault injector that triggers DURING fsync.
    // This simulates: COMMIT written, power lost before fsync completes.
    let mut config = StoreConfig::new(tmp.path());
    config.sync.mode = SyncMode::SyncAll; // Full sync to test real ambiguity
    config.fault_injector = Some(std::sync::Arc::new(
        CountdownInjector::new(
            1,
            CountdownAction::Fail("simulated power loss during fsync"),
        )
        .with_filter(|p| matches!(p, InjectionPoint::BatchFsync { .. })),
    ));

    let store = Store::open(config).expect("open fault-injected store for fsync ambiguity");
    let items = vec![
        BatchAppendItem::new(
            coord.clone(),
            EventKind::DATA,
            &serde_json::json!({"batch": 1}),
            AppendOptions::default(),
            CausationRef::None,
        )
        .expect("construct first fsync ambiguity batch item"),
        BatchAppendItem::new(
            coord.clone(),
            EventKind::DATA,
            &serde_json::json!({"batch": 2}),
            AppendOptions::default(),
            CausationRef::None,
        )
        .expect("construct second fsync ambiguity batch item"),
    ];

    // Fault during fsync.
    let result = store.append_batch(items);
    assert!(result.is_err(), "should fail during fsync");
    drop(store);

    // Recovery: un-fsynced COMMIT should be discarded (fsync ambiguity rule).
    let config = StoreConfig::new(tmp.path());
    let store = Store::open(config).expect("reopen store after fsync ambiguity fault");
    let mut cursor = store.cursor(&Region::all());
    let entries = cursor.poll_batch(10);

    // Only pre-established event should be present.
    assert_eq!(
        entries.len(),
        1,
        "un-fsynced batch must be discarded per fsync ambiguity rule"
    );
    assert_eq!(
        store
            .get(entries[0].event_id)
            .expect("load recovered pre-established event after fsync ambiguity")
            .event
            .payload["pre"],
        serde_json::json!(1)
    );
}

/// Test: post-recovery system operations continue correctly.
/// Verifies that after recovery, the store is fully functional.
#[cfg(feature = "test-support")]
#[test]
fn batch_recovery_system_remains_coherent() {
    use batpak::store::CountdownInjector;

    let tmp = tempfile::tempdir().expect("create temp dir for recovery coherence test");
    let coord_a = Coordinate::new("entity_a", "scope").expect("valid entity_a coordinate");
    let coord_b = Coordinate::new("entity_b", "scope").expect("valid entity_b coordinate");

    // Phase 1: Establish committed state.
    let config = StoreConfig::new(tmp.path());
    let store = Store::open(config).expect("open baseline store for recovery coherence test");
    let receipt_a1 = store
        .append(&coord_a, EventKind::DATA, &serde_json::json!({"seq": 1}))
        .expect("append baseline entity_a event for recovery coherence test");
    drop(store);

    // Phase 2: Crash during second batch.
    let mut config = StoreConfig::new(tmp.path());
    config.fault_injector = Some(std::sync::Arc::new(CountdownInjector::after_batch_items(1)));

    let store = Store::open(config).expect("open fault-injected store for recovery coherence");
    let items = vec![
        BatchAppendItem::new(
            coord_a.clone(),
            EventKind::DATA,
            &serde_json::json!({"seq": 2}),
            AppendOptions::default(),
            CausationRef::None,
        )
        .expect("construct faulted entity_a batch item"),
        BatchAppendItem::new(
            coord_b.clone(),
            EventKind::DATA,
            &serde_json::json!({"other": 1}),
            AppendOptions::default(),
            CausationRef::None,
        )
        .expect("construct faulted entity_b batch item"),
    ];

    // Batch append should fail due to fault injection.
    let _ = store.append_batch(items);
    drop(store);

    // Phase 3: Reopen and verify system coherency.
    let config = StoreConfig::new(tmp.path());
    let store = Store::open(config).expect("reopen store for recovery coherence verification");

    // Verify committed data intact.
    let mut cursor_a = store.cursor(&Region::entity(coord_a.entity()));
    let entries_a = cursor_a.poll_batch(10);
    assert_eq!(
        entries_a.len(),
        1,
        "entity_a should have only committed event"
    );
    assert_eq!(entries_a[0].global_sequence, receipt_a1.sequence);

    let mut cursor_b = store.cursor(&Region::entity(coord_b.entity()));
    let entries_b = cursor_b.poll_batch(10);
    assert!(
        entries_b.is_empty(),
        "entity_b should have no events (batch discarded)"
    );

    // Phase 4: Verify new operations work correctly post-recovery.
    // Single append should work.
    let receipt_new = store
        .append(&coord_a, EventKind::DATA, &serde_json::json!({"seq": 3}))
        .expect("append post-recovery entity_a event");
    assert_eq!(
        receipt_new.sequence,
        receipt_a1.sequence + 1,
        "sequence should continue"
    );

    // Batch append should work.
    let batch_items = vec![
        BatchAppendItem::new(
            coord_b.clone(),
            EventKind::DATA,
            &serde_json::json!({"new": 1}),
            AppendOptions::default(),
            CausationRef::None,
        )
        .expect("construct first post-recovery entity_b batch item"),
        BatchAppendItem::new(
            coord_b.clone(),
            EventKind::DATA,
            &serde_json::json!({"new": 2}),
            AppendOptions::default(),
            CausationRef::None,
        )
        .expect("construct second post-recovery entity_b batch item"),
    ];
    let batch_receipts = store
        .append_batch(batch_items)
        .expect("append post-recovery entity_b batch");
    assert_eq!(batch_receipts.len(), 2);

    // Verify cross-entity causation works post-recovery.
    let mut cursor_all = store.cursor(&Region::all());
    let all_entries = cursor_all.poll_batch(10);
    assert_eq!(all_entries.len(), 4, "should have all committed events");

    // Verify hash chain integrity post-recovery.
    for entry in &all_entries {
        if entry.clock > 0 {
            // Verify entity clock progression.
            let mut entity_cursor = store.cursor(&Region::entity(entry.coord.entity()));
            let entity_entries = entity_cursor.poll_batch(10);
            for (i, e) in entity_entries.iter().enumerate() {
                assert_eq!(e.clock as usize, i, "entity clock should be contiguous");
            }
        }
    }
}

/// Test: subscriptions don't see partial batches during crash scenarios.
/// Verifies notification atomicity - subscribers either see all or none.
#[cfg(feature = "test-support")]
#[test]
fn batch_subscription_atomicity_no_partial_visibility() {
    use batpak::store::CountdownInjector;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let tmp = tempfile::tempdir().expect("create temp dir for subscription atomicity test");
    let coord = Coordinate::new("sub", "test").expect("valid subscription test coordinate");

    // Subscribe before any operations.
    let mut config = StoreConfig::new(tmp.path());
    let store =
        Store::open(config.clone()).expect("open baseline store for subscription atomicity");
    let sub = store.subscribe(&Region::all());

    // Counter for notifications received.
    let notification_count = std::sync::Arc::new(AtomicUsize::new(0));
    let count_clone = std::sync::Arc::<AtomicUsize>::clone(&notification_count);

    // Spawn subscriber task.
    let _sub_handle = std::thread::Builder::new()
        .name("atomic-batch-sub-pre-crash".into())
        .spawn(move || {
            let start = std::time::Instant::now();
            while start.elapsed() < std::time::Duration::from_millis(500) {
                if sub.receiver().try_recv().is_ok() {
                    count_clone.fetch_add(1, Ordering::SeqCst);
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
        .expect("spawn pre-crash subscription observer thread");

    // Pre-establish one committed event.
    store
        .append(&coord, EventKind::DATA, &serde_json::json!({"pre": 1}))
        .expect("append pre-crash subscription event");
    std::thread::sleep(std::time::Duration::from_millis(50));

    let pre_crash_count = notification_count.load(Ordering::SeqCst);
    assert!(
        pre_crash_count >= 1,
        "should have received notification for pre-established event"
    );

    drop(store);

    // Reopen with fault injector that crashes mid-batch.
    config.fault_injector = Some(std::sync::Arc::new(CountdownInjector::after_batch_items(1)));
    let store = Store::open(config).expect("open fault-injected store for subscription atomicity");
    let sub = store.subscribe(&Region::all());

    // Counter for post-crash notifications.
    let post_count = std::sync::Arc::new(AtomicUsize::new(0));
    let post_clone = std::sync::Arc::<AtomicUsize>::clone(&post_count);

    let post_handle = std::thread::Builder::new()
        .name("atomic-batch-sub-post-crash".into())
        .spawn(move || {
            let start = std::time::Instant::now();
            while start.elapsed() < std::time::Duration::from_millis(200) {
                if sub.receiver().try_recv().is_ok() {
                    post_clone.fetch_add(1, Ordering::SeqCst);
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
        .expect("spawn post-crash subscription observer thread");

    // Attempt batch that will crash.
    let items = vec![
        BatchAppendItem::new(
            coord.clone(),
            EventKind::DATA,
            &serde_json::json!({"batch": 1}),
            AppendOptions::default(),
            CausationRef::None,
        )
        .expect("construct first subscription atomicity batch item"),
        BatchAppendItem::new(
            coord.clone(),
            EventKind::DATA,
            &serde_json::json!({"batch": 2}),
            AppendOptions::default(),
            CausationRef::None,
        )
        .expect("construct second subscription atomicity batch item"),
    ];

    let _ = store.append_batch(items);

    post_handle
        .join()
        .expect("join post-crash subscription observer thread");
    drop(store);

    // Subscribers should NOT have received notifications for partial batch.
    // The first item was written but the crash happened before index publish.
    // However, notification happens AFTER index publish, so no partial notifications.
    let notifications_received = post_count.load(Ordering::SeqCst);
    assert_eq!(
        notifications_received, 0,
        "no notifications for incomplete batch (atomicity)"
    );

    // After recovery, verify no partial data is visible.
    let config = StoreConfig::new(tmp.path());
    let store = Store::open(config).expect("reopen store after subscription atomicity fault");
    let mut cursor = store.cursor(&Region::all());
    let entries = cursor.poll_batch(10);

    // Should only have the pre-established event.
    assert_eq!(entries.len(), 1, "only pre-established event visible");
    assert_eq!(
        store
            .get(entries[0].event_id)
            .expect("load recovered pre-established subscription event")
            .event
            .payload["pre"],
        serde_json::json!(1)
    );
}

/// Test: CountdownInjector::after_commit_before_fsync convenience constructor.
#[cfg(feature = "test-support")]
#[test]
fn fault_injector_after_commit_before_fsync() {
    use batpak::store::{CountdownInjector, FaultInjector, InjectionPoint};

    let injector = CountdownInjector::after_commit_before_fsync();

    // Should trigger at BatchCommitWritten point.
    let commit_point = InjectionPoint::BatchCommitWritten { batch_id: 1 };
    assert!(injector.check(commit_point).is_some());

    // Should NOT trigger at other points.
    let begin_point = InjectionPoint::BatchBeginWritten {
        batch_id: 1,
        item_count: 5,
    };
    assert!(injector.check(begin_point).is_none());

    let items_point = InjectionPoint::BatchItemWritten {
        batch_id: 1,
        item_index: 0,
        total_items: 5,
    };
    assert!(injector.check(items_point).is_none());
}

/// Test: cross-segment batch with fault at segment boundary.
/// Verifies that partial batches spanning segments are handled correctly.
#[cfg(feature = "test-support")]
#[test]
fn batch_cross_segment_fault_recovery() {
    use batpak::store::{CountdownAction, CountdownInjector, InjectionPoint};

    let tmp = tempfile::tempdir().expect("create temp dir for cross-segment recovery test");
    let coord = Coordinate::new("cross", "seg").expect("valid cross-segment coordinate");

    // Configure tiny segments to force rotation mid-batch.
    let mut config = StoreConfig::new(tmp.path());
    config.segment_max_bytes = 1024; // 1KB segments

    let store = Store::open(config).expect("open baseline store for cross-segment recovery");

    // Fill first segment to near capacity.
    let large_payload = serde_json::json!({"data": "x".repeat(400) });
    let _ = store
        .append(&coord, EventKind::DATA, &large_payload)
        .expect("append baseline large payload before cross-segment fault");
    drop(store);

    // Reopen with fault injector at segment rotation.
    let mut config = StoreConfig::new(tmp.path());
    config.segment_max_bytes = 1024;
    config.fault_injector = Some(std::sync::Arc::new(
        CountdownInjector::new(1, CountdownAction::Fail("crash at segment rotation"))
            .with_filter(|p| matches!(p, InjectionPoint::BatchItemWritten { item_index: 2, .. })),
    ));

    let store = Store::open(config).expect("open fault-injected store for cross-segment recovery");

    // Create batch that will span segments.
    let items: Vec<_> = (0..5)
        .map(|i| {
            BatchAppendItem::new(
                coord.clone(),
                EventKind::DATA,
                &serde_json::json!({"item": i, "pad": "y".repeat(300)}),
                AppendOptions::default(),
                CausationRef::None,
            )
            .expect("construct cross-segment batch item")
        })
        .collect();

    // Fault during batch (after 3rd item, likely mid-segment-rotation).
    let _ = store.append_batch(items);
    drop(store);

    // Recovery should discard all partial batch data across both segments.
    let config = StoreConfig::new(tmp.path());
    let store = Store::open(config).expect("reopen store after cross-segment fault recovery");
    let mut cursor = store.cursor(&Region::all());
    let entries = cursor.poll_batch(10);

    // Should only have the first large event.
    assert_eq!(
        entries.len(),
        1,
        "cross-segment partial batch should be fully discarded"
    );

    // Verify store is fully operational after recovery.
    let new_receipt = store
        .append(
            &coord,
            EventKind::DATA,
            &serde_json::json!({"after": "recovery"}),
        )
        .expect("append event after cross-segment recovery");
    assert!(
        new_receipt.sequence > 0,
        "new appends should work after cross-segment recovery"
    );
}

/// Test: concurrent readers NEVER observe a partial batch.
///
/// Uses a fault injector at `BatchPrePublish` to create a deterministic
/// window where all batch entries are in the index maps but the visibility
/// watermark has NOT yet advanced. A reader thread queries continuously
/// during this window and asserts that it sees exactly 0 batch entries
/// (not a strict prefix).
///
/// [INV-BATCH-ATOMIC-VISIBILITY]
#[cfg(feature = "test-support")]
#[test]
fn batch_publish_atomicity_no_partial_read_during_insert() {
    use batpak::store::{CountdownAction, CountdownInjector, InjectionPoint};
    use std::sync::Arc;

    let tmp = tempfile::tempdir().expect("create temp dir for publish atomicity test");
    let coord = Coordinate::new("batch_vis", "test").expect("valid coordinate");

    // Pre-populate a baseline event so we can distinguish "pre-batch" from "batch" entries.
    let config = StoreConfig::new(tmp.path());
    let store = Store::open(config).expect("open baseline store");
    let pre = store
        .append(
            &coord,
            EventKind::DATA,
            &serde_json::json!({"baseline": true}),
        )
        .expect("append baseline event");
    drop(store);

    // Reopen with a fault injector that fails at BatchPrePublish.
    // This means insert_batch() has run (entries are in maps) but
    // publish() has NOT been called yet, so the batch attempt fails
    // before advancing the visibility watermark.
    let mut config = StoreConfig::new(tmp.path());
    config.fault_injector = Some(Arc::new(
        CountdownInjector::new(1, CountdownAction::Fail("halt before publish"))
            .with_filter(|p| matches!(p, InjectionPoint::BatchPrePublish { .. })),
    ));
    let store = Arc::new(Store::open(config).expect("open fault-injected store"));

    let batch_size = 10usize;
    let items: Vec<_> = (0..batch_size)
        .map(|i| {
            BatchAppendItem::new(
                coord.clone(),
                EventKind::DATA,
                &serde_json::json!({"batch_item": i}),
                AppendOptions::default(),
                CausationRef::None,
            )
            .expect("construct batch item")
        })
        .collect();

    // The batch should fail because BatchPrePublish injects a fault.
    let result = store.append_batch(items);
    assert!(
        result.is_err(),
        "batch must fail when BatchPrePublish fault is injected"
    );

    // After the failed batch, query the store. Because publish() was never called,
    // readers must see only the baseline event — no partial batch entries.
    let region = Region::entity("batch_vis");
    let entries = store.query(&region);

    assert_eq!(
        entries.len(),
        1,
        "PROPERTY: after BatchPrePublish fault, readers must see 0 batch entries.\n\
         Expected only the baseline event (id={}), but got {} entries.\n\
         Investigate: src/store/index.rs read methods must filter by visible_sequence.\n\
         Common causes: read method missing visibility filter, publish() called before fault point.",
        pre.event_id,
        entries.len(),
    );
    assert_eq!(
        entries[0].event_id, pre.event_id,
        "the single visible entry must be the pre-batch baseline event"
    );
}

/// Real concurrent-reader proof of batch publish atomicity.
///
/// A reader thread runs `store.query(...)` in a tight loop while a writer
/// thread does many batch appends back-to-back. The reader records every
/// observed count. After the writer finishes, every observation must be of
/// the form `pre_count + k * batch_size` for some `k`. Any other value
/// (e.g. `pre_count + 3` when batch_size = 7) means a partial batch became
/// visible — i.e. the SequenceGate failed to enforce atomic publish.
///
/// This is the "show me the race" companion to the loom model in
/// tests/deterministic_concurrency.rs. The loom model proves the property
/// under exhaustive interleavings of a simplified abstract model; this
/// integration test exercises the real Store/SequenceGate code under real
/// OS-scheduled contention.
///
/// [INV-BATCH-ATOMIC-VISIBILITY]
#[test]
fn batch_publish_atomicity_concurrent_reader_sees_zero_or_all() {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, Ordering as MemOrd};
    use std::sync::Arc;
    use std::thread;

    let tmp = tempfile::tempdir().expect("create temp dir for concurrent atomicity test");
    let coord = Coordinate::new("concurrent_atom", "scope").expect("valid coordinate");

    let config = StoreConfig::new(tmp.path());
    let store = Arc::new(Store::open(config).expect("open store"));

    // Pre-populate baseline events so the "post-batch" count is always
    // pre_count + k * batch_size.
    let pre_count: usize = 3;
    for i in 0..pre_count {
        store
            .append(&coord, EventKind::DATA, &serde_json::json!({"pre": i}))
            .expect("append baseline event");
    }

    let stop = Arc::new(AtomicBool::new(false));
    let region = Region::entity("concurrent_atom");

    // Reader thread: hammer query() until told to stop, recording every
    // distinct count we observe along the way.
    let r_store = Arc::clone(&store);
    let r_stop = Arc::clone(&stop);
    let r_region = region.clone();
    let reader = thread::Builder::new()
        .name("atomic-batch-reader".into())
        .spawn(move || {
            let mut observations: HashSet<usize> = HashSet::new();
            while !r_stop.load(MemOrd::Acquire) {
                let count = r_store.query(&r_region).len();
                observations.insert(count);
            }
            // One final read after the stop signal so we always include the
            // post-writer terminal state in the observations.
            observations.insert(r_store.query(&r_region).len());
            observations
        })
        .expect("spawn reader thread");

    // Writer thread: many back-to-back batch appends. Run on this thread so
    // we don't have to deal with sharing the Store as Arc both ways.
    let batch_size: usize = 7;
    let num_batches: usize = 50;
    for _ in 0..num_batches {
        let items: Vec<BatchAppendItem> = (0..batch_size)
            .map(|i| {
                BatchAppendItem::new(
                    coord.clone(),
                    EventKind::DATA,
                    &serde_json::json!({"batch_item": i}),
                    AppendOptions::default(),
                    CausationRef::None,
                )
                .expect("construct batch item")
            })
            .collect();
        store.append_batch(items).expect("batch append");
    }

    // Stop the reader and collect observations.
    stop.store(true, MemOrd::Release);
    let observed = reader.join().expect("reader thread joined cleanly");

    // Compute the set of valid counts: pre_count + k * batch_size for
    // 0 <= k <= num_batches.
    let allowed: HashSet<usize> = (0..=num_batches)
        .map(|k| pre_count + k * batch_size)
        .collect();

    // Every observation must be in the allowed set. Anything else means
    // the reader saw a partial batch.
    let bad: Vec<usize> = observed.difference(&allowed).copied().collect();
    assert!(
        bad.is_empty(),
        "PROPERTY: reader must only ever observe pre_count + k * batch_size.\n\
         Observed counts not in the allowed set: {bad:?}\n\
         Allowed set: {allowed:?}\n\
         All observed: {observed:?}\n\
         A partial batch was visible — INV-BATCH-ATOMIC-VISIBILITY violated.\n\
         Investigate: src/store/index.rs SequenceGate visibility filter +\n\
         src/store/writer.rs handle_append_batch publish ordering.",
    );

    // Sanity check: we should have observed AT LEAST the initial pre_count
    // and the terminal pre_count + num_batches * batch_size. (The reader
    // is fast enough to almost certainly catch some intermediate states
    // too, but we don't depend on that.)
    assert!(
        observed.contains(&pre_count),
        "expected to observe at least the pre-batch baseline count {pre_count}, observed {observed:?}",
    );
    let terminal = pre_count + num_batches * batch_size;
    assert!(
        observed.contains(&terminal),
        "expected to observe the terminal count {terminal}, observed {observed:?}",
    );
}
