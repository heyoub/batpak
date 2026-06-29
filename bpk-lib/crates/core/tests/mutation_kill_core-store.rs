//! Diff-scoped mutation-kill tests for the core store surface.
//!
//! Each test pins the exact observable behavior of a public-API path so that a
//! specific surviving mutant (identified by the diff-scoped mutation gate) is
//! caught: a test here fails iff that mutation is applied.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use batpak::event::StoredEvent;
use batpak::id::{EntityIdType, IdempotencyKey};
use batpak_testkit::prelude::*;
use batpak_testkit::small_store::small_segment_store;

// ─── reactor_typed::spawn_reactor_lossy guard ────────────────────────────────

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 11, type_id = 1)]
struct LossyTrigger {
    n: u64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 11, type_id = 2)]
struct LossyReaction {
    doubled: u64,
}

/// Lossy reactor that emits exactly one reaction per trigger — producing a
/// NON-empty `ReactionBatch` that the loop must flush.
struct DoublingReactor;

impl TypedReactive<LossyTrigger> for DoublingReactor {
    type Error = std::convert::Infallible;
    fn react(
        &mut self,
        event: &StoredEvent<LossyTrigger>,
        out: &mut ReactionBatch,
        _witness: Option<&batpak::store::AtLeastOnce>,
    ) -> Result<(), Self::Error> {
        let coord = Coordinate::new("entity:lossy-reaction", "scope:mutation")
            .expect("reaction coordinate");
        out.push_typed(
            coord,
            &LossyReaction {
                doubled: event.event.payload.n.saturating_mul(2),
            },
            CausationRef::None,
        )
        .expect("push reaction");
        Ok(())
    }
}

fn wait_for<F: Fn() -> bool>(cond: F, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    cond()
}

/// The lossy reactor loop guard is `Ok(()) if !batch.is_empty()`. Both the
/// `delete !` and `replace guard with false` mutants stop a NON-empty batch from
/// ever flushing, so the emitted reaction would never land. This asserts the
/// reaction IS persisted, killing both.
#[test]
fn lossy_reactor_flushes_a_non_empty_reaction_batch() {
    let (_dir, store) = small_segment_store().expect("small segment store");
    let store = Arc::new(store);

    let handle: TypedReactorHandle<std::convert::Infallible> = store
        .react_loop_typed::<LossyTrigger, _>(
            &Region::all(),
            ReactorConfig {
                batch_size: 1,
                idle_sleep: Duration::from_millis(5),
                restart_policy: RestartPolicy::Once,
                checkpoint_id: None,
                canal: ReactorCanal::LossySubscription,
            },
            DoublingReactor,
        )
        .expect("spawn lossy reactor");

    let source = Coordinate::new("entity:lossy-source", "scope:mutation").expect("source coord");
    let _ = store
        .append_typed(&source, &LossyTrigger { n: 21 })
        .expect("append trigger");

    let landed = wait_for(
        || store.by_fact_typed::<LossyReaction>().len() == 1,
        Duration::from_secs(3),
    );
    assert!(
        landed,
        "the lossy reactor must flush its non-empty reaction batch (the emitted \
         LossyReaction must be persisted)"
    );

    handle.stop_and_join().expect("clean stop and join");
}

// ─── ReadOnly writer_queue_len → None (capacity reported as 0) ────────────────

/// `<ReadOnly as StoreState>::writer_queue_len` returns `None`, which the
/// diagnostics path maps to a writer-pressure capacity of 0. The `Some(0)` /
/// `Some(1)` mutants would instead report the configured channel capacity.
#[test]
fn read_only_store_reports_zero_writer_capacity() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Seed the directory so it can be opened read-only.
    {
        let store = Store::open(StoreConfig::new(dir.path()).with_writer_channel_capacity(64))
            .expect("open seed store");
        let coord = Coordinate::new("entity:ro", "scope:mutation").expect("coord");
        let _ = store
            .append(&coord, EventKind::DATA, &serde_json::json!({"v": 1}))
            .expect("append");
        store.close().expect("close");
    }

    let ro = Store::open_read_only(StoreConfig::new(dir.path()).with_writer_channel_capacity(64))
        .expect("open read-only");
    let pressure = ro.diagnostics().writer_pressure;
    assert_eq!(
        pressure.capacity, 0,
        "a read-only store has no writer mailbox; writer_queue_len() must be None so the \
         reported capacity is 0 (a Some(_) mutant would surface the configured capacity)"
    );
    assert_eq!(
        pressure.queue_len, 0,
        "a read-only store reports no queued writer commands"
    );
}

