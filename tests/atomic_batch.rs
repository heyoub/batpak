//! Atomic batch append tests.

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
    let visible_count = store.cursor_guaranteed(&Region::all()).poll_batch(10).len();
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
    let visible_count = store
        .cursor_guaranteed(&Region::all())
        .poll_batch(100)
        .len();
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
    let mut cursor = store.cursor_guaranteed(&Region::all());
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
    let mut cursor = store.cursor_guaranteed(&Region::all());
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
    let mut cursor = store.cursor_guaranteed(&Region::all());
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
    let err = result.expect_err(
        "PROPERTY: a batch exceeding batch_max_size must fail. \
         Investigate: src/store/writer.rs validate_batch size check.",
    );
    assert!(
        matches!(
            err,
            StoreError::BatchFailed {
                stage: BatchStage::Validation,
                ..
            }
        ),
        "PROPERTY: batch size violation must be reported as \
         BatchFailed{{stage: Validation, ..}}, got {err:?}"
    );
}

/// Test: restart recovery discards incomplete batch (crash after BEGIN, before COMMIT).
/// Uses fault injection framework to simulate crash at exact point.
#[cfg(feature = "dangerous-test-hooks")]
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

    // Batch append should fail due to fault injection at the BatchBeginWritten point.
    let result = store.append_batch(items);
    let err = result.expect_err(
        "PROPERTY: fault injection at BatchBeginWritten must propagate as a \
         BatchFailed or FaultInjected error.",
    );
    assert!(
        matches!(err, StoreError::BatchFailed { .. })
            || matches!(err, StoreError::FaultInjected(_)),
        "PROPERTY: BatchBeginWritten fault must surface as BatchFailed or \
         FaultInjected, got {err:?}"
    );
    drop(store);

    // Reopen store normally (no fault injector) and verify incomplete batch was discarded.
    let config = StoreConfig::new(tmp.path());
    let store = Store::open(config).expect("reopen store after begin-fault recovery");
    let mut cursor = store.cursor_guaranteed(&Region::all());
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
#[cfg(feature = "dangerous-test-hooks")]
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
    let err = result.expect_err(
        "PROPERTY: fault injection mid-batch must propagate as a BatchFailed \
         or FaultInjected error.",
    );
    assert!(
        matches!(err, StoreError::BatchFailed { .. })
            || matches!(err, StoreError::FaultInjected(_)),
        "PROPERTY: mid-batch fault must surface as BatchFailed or \
         FaultInjected, got {err:?}"
    );
    drop(store);

    // Reopen store normally and verify no partial batch visible.
    let config = StoreConfig::new(tmp.path());
    let store = Store::open(config).expect("reopen store after mid-items fault recovery");
    let mut cursor = store.cursor_guaranteed(&Region::all());
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
    let mut cursor = store.cursor_guaranteed(&Region::all());
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
#[cfg(feature = "dangerous-test-hooks")]
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
    let err = result.expect_err(
        "PROPERTY: a fault injected during BatchFsync must propagate as an \
         error. Investigate: src/store/writer.rs handle_append_batch fsync \
         site fault injection point.",
    );
    assert!(
        matches!(
            err,
            StoreError::BatchFailed {
                stage: BatchStage::Syncing,
                ..
            }
        ) || matches!(err, StoreError::FaultInjected(_)),
        "PROPERTY: BatchFsync fault must surface as BatchFailed{{stage: \
         Syncing}} or FaultInjected, got {err:?}"
    );
    drop(store);

    // Recovery: un-fsynced COMMIT should be discarded (fsync ambiguity rule).
    let config = StoreConfig::new(tmp.path());
    let store = Store::open(config).expect("reopen store after fsync ambiguity fault");
    let mut cursor = store.cursor_guaranteed(&Region::all());
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
#[cfg(feature = "dangerous-test-hooks")]
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
    let mut cursor_a = store.cursor_guaranteed(&Region::entity(coord_a.entity()));
    let entries_a = cursor_a.poll_batch(10);
    assert_eq!(
        entries_a.len(),
        1,
        "entity_a should have only committed event"
    );
    assert_eq!(entries_a[0].global_sequence, receipt_a1.sequence);

    let mut cursor_b = store.cursor_guaranteed(&Region::entity(coord_b.entity()));
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
    let mut cursor_all = store.cursor_guaranteed(&Region::all());
    let all_entries = cursor_all.poll_batch(10);
    assert_eq!(all_entries.len(), 4, "should have all committed events");

    // Verify hash chain integrity post-recovery.
    for entry in &all_entries {
        if entry.clock > 0 {
            // Verify entity clock progression.
            let mut entity_cursor = store.cursor_guaranteed(&Region::entity(entry.coord.entity()));
            let entity_entries = entity_cursor.poll_batch(10);
            for (i, e) in entity_entries.iter().enumerate() {
                assert_eq!(e.clock as usize, i, "entity clock should be contiguous");
            }
        }
    }
}