// ─── Closed writer_queue_len → None (the marker is directly observable) ───────

/// `Closed` is a public typestate ZST. No `Store<Closed>` is ever constructed,
/// but its `StoreState::writer_queue_len` is callable on the bare marker, so the
/// impl is reachable and observable — the round-2 "unreachable, never observed"
/// equivalence claim was false. A closed store has no writer mailbox, so it must
/// report `None`; the `Some(0)`/`Some(1)` constant mutants would fabricate a
/// nonexistent writer queue.
#[test]
fn closed_state_reports_no_writer_queue() {
    use batpak::store::{Closed, StoreState};
    assert_eq!(
        Closed.writer_queue_len(),
        None,
        "a cleanly-closed store owns no writer, so writer_queue_len() must be None; \
         a Some(_) mutant would fabricate a nonexistent writer mailbox"
    );
}

// ─── batch idempotency recording ──────────────────────────────────────────────

const IDEM_KIND: EventKind = EventKind::custom(0xB, 5);

/// `WriterCore::record_batch_idempotency` records the DURABLE dedup entry for
/// each keyed batch item — the only authority that survives retention
/// compaction once the underlying event frame is evicted from the live index.
/// The `-> ()` mutant drops that side effect, so after compaction evicts the
/// keyed batch event, a replay of the same key would re-append a duplicate
/// instead of being a no-op.
#[test]
fn keyed_batch_idempotency_survives_retention_eviction() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Checkpoint/mmap off + tiny segments so compaction has sealed inputs and
    // the durable idemp sidecar (not the live frame) is the dedup authority.
    let config = StoreConfig::new(dir.path())
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false)
        .with_segment_max_bytes(512)
        .with_sync_every_n_events(1);
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("entity:batch-idem", "scope:mutation").expect("coord");
    let key = 0x0BAD_F00D_0BAD_F00D_0BAD_F00D_0BAD_F00Du128;

    // Commit the keyed item through the BATCH path (record_batch_idempotency).
    let item = BatchAppendItem::new(
        coord.clone(),
        IDEM_KIND,
        &serde_json::json!({"amount": 1}),
        AppendOptions::new().with_idempotency(IdempotencyKey::from(key)),
        CausationRef::None,
    )
    .expect("construct keyed batch item");
    let first = store
        .append_batch(vec![item])
        .expect("first keyed batch append");
    assert_eq!(first.len(), 1, "the keyed batch must commit one event");
    let first_seq = first[0].global_sequence;

    // Force segment rotation so retention compaction has sealed inputs to evict.
    for i in 0..8 {
        let _ = store
            .append(&coord, IDEM_KIND, &serde_json::json!({ "filler": i }))
            .expect("append filler event");
    }

    // Retention compaction that evicts every IDEM_KIND user event (keeps only
    // batch/system markers), dropping the keyed event frame from the live index.
    let strategy = CompactionStrategy::Retention(Box::new(|stored| {
        stored.event.header.event_kind != IDEM_KIND
    }));
    let _ = store
        .compact(&CompactionConfig {
            strategy,
            min_segments: 1,
        })
        .expect("retention compaction");

    // The keyed event frame is gone from the live index...
    let live_keyed = store
        .query(&Region::all())
        .into_iter()
        .filter(|e| e.event_id().as_u128() == key)
        .count();
    assert_eq!(
        live_keyed, 0,
        "PRECONDITION: retention compaction evicted the keyed batch event frame"
    );

    // ...yet a replay of the same key is still a no-op via the DURABLE idemp
    // entry recorded by record_batch_idempotency. The -> () mutant never wrote
    // that entry, so the replay would re-append a duplicate with a new sequence.
    let replay = store
        .append_with_options(
            &coord,
            IDEM_KIND,
            &serde_json::json!({"amount": 1}),
            AppendOptions::new().with_idempotency(IdempotencyKey::from(key)),
        )
        .expect("replay keyed append");
    assert_eq!(
        replay.global_sequence, first_seq,
        "INV-IDEMPOTENCY-DURABLE-WINDOW: a keyed batch retry after eviction must return the \
         original sequence (the -> () mutant loses the durable entry and re-appends)"
    );

    store.close().expect("close");
}

// ─── reactor_typed lossy: explicit witness/seen sanity (anchors the harness) ──

/// A small, lossy-canal sanity check that the reactor actually observes events,
/// so the flush-path assertion above is exercised on a live worker.
#[test]
fn lossy_reactor_observes_each_trigger() {
    let (_dir, store) = small_segment_store().expect("small segment store");
    let store = Arc::new(store);
    let seen = Arc::new(AtomicUsize::new(0));

    struct Counting {
        seen: Arc<AtomicUsize>,
    }
    impl TypedReactive<LossyTrigger> for Counting {
        type Error = std::convert::Infallible;
        fn react(
            &mut self,
            _event: &StoredEvent<LossyTrigger>,
            _out: &mut ReactionBatch,
            _witness: Option<&batpak::store::AtLeastOnce>,
        ) -> Result<(), Self::Error> {
            self.seen.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    let handle: TypedReactorHandle<std::convert::Infallible> = store
        .react_loop_typed::<LossyTrigger, _>(
            &Region::all(),
            ReactorConfig {
                batch_size: 1,
                idle_sleep: Duration::from_millis(5),
                restart_policy: RestartPolicy::Once,
                checkpoint_id: None,
                canal: ReactorCanal::LossySubscription,
            },
            Counting {
                seen: Arc::clone(&seen),
            },
        )
        .expect("spawn lossy reactor");

    let source = Coordinate::new("entity:lossy-source-2", "scope:mutation").expect("coord");
    let _ = store
        .append_typed(&source, &LossyTrigger { n: 7 })
        .expect("append trigger");

    assert!(
        wait_for(|| seen.load(Ordering::SeqCst) == 1, Duration::from_secs(3)),
        "lossy reactor must observe the trigger"
    );

    handle.stop_and_join().expect("clean stop and join");
}

// ─── writer maybe_rotate_segment: new_segment = segment_id + 1 ─────────────────

/// `maybe_rotate_segment` computes `new_segment = self.segment_id + 1` and feeds
/// it to the rotation fault-injection points. Pinning the injector to
/// `new_segment == old + 1` makes the arithmetic load-bearing: the `+ -> *`
/// mutant yields the old id (no match → rotation succeeds), and the `+ -> -`
/// mutant underflows. Either way the rotating append no longer fails with the
/// injected `FaultInjected`, which this test requires.
#[cfg(feature = "dangerous-test-hooks")]
#[test]
fn rotation_fault_keyed_on_new_segment_is_old_plus_one() {
    use batpak::store::fault::{CountdownAction, CountdownInjector, InjectionPoint};

    let dir = tempfile::tempdir().expect("tempdir");
    let coord = Coordinate::new("entity:rot", "scope:mutation").expect("coord");

    // Phase 1: fill segment past the rotation threshold so the NEXT append rotates.
    {
        let store = Store::open(StoreConfig::new(dir.path()).with_segment_max_bytes(1024))
            .expect("open seed store");
        let _ = store
            .append(
                &coord,
                EventKind::DATA,
                &serde_json::json!({"phase": 1, "pad": "x".repeat(900)}),
            )
            .expect("append pre-rotation event");
        store.close().expect("close seed");
    }

    // Phase 2: reopen with a fault injector that fires ONLY when the rotation's
    // new_segment == old_segment + 1. On reopen the active segment is the latest
    // existing id `S`; the first rotation must produce new_segment == S + 1.
    let injector = Arc::new(
        CountdownInjector::new(1, CountdownAction::Fail("rotation new_segment == old+1"))
            .with_filter(|p| {
                matches!(
                    p,
                    InjectionPoint::SegmentRotationCreate { old_segment, new_segment }
                        | InjectionPoint::SegmentRotation { old_segment, new_segment }
                    if *new_segment == old_segment.saturating_add(1)
                )
            }),
    );
    let store = Store::open(
        StoreConfig::new(dir.path())
            .with_segment_max_bytes(1024)
            .with_fault_injector(Some(injector)),
    )
    .expect("open fault-injected store");

    let result = store.append(
        &coord,
        EventKind::DATA,
        &serde_json::json!({"phase": 2, "pad": "y".repeat(400)}),
    );
    assert!(
        matches!(result, Err(StoreError::FaultInjected(_))),
        "the rotating append must fault when new_segment == old + 1; got {result:?} \
         (a `+ -> *` mutant yields new_segment == old and never matches; a `+ -> -` \
         mutant underflows)"
    );
}

// ─── cold_start inject_scan_frame side effect ─────────────────────────────────

/// `inject_scan_frame` advances the frame counter AND fires the per-frame
/// `ReadAt` / `ColdStartScanFrame` injection points. The `-> Ok(())` mutant
/// removes the whole body, so neither injection point is ever emitted.
///
/// To reach the per-frame scan deterministically the injector also fails the
/// `IndexFooterDecode` so the parallel SIDX fast-path is abandoned in favour of
/// the sequential frame scan (the only path that runs `inject_scan_frame`). With
/// real code the scan then fires `ReadAt` and the open aborts; the `-> Ok(())`
/// mutant emits nothing during the scan, so the open would wrongly succeed.
#[cfg(feature = "dangerous-test-hooks")]
#[test]
fn cold_start_scan_frame_injection_fires_during_rebuild() {
    use batpak::store::fault::{CountdownAction, CountdownInjector, InjectionPoint};

    let dir = tempfile::tempdir().expect("tempdir");
    let coord = Coordinate::new("entity:scan", "scope:mutation").expect("coord");

    // Seed a single (un-rotated) segment with several events so cold start must
    // frame-scan the active segment. mmap + checkpoint are OFF so the only open
    // path is the segment scan that runs `inject_scan_frame`.
    {
        let store = Store::open(
            StoreConfig::new(dir.path())
                .with_enable_mmap_index(false)
                .with_enable_checkpoint(false)
                .with_segment_max_bytes(8 * 1024 * 1024),
        )
        .expect("open seed store");
        for n in 0..16u32 {
            let _ = store
                .append(&coord, EventKind::DATA, &serde_json::json!({"n": n}))
                .expect("append seed event");
        }
        store.close().expect("close seed");
    }

    // Fail the SIDX footer decode (forces the frame-scan fallback) and the
    // per-frame scan points (emitted only by inject_scan_frame). CountdownInjector
    // fires on every matching point once armed, so the footer decode triggers the
    // fallback and the scan then triggers the abort.
    let injector = Arc::new(
        CountdownInjector::new(1, CountdownAction::Fail("scan-frame fault")).with_filter(|p| {
            matches!(
                p,
                InjectionPoint::IndexFooterDecode { .. }
                    | InjectionPoint::ColdStartScanFrame { .. }
                    | InjectionPoint::ReadAt { .. }
            )
        }),
    );
    let result = Store::open(
        StoreConfig::new(dir.path())
            .with_enable_mmap_index(false)
            .with_enable_checkpoint(false)
            .with_segment_max_bytes(8 * 1024 * 1024)
            .with_fault_injector(Some(injector)),
    );
    assert!(
        result.is_err(),
        "a fault keyed to the per-frame scan injection points must abort cold start; \
         the -> Ok(()) mutant skips emitting them so open would wrongly succeed"
    );
}

// ─── lifecycle wait_for_emitted blocks on an unmet emitted frontier ───────────

/// `Store::wait_for_emitted` must actually wait on the emitted watermark. The
/// `-> Ok(())` mutant returns success without waiting, so a point the emitted
/// frontier can never reach within the timeout would wrongly report Ok.
#[test]
fn wait_for_emitted_times_out_when_the_emitted_frontier_is_behind() {
    use batpak::store::HlcPoint;

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");

    // A point far beyond any sequence the emitted frontier of a fresh, reactor-less
    // store could reach: the real wait must exhaust its (short) timeout.
    let unreachable = HlcPoint {
        wall_ms: u64::MAX,
        global_sequence: u64::MAX,
    };
    let result = store.wait_for_emitted(unreachable, Duration::from_millis(50));
    assert!(
        matches!(result, Err(StoreError::WaitTimeout { .. })),
        "wait_for_emitted must time out waiting for an unreachable emitted point (the \
         -> Ok(()) mutant returns Ok without waiting); got {result:?}"
    );

    store.close().expect("close");
}