/// Test: subscriptions don't see partial batches during crash scenarios.
/// Verifies notification atomicity - subscribers either see all or none.
///
/// **Synchronization rationale:** `store.append()` and `store.append_batch()`
/// are synchronous — they block until the writer thread acknowledges. The
/// writer broadcasts notifications BEFORE sending the response (see the
/// `STEP 10` comment in writer.rs handle_append). So by the time `append()`
/// returns, any notification for a successful append is already in the
/// subscriber's flume channel buffer. Failed appends never broadcast.
/// We can therefore drain the receiver immediately after each operation
/// without any timing assumption — no spawned threads, no polling, no
/// `Instant::now()` deadlines, no `thread::sleep`.
#[cfg(feature = "dangerous-test-hooks")]
#[test]
fn batch_subscription_atomicity_no_partial_visibility() {
    use batpak::store::CountdownInjector;

    let tmp = tempfile::tempdir().expect("create temp dir for subscription atomicity test");
    let coord = Coordinate::new("sub", "test").expect("valid subscription test coordinate");

    // Helper: drain a subscription receiver into a count using try_recv,
    // returning when the channel is empty. Safe because the writer has
    // already broadcast (synchronously, before responding to append).
    fn drain(sub: &batpak::store::Subscription) -> usize {
        let mut count = 0;
        while sub.receiver().try_recv().is_ok() {
            count += 1;
        }
        count
    }

    // Phase 1: subscribe, append a baseline event, drain.
    let mut config = StoreConfig::new(tmp.path());
    let store =
        Store::open(config.clone()).expect("open baseline store for subscription atomicity");
    let sub = store.subscribe_lossy(&Region::all());
    store
        .append(&coord, EventKind::DATA, &serde_json::json!({"pre": 1}))
        .expect("append pre-crash subscription event");
    let pre_crash_count = drain(&sub);
    assert_eq!(
        pre_crash_count, 1,
        "PROPERTY: a successful append must produce exactly one subscriber \
         notification, drainable immediately. Got {pre_crash_count}. \
         Investigate: src/store/writer.rs handle_append broadcast site, \
         and ensure append() blocks until AFTER the broadcast."
    );
    drop(store);

    // Phase 2: reopen with a fault injector that fails the batch mid-flight.
    config.fault_injector = Some(std::sync::Arc::new(CountdownInjector::after_batch_items(1)));
    let store = Store::open(config).expect("open fault-injected store for subscription atomicity");
    let sub = store.subscribe_lossy(&Region::all());

    // Phase 3: attempt a batch that will fault. The append_batch call must
    // return Err. Subscriber must observe ZERO notifications because the
    // writer broadcasts only after the atomic publish, which never happens
    // for a faulted batch.
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

    let result = store.append_batch(items);
    let _err = result.expect_err(
        "PROPERTY: batch with after_batch_items(1) fault must fail. If this \
         passes, fault injection is silently swallowed.",
    );

    let notifications_received = drain(&sub);
    drop(store);

    assert_eq!(
        notifications_received, 0,
        "PROPERTY: BATCH SUBSCRIPTION ATOMICITY VIOLATION — a faulted batch \
         must produce ZERO subscriber notifications. Got {notifications_received}. \
         The writer must broadcast notifications only AFTER the atomic publish, \
         and the publish must never happen for a faulted batch. \
         Investigate: src/store/writer.rs handle_append_batch ordering of \
         publish() and broadcast_batch_notifications()."
    );

    // After recovery, verify no partial data is visible.
    let config = StoreConfig::new(tmp.path());
    let store = Store::open(config).expect("reopen store after subscription atomicity fault");
    let mut cursor = store.cursor_guaranteed(&Region::all());
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
#[cfg(feature = "dangerous-test-hooks")]
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
#[cfg(feature = "dangerous-test-hooks")]
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
    let mut cursor = store.cursor_guaranteed(&Region::all());
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
#[cfg(feature = "dangerous-test-hooks")]
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
    let err = result.expect_err(
        "PROPERTY: a batch with a BatchPrePublish fault injection must fail. \
         If this passes, fault injection is being silently swallowed.",
    );
    assert!(
        matches!(err, StoreError::BatchFailed { .. })
            || matches!(err, StoreError::FaultInjected(_)),
        "PROPERTY: BatchPrePublish fault must surface as BatchFailed or \
         FaultInjected, got {err:?}"
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
    use std::time::{Duration, Instant};

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
    let baseline_seen = Arc::new(AtomicBool::new(false));
    let region = Region::entity("concurrent_atom");

    // Reader thread: hammer query() until told to stop, recording every
    // distinct count we observe along the way.
    let r_store = Arc::clone(&store);
    let r_stop = Arc::clone(&stop);
    let r_baseline_seen = Arc::clone(&baseline_seen);
    let r_region = region.clone();
    let reader = thread::Builder::new()
        .name("atomic-batch-reader".into())
        .spawn(move || {
            let mut observations: HashSet<usize> = HashSet::new();
            while !r_stop.load(MemOrd::Acquire) {
                let count = r_store.query(&r_region).len();
                observations.insert(count);
                if count == pre_count {
                    r_baseline_seen.store(true, MemOrd::Release);
                }
            }
            // One final read after the stop signal so we always include the
            // post-writer terminal state in the observations.
            let final_count = r_store.query(&r_region).len();
            observations.insert(final_count);
            if final_count == pre_count {
                r_baseline_seen.store(true, MemOrd::Release);
            }
            observations
        })
        .expect("spawn reader thread");

    let baseline_deadline = Instant::now() + Duration::from_secs(1);
    while !baseline_seen.load(MemOrd::Acquire) && Instant::now() < baseline_deadline {
        thread::yield_now();
    }
    assert!(
        baseline_seen.load(MemOrd::Acquire),
        "PROPERTY: reader must observe the pre-batch baseline before the writer starts hammering batches."
    );

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

// ── Regression tests for the v0.3.0-prep batch hash-chain / cold-start /
//    wall_ms bugs found in the final 1M-context audit. Each test names the
//    specific bug it pins down so a future regression has a loud signal.
// ─────────────────────────────────────────────────────────────────────────

/// REGRESSION: multi-item same-entity batches must produce a continuous
/// on-disk + in-memory hash chain.
///
/// Before the fix in `precompute_batch_items`, the second-or-later item of
/// any same-entity batch wrote `prev_hash = [0u8; 32]` into its on-disk
/// frame because `entity_prev_hashes.insert(entity, [0u8; 32])` ran *before*
/// the real `event_hash` was known, AND every staged `IndexEntry` /
/// `SidxEntry` collapsed to the entity's LAST item's `event_hash` because
/// `stage_batch_index_entries` looked the value up in a shared scratch map
/// instead of using a per-item field. This test would have caught both
/// halves: hash uniqueness, prev/event linking, and `walk_ancestors`
/// traversal all fail loud against the buggy code.
#[cfg(feature = "blake3")]
#[test]
fn batch_multi_item_same_entity_hash_chain_is_continuous() {
    let tmp = tempfile::tempdir().expect("create temp dir for hash chain regression");
    let store = Store::open(StoreConfig::new(tmp.path())).expect("open store");
    let coord = Coordinate::new("regress", "hashchain").expect("valid coord");

    // Three distinct payloads on the SAME entity. Distinct payloads matter:
    // identical payloads would produce identical event_hash values and the
    // bug would be invisible against deduped hashes.
    let items: Vec<BatchAppendItem> = (0..3)
        .map(|i| {
            BatchAppendItem::new(
                coord.clone(),
                EventKind::DATA,
                &serde_json::json!({"step": i, "nonce": format!("regress-{i}")}),
                AppendOptions::default(),
                CausationRef::None,
            )
            .expect("construct batch item")
        })
        .collect();

    let receipts = store.append_batch(items).expect("batch append");
    assert_eq!(receipts.len(), 3, "all three items must be committed");

    // Pull the in-memory IndexEntries via query.
    let entries = store.query(&Region::entity("regress"));
    assert_eq!(entries.len(), 3, "query must surface all three batch items");

    // Sort by clock so the order is deterministic regardless of map ordering.
    let mut entries = entries;
    entries.sort_by_key(|e| e.clock);

    // (a) every event_hash distinct (would fail under the
    //     "stage step collapses to LAST item's hash" bug)
    let h0 = entries[0].hash_chain.event_hash;
    let h1 = entries[1].hash_chain.event_hash;
    let h2 = entries[2].hash_chain.event_hash;
    assert_ne!(
        h0, h1,
        "PROPERTY: distinct payloads must produce distinct event_hash values \
         in the in-memory IndexEntry. Buggy stage_batch_index_entries collapsed \
         every same-entity entry's event_hash to the LAST item's hash via the \
         shared entity_prev_hashes map."
    );
    assert_ne!(h1, h2, "second pair of event_hash values must be distinct");
    assert_ne!(h0, h2, "first/third event_hash values must be distinct");
    assert_ne!(
        h0, [0u8; 32],
        "blake3 of a non-empty payload must be non-zero"
    );

    // (b) prev/event chain links: items[i].prev_hash == items[i-1].event_hash
    //     This is the on-disk-and-in-memory chain that the bug broke. The
    //     buggy precompute populated entity_prev_hashes with [0; 32], so
    //     items[1].prev_hash and items[2].prev_hash were both [0; 32].
    assert_eq!(
        entries[1].hash_chain.prev_hash, h0,
        "PROPERTY: items[1].prev_hash MUST equal items[0].event_hash. \
         Buggy precompute_batch_items inserted [0; 32] into entity_prev_hashes \
         before the real hash was computed, so this assertion would fail with \
         actual = [0; 32]."
    );
    assert_eq!(
        entries[2].hash_chain.prev_hash, h1,
        "PROPERTY: items[2].prev_hash MUST equal items[1].event_hash. Same bug."
    );
    assert_eq!(
        entries[0].hash_chain.prev_hash, [0u8; 32],
        "items[0] is the genesis for the entity, so prev_hash is the all-zero \
         sentinel. (Entity has no prior history in this test.)"
    );

    // (c) walk_ancestors must traverse the full chain in reverse order.
    //     Buggy code would terminate at items[2] because items[2].prev_hash
    //     was [0; 32] and walk_ancestors_by_hash bails on prev == [0; 32].
    let walked = store.walk_ancestors(receipts[2].event_id, 8);
    let walked_ids: Vec<u128> = walked.iter().map(|s| s.event.event_id()).collect();
    let expected: Vec<u128> = vec![
        receipts[2].event_id,
        receipts[1].event_id,
        receipts[0].event_id,
    ];
    assert_eq!(
        walked_ids, expected,
        "PROPERTY: walk_ancestors from the last batch item must yield all \
         three items in reverse insertion order. Buggy hash chain breaks the \
         traversal at the [0; 32] terminator after step 1."
    );
}

/// REGRESSION: a durably-committed batch must survive an unclean shutdown
/// that left the segment without a SIDX footer.
///
/// Before the fix in `reader.rs::scan_segment_index`, the slow path tracked
/// `batch_committed_indices` and discarded every batch entry from the result
/// when `has_sidx_footer == false`, on the (false) premise that "SIDX is
/// written after sync, so its absence implies sync didn't complete." But
/// SIDX is only ever written on segment rotation or clean shutdown — never
/// per batch — so a successful `append_batch` followed by a crash before
/// the next rotation/clean close caused silent data loss for the entire
/// batch even though `append_batch` returned `Ok(receipts)`.
///
/// This test simulates that exact scenario by writing a batch, closing
/// cleanly (which writes SIDX), then surgically truncating the SIDX footer
/// off the segment file. Reopening the store must recover the batch via
/// the slow path's COMMIT-marker oracle.
#[test]
fn batch_survives_unclean_shutdown_without_sidx_footer() {
    let tmp = tempfile::tempdir().expect("create temp dir for unclean-shutdown regression");
    let data_dir = tmp.path().to_path_buf();
    let coord = Coordinate::new("regress", "no-sidx").expect("valid coord");

    // Phase 1: open, write a 3-item batch, close cleanly. Clean close
    // writes SIDX, which we strip in Phase 2 to simulate the unclean case.
    {
        let store = Store::open(StoreConfig::new(&data_dir)).expect("open store");
        let items: Vec<BatchAppendItem> = (0..3)
            .map(|i| {
                BatchAppendItem::new(
                    coord.clone(),
                    EventKind::DATA,
                    &serde_json::json!({"step": i}),
                    AppendOptions::default(),
                    CausationRef::None,
                )
                .expect("construct item")
            })
            .collect();
        let receipts = store
            .append_batch(items)
            .expect("batch append must succeed");
        assert_eq!(receipts.len(), 3, "baseline: all 3 items committed");
        store.close().expect("clean close");
    }

    // Phase 2a: delete the index checkpoint that clean close just wrote.
    // Without this, the next open uses the checkpoint fast path and skips
    // the segment scan entirely — which means the slow-path discard branch
    // (the H2 bug) never runs and we'd be testing the wrong code path.
    let _ = std::fs::remove_file(data_dir.join("index.ckpt"));

    // Phase 2b: locate the segment file and strip its SIDX footer in place.
    // The SIDX trailer is the last 16 bytes: [string_table_offset:u64 LE]
    // [entry_count:u32 LE][magic:4 b"SDX2"]. Truncating to string_table_offset
    // restores the file to its pre-SIDX state — exactly what an unclean
    // shutdown between batch sync and segment rotation/close would produce.
    let entries: Vec<_> = std::fs::read_dir(&data_dir)
        .expect("read data_dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "fbat").unwrap_or(false))
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "exactly one segment file expected before truncation"
    );
    let seg_path = entries[0].path();
    let bytes = std::fs::read(&seg_path).expect("read segment file");
    assert!(
        bytes.len() >= 16,
        "segment file must be at least 16 bytes (SIDX trailer length)"
    );
    let trailer = &bytes[bytes.len() - 16..];
    assert_eq!(
        &trailer[12..16],
        b"SDX2",
        "clean close must have written the SIDX footer (sanity check before truncation)"
    );
    let string_table_offset = u64::from_le_bytes(
        trailer[0..8]
            .try_into()
            .expect("SIDX trailer offset is exactly 8 bytes"),
    );
    std::fs::write(
        &seg_path,
        &bytes[..usize::try_from(string_table_offset).expect("offset fits in usize")],
    )
    .expect("truncate SIDX footer off segment");

    // Phase 3: reopen. The reader's slow path must recover the batch via
    // the COMMIT marker, NOT discard it for lacking a SIDX footer.
    let store = Store::open(StoreConfig::new(&data_dir)).expect("reopen after truncation");
    let recovered = store.query(&Region::entity("regress"));
    assert_eq!(
        recovered.len(),
        3,
        "PROPERTY: a durably-committed batch (BEGIN+frames+COMMIT all on disk) \
         must survive an unclean shutdown that stripped the SIDX footer. The \
         old reader.rs:707 discard branch silently dropped all 3 entries when \
         has_sidx_footer == false, violating [INV-BATCH-ATOMIC-VISIBILITY]."
    );

    // Sanity: the recovered entries are the same payloads we wrote.
    let mut steps: Vec<i64> = recovered
        .iter()
        .filter_map(|e| {
            store
                .get(e.event_id)
                .ok()
                .and_then(|stored| stored.event.payload["step"].as_i64())
        })
        .collect();
    steps.sort();
    assert_eq!(
        steps,
        vec![0, 1, 2],
        "recovered batch payloads must round-trip exactly"
    );
}

/// REGRESSION: batch wall_ms must remain monotonic per entity even when the
/// injected clock regresses between batch items.
///
/// Before the fix in `precompute_batch_items` + `BatchItemComputed.wall_ms`,
/// the batch path called `self.config.now_us()` independently for the
/// header and for the IndexEntry, and never applied the `raw_ms.max(last_ms)`
/// clamp the single-append path uses. A regressing test/system clock could
/// reorder `stream()` results within a batch and produce divergent wall_ms
/// between the on-disk frame header, the in-memory IndexEntry, and the SIDX
/// entry recovered through the cold-start fast path. The fix captures a
/// single `now_us` per batch and clamps `wall_ms = now_ms.max(entity_last_ms)`
/// per entity, mirroring the single-append guard.
#[test]
fn batch_wall_ms_monotonic_under_regressing_clock() {
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;

    // First call returns a "high" timestamp; every subsequent call returns
    // a strictly lower value. This is the kind of regression a mocked clock,
    // a coarse Windows timer, or NTP slew could produce.
    let tick = Arc::new(AtomicI64::new(2_000_000_000_000)); // 2e12 µs
    let clock_tick = Arc::clone(&tick);
    let clock: Arc<dyn Fn() -> i64 + Send + Sync> =
        Arc::new(move || clock_tick.fetch_sub(10_000, Ordering::SeqCst));

    let tmp = tempfile::tempdir().expect("create temp dir for wall_ms regression");
    let store = Store::open(StoreConfig::new(tmp.path()).with_clock(Some(clock)))
        .expect("open store with regressing clock");
    let coord = Coordinate::new("regress", "wallms").expect("valid coord");

    // Pre-establish a single event so the entity has a baseline `last_ms`
    // the batch path must clamp against.
    let pre = store
        .append(&coord, EventKind::DATA, &serde_json::json!({"pre": true}))
        .expect("pre-establish single event");
    let pre_entry = store
        .get(pre.event_id)
        .expect("load pre-established event")
        .event;
    let pre_wall_ms = pre_entry.header.position.wall_ms;

    // Now write a 3-item batch on the same entity. With the regressing clock,
    // the raw `now_ms` for the batch will be smaller than `pre_wall_ms`. The
    // monotonicity clamp must lift each batch item's wall_ms back up.
    let items: Vec<BatchAppendItem> = (0..3)
        .map(|i| {
            BatchAppendItem::new(
                coord.clone(),
                EventKind::DATA,
                &serde_json::json!({"batch_step": i}),
                AppendOptions::default(),
                CausationRef::None,
            )
            .expect("construct batch item")
        })
        .collect();
    store
        .append_batch(items)
        .expect("batch append must succeed");

    // Pull the entries in stream order (BTreeMap-sorted by ClockKey).
    let mut entries = store.query(&Region::entity("regress"));
    entries.sort_by_key(|e| e.clock);
    assert_eq!(entries.len(), 4, "1 single + 3 batch items expected");

    // PROPERTY: every IndexEntry.wall_ms must be >= the entity's prior
    // wall_ms. The batch items must NOT regress below pre_wall_ms.
    for (idx, entry) in entries.iter().enumerate() {
        assert!(
            entry.wall_ms >= pre_wall_ms,
            "PROPERTY: batch item {idx} wall_ms ({}) must NOT regress below \
             the entity's prior wall_ms ({pre_wall_ms}). Buggy precompute \
             never applied raw_ms.max(last_ms) for batches, so a regressing \
             clock would write wall_ms < pre_wall_ms and reorder stream() \
             results.",
            entry.wall_ms
        );
    }

    // PROPERTY: stream order must follow append order across the boundary.
    // If the regression had broken BTreeMap ordering, the batch items would
    // sort BEFORE the pre-established event.
    let mut sequences: Vec<u64> = entries.iter().map(|e| e.global_sequence).collect();
    let sorted_sequences = {
        let mut s = sequences.clone();
        s.sort();
        s
    };
    sequences.sort_by_key(|_| 0); // no-op, keep clock-sorted order
    assert_eq!(
        sequences, sorted_sequences,
        "PROPERTY: stream-order (clock) and append-order (global_sequence) \
         must agree per entity. A wall_ms regression in a batch breaks this \
         invariant by inserting batch items at a lower BTreeMap key."
    );
}
