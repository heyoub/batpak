use super::*;
use crate::event::{Event, EventKind};
use crate::store::index::columnar::CachedProjectionSlot;
use crate::store::index::ProjectionReplayPlan;
use crate::store::{IndexTopology, Open, StoreConfig};
use std::error::Error;
use tempfile::TempDir;

type TestResult = Result<(), Box<dyn Error>>;

#[derive(Default, Debug, serde::Serialize, serde::Deserialize)]
struct Counter;

impl EventSourced for Counter {
    type Input = crate::event::JsonValueInput;

    fn apply_event(&mut self, event: &Event<serde_json::Value>) {
        std::hint::black_box(event.event_kind());
    }

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        (!events.is_empty()).then_some(Self)
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        static KINDS: [EventKind; 1] = [EventKind::custom(0xF, 1)];
        &KINDS
    }
}

struct FailingProjection;

struct FixedMonoClock {
    mono_ns: i64,
}

impl crate::store::Clock for FixedMonoClock {
    fn now_us(&self) -> i64 {
        0
    }

    fn now_wall_ns(&self) -> i64 {
        0
    }

    fn now_mono_ns(&self) -> i64 {
        self.mono_ns
    }

    fn process_boot_ns(&self) -> u64 {
        0
    }
}

impl serde::Serialize for FailingProjection {
    fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        Err(serde::ser::Error::custom(
            "intentional projection serialization failure",
        ))
    }
}

#[test]
fn elapsed_us_converts_nanoseconds_by_division() {
    let clock = FixedMonoClock { mono_ns: 3_456 };

    assert_eq!(
        elapsed_us(&clock, 123),
        3,
        "PROPERTY: projection flow timing must convert elapsed nanoseconds to microseconds by division.\n\
         Investigate: src/store/projection/flow.rs elapsed_us.\n\
         Common causes: using remainder/modulo instead of / 1_000."
    );
}

fn append_counter_event(store: &Store<Open>, entity: &str) -> TestResult {
    let coord = crate::coordinate::Coordinate::new(entity, "scope:test")?;
    store.append(
        &coord,
        Counter::relevant_event_kinds()[0],
        &serde_json::json!({"n": 1}),
    )?;
    Ok(())
}

fn replay_context_for<State>(
    store: &Store<State>,
    entity: &str,
    type_id: std::any::TypeId,
    cache_key: Vec<u8>,
) -> ReplayContext {
    let plan = store
        .index
        .projection_replay_plan(entity, Counter::relevant_event_kinds())
        .unwrap_or_else(|| ProjectionReplayPlan {
            watermark: 1,
            generation: 1,
            items: vec![],
        });
    ReplayContext {
        watermark: plan.watermark,
        cached_at_us: store.runtime.cache_now_us(),
        cached_at_mono_ns: store.runtime.now_mono_ns(),
        process_boot_ns: store.runtime.process_boot_ns(),
        type_id,
        cache_key,
        plan,
    }
}

#[test]
fn projection_replay_plan_matches_legacy_stream_filtering() -> TestResult {
    let dir = TempDir::new()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let coord = crate::coordinate::Coordinate::new("entity:proj", "scope:test")?;
    let kept = EventKind::custom(0xF, 1);
    let skipped = EventKind::custom(0xF, 2);

    for (kind, payload) in [
        (kept, serde_json::json!({"n": 1})),
        (skipped, serde_json::json!({"n": 2})),
        (kept, serde_json::json!({"n": 3})),
    ] {
        store.append(&coord, kind, &payload)?;
    }

    let Some(plan) = store
        .index
        .projection_replay_plan("entity:proj", Counter::relevant_event_kinds())
    else {
        return Err(std::io::Error::other("expected projection replay plan").into());
    };

    let legacy_entries = store.index.stream("entity:proj");
    let legacy_entries: Vec<_> = legacy_entries
        .into_iter()
        .filter(|entry| Counter::relevant_event_kinds().contains(&entry.kind))
        .collect();
    let legacy_items: Vec<_> = legacy_entries
        .iter()
        .map(|entry| (entry.global_sequence, entry.disk_pos))
        .collect();
    let planned_items: Vec<_> = plan
        .items
        .iter()
        .map(|item| (item.global_sequence, item.disk_pos))
        .collect();
    let Some(legacy_watermark) = legacy_entries.last().map(|entry| entry.global_sequence) else {
        return Err(std::io::Error::other("expected legacy filtered entries").into());
    };

    assert_eq!(plan.watermark, legacy_watermark);
    assert_eq!(
        plan.generation,
        store.index.entity_generation("entity:proj").unwrap_or(0)
    );
    assert_eq!(planned_items, legacy_items);

    store.close()?;
    Ok(())
}

#[test]
fn cache_store_reports_serialization_failure_without_touching_index() -> TestResult {
    let dir = TempDir::new()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let entity = "entity:serialize-fail";
    append_counter_event(&store, entity)?;
    let freshness = Freshness::Consistent;
    let replay = replay_context_for(
        &store,
        entity,
        std::any::TypeId::of::<FailingProjection>(),
        b"serialize-failure-cache-key".to_vec(),
    );
    let execution = replay_execution(entity, &freshness, &replay, store.runtime.now_mono_ns());

    let outcome = store_projection_value(&store, &execution, &FailingProjection);

    assert_eq!(outcome, ProjectionCacheStoreOutcome::SerializationFailed);
    assert!(
        store
            .index
            .cached_projection(entity, std::any::TypeId::of::<FailingProjection>())
            .is_none(),
        "PROPERTY: serialization failure must not populate the group-local projection cache"
    );

    store.close()?;
    Ok(())
}

#[test]
fn cache_store_reports_index_store_success_and_unsupported_topology() -> TestResult {
    let supported_dir = TempDir::new()?;
    let supported = Store::open(
        StoreConfig::new(supported_dir.path()).with_index_topology(IndexTopology::entity_local()),
    )?;
    let entity = "entity:index-store-supported";
    append_counter_event(&supported, entity)?;
    let freshness = Freshness::Consistent;
    let replay = replay_context_for(
        &supported,
        entity,
        std::any::TypeId::of::<Counter>(),
        projection_cache_key::<Counter>(entity),
    );
    let execution = replay_execution(entity, &freshness, &replay, supported.runtime.now_mono_ns());

    let outcome = store_projection_value(&supported, &execution, &Counter);

    assert_eq!(
        outcome,
        ProjectionCacheStoreOutcome::Stored {
            external: ProjectionExternalCacheStoreOutcome::Stored,
            index: ProjectionIndexCacheStoreOutcome::Stored,
        }
    );
    assert!(
        supported
            .index
            .cached_projection(entity, std::any::TypeId::of::<Counter>())
            .is_some(),
        "PROPERTY: a true index-side store return must leave a group-local slot"
    );
    supported.close()?;

    let unsupported_dir = TempDir::new()?;
    let unsupported = Store::open(
        StoreConfig::new(unsupported_dir.path()).with_index_topology(IndexTopology::scan()),
    )?;
    let entity = "entity:index-store-unsupported";
    append_counter_event(&unsupported, entity)?;
    let replay = replay_context_for(
        &unsupported,
        entity,
        std::any::TypeId::of::<Counter>(),
        projection_cache_key::<Counter>(entity),
    );
    let execution = replay_execution(
        entity,
        &freshness,
        &replay,
        unsupported.runtime.now_mono_ns(),
    );

    let outcome = store_projection_value(&unsupported, &execution, &Counter);

    assert_eq!(
        outcome,
        ProjectionCacheStoreOutcome::Stored {
            external: ProjectionExternalCacheStoreOutcome::Stored,
            index: ProjectionIndexCacheStoreOutcome::UnsupportedTopology,
        }
    );
    assert!(
        unsupported
            .index
            .cached_projection(entity, std::any::TypeId::of::<Counter>())
            .is_none(),
        "PROPERTY: unsupported topology must be reported instead of silently ignored"
    );
    unsupported.close()?;
    Ok(())
}

#[test]
fn projection_timings_cold_path_breakdown() -> TestResult {
    let dir = TempDir::new()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let coord = crate::coordinate::Coordinate::new("entity:timed", "scope:test")?;
    let kind = EventKind::custom(0xF, 1);
    for i in 0..1_000u32 {
        store.append(&coord, kind, &serde_json::json!({"i": i}))?;
    }

    // Close and reopen to get a true cold path
    store.close()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;

    let mut timings = ProjectionTimings::default();
    let result: Option<Counter> =
        project_timed(&store, "entity:timed", &Freshness::Consistent, &mut timings)?;
    assert!(result.is_some(), "projection must produce a value");

    let accounted = timings.plan_build_us
        + timings.cache_key_build_us
        + timings.group_local_lookup_us
        + timings.prefetch_us
        + timings.external_cache_probe_us
        + timings.disk_read_us
        + timings.event_extract_us
        + timings.replay_fold_us
        + timings.cache_store_us;

    assert!(timings.total_us > 0, "total must be positive");
    assert!(
        accounted <= timings.total_us,
        "phase timings must not exceed total"
    );
    store.close()?;
    Ok(())
}

#[test]
fn compute_strategy_exhaustive() {
    let slot = CachedProjectionSlot {
        bytes: vec![],
        watermark: 42,
        generation: 1,
    };

    // Slot present + fresh -> GroupLocalHit
    assert_eq!(
        compute_strategy(Some(&slot), true, false, false, false),
        ProjectionStrategy::GroupLocalHit,
    );
    assert_eq!(
        compute_strategy(Some(&slot), true, true, true, true),
        ProjectionStrategy::GroupLocalHit,
    );

    // Slot present + stale + incremental supported + enabled -> GroupLocalIncremental
    assert_eq!(
        compute_strategy(Some(&slot), false, true, true, false),
        ProjectionStrategy::GroupLocalIncremental,
    );
    assert_eq!(
        compute_strategy(Some(&slot), false, true, true, true),
        ProjectionStrategy::GroupLocalIncremental,
    );

    // Slot present + stale + incremental disabled -> falls through to cache check
    assert_eq!(
        compute_strategy(Some(&slot), false, true, false, false),
        ProjectionStrategy::ExternalCacheThenReplay,
    );
    assert_eq!(
        compute_strategy(Some(&slot), false, true, false, true),
        ProjectionStrategy::DirectReplay,
    );

    // Slot present + stale + incremental NOT supported -> falls through to cache check
    assert_eq!(
        compute_strategy(Some(&slot), false, false, false, false),
        ProjectionStrategy::ExternalCacheThenReplay,
    );
    assert_eq!(
        compute_strategy(Some(&slot), false, false, true, false),
        ProjectionStrategy::ExternalCacheThenReplay,
    );
    assert_eq!(
        compute_strategy(Some(&slot), false, false, false, true),
        ProjectionStrategy::DirectReplay,
    );

    // No slot + noop cache -> DirectReplay
    assert_eq!(
        compute_strategy(None, false, false, false, true),
        ProjectionStrategy::DirectReplay,
    );

    // No slot + real cache -> ExternalCacheThenReplay
    assert_eq!(
        compute_strategy(None, false, false, false, false),
        ProjectionStrategy::ExternalCacheThenReplay,
    );
    assert_eq!(
        compute_strategy(None, false, true, true, false),
        ProjectionStrategy::ExternalCacheThenReplay,
    );
}

#[test]
fn group_local_projection_freshness_is_typed() {
    let replay = ReplayContext {
        watermark: 42,
        cached_at_us: 0,
        cached_at_mono_ns: 0,
        process_boot_ns: 0,
        type_id: std::any::TypeId::of::<Counter>(),
        cache_key: b"freshness-key".to_vec(),
        plan: ProjectionReplayPlan {
            watermark: 42,
            generation: 7,
            items: vec![],
        },
    };
    let fresh = CachedProjectionSlot {
        bytes: vec![],
        watermark: 42,
        generation: 7,
    };
    let stale_watermark = CachedProjectionSlot {
        bytes: vec![],
        watermark: 41,
        generation: 7,
    };
    let stale_generation = CachedProjectionSlot {
        bytes: vec![],
        watermark: 42,
        generation: 6,
    };

    assert_eq!(
        group_local_projection_freshness(None, &replay, &Freshness::Consistent),
        GroupLocalProjectionFreshness::Missing
    );
    assert_eq!(
        group_local_projection_freshness(Some(&fresh), &replay, &Freshness::Consistent),
        GroupLocalProjectionFreshness::Fresh
    );
    assert_eq!(
        group_local_projection_freshness(Some(&stale_watermark), &replay, &Freshness::Consistent),
        GroupLocalProjectionFreshness::Stale
    );
    assert_eq!(
        group_local_projection_freshness(
            Some(&stale_generation),
            &replay,
            &Freshness::MaybeStale {
                max_stale_ms: 1_000,
            },
        ),
        GroupLocalProjectionFreshness::Stale
    );
}

#[test]
fn group_local_freshness_is_fresh_only_for_fresh_variant() {
    // Pins `is_fresh`: hardcoding it to `false` would force a re-project on
    // every genuinely-fresh cache slot, silently defeating the cache.
    assert!(GroupLocalProjectionFreshness::Fresh.is_fresh());
    assert!(!GroupLocalProjectionFreshness::Stale.is_fresh());
    assert!(!GroupLocalProjectionFreshness::Missing.is_fresh());
}

#[test]
fn incremental_projection_applies_events_after_cached_watermark() -> TestResult {
    use crate::coordinate::Coordinate;
    use crate::store::{Freshness, Store};

    // A projection that supports incremental apply: `from_events` and
    // `apply_event` agree (count == number of events folded).
    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct IncCounter {
        count: u32,
    }
    impl EventSourced for IncCounter {
        type Input = crate::event::JsonValueInput;
        fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
            Some(IncCounter {
                count: u32::try_from(events.len()).expect("test uses < 2^32 events"),
            })
        }
        fn apply_event(&mut self, _event: &Event<serde_json::Value>) {
            // R10: sentinel delta. Using +=100 (not +=1) makes the incremental
            // fold yield a value distinct from every alternative path, so the
            // exact-value assertion below becomes a true discriminator:
            //   incremental fold (cached 2 + one apply) -> 102
            //   full-replay fallback (from_events over 3 events) -> 3
            //   no-op apply (mutated/skipped incremental) -> 2
            // With +=1 the incremental (3) and full-replay (3) paths collide and
            // the test cannot tell them apart.
            self.count += 100;
        }
        fn relevant_event_kinds() -> &'static [EventKind] {
            &[]
        }
        fn supports_incremental_apply() -> bool {
            true
        }
    }

    let dir = TempDir::new()?;
    let config = StoreConfig::new(dir.path().join("data"))
        .with_sync_every_n_events(1)
        .with_incremental_projection(true);
    let store = Store::open_with_native_cache(config, dir.path().join("cache"))?;

    let coord = Coordinate::new("entity:inc", "scope:test").expect("coordinate");
    let kind = EventKind::custom(0xF, 1);
    store.append(&coord, kind, &serde_json::json!({ "x": 1 }))?;
    store.append(&coord, kind, &serde_json::json!({ "x": 2 }))?;

    // First project: cache miss → full replay → external cache populated at the
    // current watermark (count == 2).
    let first: Option<IncCounter> = store.project("entity:inc", &Freshness::Consistent)?;
    assert_eq!(first, Some(IncCounter { count: 2 }));

    // Commit one more event past the cached watermark.
    store.append(&coord, kind, &serde_json::json!({ "x": 3 }))?;

    // Second project: cache hit at the stale watermark + incremental enabled, so
    // `apply_incremental_events` must fold the single post-watermark event via
    // the sentinel `+=100` apply. The exact value is the discriminator:
    //   102 => incremental fold (cached 2 + one sentinel apply) — the correct path
    //     3 => full-replay fallback (from_events over 3 events) — wrong path taken
    //     2 => no-op apply / incremental branch skipped — stale cached count
    let second: Option<IncCounter> = store.project("entity:inc", &Freshness::Consistent)?;
    assert_eq!(
        second,
        Some(IncCounter { count: 102 }),
        "incremental apply must fold events committed after the cached watermark \
         (102=incremental fold, 3=full replay, 2=no-op apply)"
    );

    store.close()?;
    Ok(())
}

#[test]
fn external_cache_path_full_replays_for_non_incremental_type() -> TestResult {
    use crate::coordinate::Coordinate;
    use crate::store::{Freshness, Store};

    // A projection type that does NOT support incremental apply: `from_events`
    // counts events, but `apply_event` is a deliberate no-op (contract-legal
    // precisely because `supports_incremental_apply()` defaults to false). If
    // the incremental branch were ever (wrongly) taken for this type, the no-op
    // apply would leave the stale cached count untouched.
    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct NonIncCounter {
        count: u32,
    }
    impl EventSourced for NonIncCounter {
        type Input = crate::event::JsonValueInput;
        fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
            Some(NonIncCounter {
                count: u32::try_from(events.len()).expect("test uses < 2^32 events"),
            })
        }
        fn apply_event(&mut self, _event: &Event<serde_json::Value>) {
            // No-op: legal only because supports_incremental_apply() == false.
        }
        fn relevant_event_kinds() -> &'static [EventKind] {
            &[]
        }
        // supports_incremental_apply() defaults to false (not overridden).
    }

    // PROOF OF ROUTING through execute_external_cache_path:
    // On the second project() below, the first project() warmed a group-local
    // slot at W2, which on the (now stale) second call is present-but-stale.
    // `compute_strategy` is the single authority that picks the dispatch arm.
    // For a non-incremental type with incremental_projection enabled and a
    // non-noop external cache, the exact arguments production passes are:
    //   group_local_slot = Some(stale), is_group_local_fresh = false,
    //   supports_incremental = false, incremental_enabled = true,
    //   cache_is_noop = false
    // which routes to ExternalCacheThenReplay -> execute_external_cache_path
    // (NOT GroupLocalIncremental, because supports_incremental is false; NOT
    // DirectReplay, because the cache is not a no-op). Pin that here so the
    // end-to-end assertion below is provably exercising the col-61 guard inside
    // execute_external_cache_path.
    {
        let stale_slot = CachedProjectionSlot {
            bytes: vec![1],
            watermark: 2,
            generation: 1,
        };
        assert_eq!(
            compute_strategy(Some(&stale_slot), false, false, true, false),
            ProjectionStrategy::ExternalCacheThenReplay,
            "non-incremental + stale slot must route through execute_external_cache_path"
        );
    }

    let dir = TempDir::new()?;
    let config = StoreConfig::new(dir.path().join("data"))
        .with_sync_every_n_events(1)
        .with_incremental_projection(true);
    let store = Store::open_with_native_cache(config, dir.path().join("cache"))?;

    let coord = Coordinate::new("entity:noninc", "scope:test").expect("coordinate");
    let kind = EventKind::custom(0xF, 1);
    store.append(&coord, kind, &serde_json::json!({ "x": 1 }))?;
    store.append(&coord, kind, &serde_json::json!({ "x": 2 }))?;

    // First project: cache miss -> full replay -> external cache populated at W2
    // (count == 2), and the group-local slot is warmed at W2.
    let first: Option<NonIncCounter> = store.project("entity:noninc", &Freshness::Consistent)?;
    assert_eq!(first, Some(NonIncCounter { count: 2 }));

    // Commit one more event past the cached watermark (W3).
    store.append(&coord, kind, &serde_json::json!({ "x": 3 }))?;

    // Second project: the stale external row at W2 + a non-incremental type must
    // take FULL REPLAY (count == 3 from on-disk events).
    //   3 => full replay (correct: guard's `&& supports_incremental_apply()`
    //        short-circuits the incremental branch closed)
    //   2 => the :540 col-61 mutant (`... || incremental_projection`) wrongly
    //        enters the incremental branch and no-op-applies, leaving stale 2.
    let second: Option<NonIncCounter> = store.project("entity:noninc", &Freshness::Consistent)?;
    assert_eq!(
        second,
        Some(NonIncCounter { count: 3 }),
        "non-incremental type must full-replay (3); the :540:61 mutant returns stale 2"
    );

    store.close()?;
    Ok(())
}

#[test]
fn maybe_stale_external_cache_age_boundary_is_pinned() -> TestResult {
    // Pins the age-based freshness comparison inside execute_external_cache_path:
    //   age_us < (max_stale_ms as i64) * 1000
    // The external-cache MaybeStale branch serves the (stale) CACHED value when
    // is_fresh==true and full-replays the (newer) DISK value when is_fresh==false,
    // so the returned count discriminates the comparison exactly:
    //   fresh (age below threshold) -> cached count 2
    //   stale (age at/above threshold) -> full-replay count 3
    // This kills the flow/mod.rs:536 mutants:
    //   `< -> ==`, `< -> >`, `< -> <=`  (comparison operator)
    //   `* -> +`,  `* -> /`             (threshold scale: 1000*1000 vs 2000 vs 1)
    use crate::coordinate::Coordinate;
    use crate::store::{Freshness, Store};
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;

    // A non-incremental projection whose value equals the on-disk event count,
    // so the cached value (2) differs from the full-replay value (3) and the age
    // comparison's fresh/stale decision is observable in the returned count.
    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct AgeCounter {
        count: u32,
    }
    impl EventSourced for AgeCounter {
        type Input = crate::event::JsonValueInput;
        fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
            Some(AgeCounter {
                count: u32::try_from(events.len()).expect("test uses < 2^32 events"),
            })
        }
        fn apply_event(&mut self, _event: &Event<serde_json::Value>) {}
        fn relevant_event_kinds() -> &'static [EventKind] {
            static KINDS: [EventKind; 1] = [EventKind::custom(0xF, 1)];
            &KINDS
        }
        // supports_incremental_apply() defaults to false -> external-cache path.
    }

    // Clock the test fully controls (microseconds). cached_at_us is stamped from
    // this clock at ProjectionCache::put time; the age compare reads it too.
    let now_us = Arc::new(AtomicI64::new(0));
    let now_us_clock = Arc::clone(&now_us);

    let dir = TempDir::new()?;
    let config = StoreConfig::new(dir.path().join("data"))
        .with_sync_every_n_events(1)
        .with_clock_fn(move || now_us_clock.load(Ordering::SeqCst));
    let store = Store::open_with_native_cache(config, dir.path().join("cache"))?;

    let coord = Coordinate::new("entity:agecmp", "scope:test").expect("coordinate");
    let kind = Counter::relevant_event_kinds()[0];
    store.append(&coord, kind, &serde_json::json!({ "n": 1 }))?;
    store.append(&coord, kind, &serde_json::json!({ "n": 2 }))?;

    // First project warms the external cache at cached_at_us == 0 with count 2.
    // (Counter is non-incremental, so the second project routes through the
    // external-cache MaybeStale branch, not GroupLocalIncremental.)
    let max_stale_ms: u64 = 1_000; // threshold = 1_000 * 1_000 = 1_000_000 us
    let freshness = Freshness::MaybeStale { max_stale_ms };
    let first: Option<AgeCounter> = store.project("entity:agecmp", &freshness)?;
    assert_eq!(first, Some(AgeCounter { count: 2 }));

    // Disk advances past the cached watermark: a full replay now yields 3.
    store.append(&coord, kind, &serde_json::json!({ "n": 3 }))?;

    // age = 999_999 < 1_000_000 -> FRESH -> serve cached 2.
    // Kills `* -> +` (threshold 2000 -> stale -> 3) and `* -> /`
    // (threshold 1 -> stale -> 3); also kills `< -> >` (would be stale -> 3).
    now_us.store(999_999, Ordering::SeqCst);
    let just_under: Option<AgeCounter> = store.project("entity:agecmp", &freshness)?;
    assert_eq!(
        just_under,
        Some(AgeCounter { count: 2 }),
        "age 999_999 < 1_000_000 must be fresh (serve cached 2)"
    );

    // age = 1_000_000 == threshold -> real `<` is STALE -> full replay 3.
    // Kills `< -> <=` and `< -> ==` (both would treat the boundary as fresh -> 2).
    now_us.store(1_000_000, Ordering::SeqCst);
    let at_boundary: Option<AgeCounter> = store.project("entity:agecmp", &freshness)?;
    assert_eq!(
        at_boundary,
        Some(AgeCounter { count: 3 }),
        "age 1_000_000 == threshold must be stale under `<` (full replay 3)"
    );

    store.close()?;
    Ok(())
}

#[test]
fn external_cache_hit_observed_freshness_distinguishes_fresh_from_stale_allowed() -> TestResult {
    // Pins the watermark-equality branch at flow/mod.rs:607 inside
    // execute_external_cache_path:
    //   if meta.watermark == execution.replay.watermark { Fresh } else { StaleAllowed }
    // The `== -> !=` mutant flips BOTH arms, so asserting BOTH branches kills it.
    //
    // The decision is observable end-to-end via `project_run_evidence`, which
    // routes the internal `ProjectionObservedFreshness` through
    // `map_observed_freshness` onto `body.observed_freshness`
    // (a `ProjectionRunFreshnessStatus`). We assert on that field, not on a log.
    use crate::coordinate::Coordinate;
    use crate::store::{Freshness, ProjectionRunFreshnessStatus, Store};

    // Non-incremental projection (supports_incremental_apply() defaults to
    // false), so a present-but-stale group-local slot routes through
    // ExternalCacheThenReplay -> execute_external_cache_path rather than the
    // GroupLocalIncremental arm. value == on-disk event count.
    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct FreshnessCounter {
        count: u32,
    }
    impl EventSourced for FreshnessCounter {
        type Input = crate::event::JsonValueInput;
        fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
            Some(FreshnessCounter {
                count: u32::try_from(events.len()).expect("test uses < 2^32 events"),
            })
        }
        fn apply_event(&mut self, _event: &Event<serde_json::Value>) {}
        fn relevant_event_kinds() -> &'static [EventKind] {
            static KINDS: [EventKind; 1] = [EventKind::custom(0xF, 1)];
            &KINDS
        }
        // supports_incremental_apply() defaults to false -> external-cache path.
    }

    let dir = TempDir::new()?;
    let config = StoreConfig::new(dir.path().join("data")).with_sync_every_n_events(1);
    let store = Store::open_with_native_cache(config, dir.path().join("cache"))?;

    let coord = Coordinate::new("entity:freshobs", "scope:test").expect("coordinate");
    let kind = FreshnessCounter::relevant_event_kinds()[0];
    store.append(&coord, kind, &serde_json::json!({ "n": 1 }))?;
    store.append(&coord, kind, &serde_json::json!({ "n": 2 }))?;

    // ---- FRESH branch (meta.watermark == replay.watermark) ----
    // First run warms the external cache at the current watermark (W2). Close and
    // reopen with the SAME cache dir so the persistent external row survives but
    // the in-memory group-local slot is dropped -> next run has no group-local
    // slot and is served straight from the external cache at the SAME watermark.
    let warm: Option<FreshnessCounter> =
        store.project("entity:freshobs", &Freshness::Consistent)?;
    assert_eq!(warm, Some(FreshnessCounter { count: 2 }));
    store.close()?;

    let store = Store::open_with_native_cache(
        StoreConfig::new(dir.path().join("data")).with_sync_every_n_events(1),
        dir.path().join("cache"),
    )?;

    let (fresh_state, fresh_report) = store
        .project_run_evidence::<FreshnessCounter>("entity:freshobs", &Freshness::Consistent)?;
    assert_eq!(fresh_state, Some(FreshnessCounter { count: 2 }));
    assert_eq!(
        fresh_report.body.observed_freshness,
        ProjectionRunFreshnessStatus::Fresh,
        "external-cache hit with meta.watermark == replay.watermark must observe Fresh; \
         the :607 `== -> !=` mutant reports StaleAllowed here"
    );

    // ---- STALE-ALLOWED branch (meta.watermark != replay.watermark) ----
    // Advance disk past the cached watermark (W3) and request MaybeStale with a
    // wide age window so the (older-watermark) cached row is still age-fresh:
    // is_fresh == true, but meta.watermark (2) != replay.watermark (3) -> the
    // line-607 else arm yields StaleAllowed. The served value is the cached 2.
    store.append(&coord, kind, &serde_json::json!({ "n": 3 }))?;
    let (stale_state, stale_report) = store.project_run_evidence::<FreshnessCounter>(
        "entity:freshobs",
        &Freshness::MaybeStale {
            max_stale_ms: 60_000,
        },
    )?;
    assert_eq!(
        stale_state,
        Some(FreshnessCounter { count: 2 }),
        "age-fresh external row at the stale watermark must be served (cached 2)"
    );
    assert_eq!(
        stale_report.body.observed_freshness,
        ProjectionRunFreshnessStatus::StaleAllowed,
        "external-cache hit with meta.watermark != replay.watermark must observe \
         StaleAllowed; the :607 `== -> !=` mutant reports Fresh here"
    );

    store.close()?;
    Ok(())
}

#[test]
fn ahead_of_disk_external_cache_row_is_not_served_on_freshness_path() -> TestResult {
    // R16 (cache-hit path): a MaybeStale (age-fresh) external cache row whose
    // watermark is AHEAD of disk — e.g. a row that survived a rollback/rebuild —
    // must NOT be returned. Serving it would report state for events no longer
    // present in the current segment log. The guard
    //   `is_fresh && meta.watermark <= execution.replay.watermark`
    // forces a full replay so disk stays authoritative. Without the
    // `meta.watermark <= replay.watermark` term, the age-only `is_fresh` would
    // return the forged ahead-of-disk value.
    use crate::coordinate::Coordinate;
    use crate::store::projection::CacheMeta;
    use crate::store::{Freshness, Store};

    // Non-incremental projection (defaults route through the external-cache
    // path). value == on-disk event count, so the forged ahead-of-disk row and
    // the honest full-replay value are distinguishable.
    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct AheadCounter {
        count: u32,
    }
    impl EventSourced for AheadCounter {
        type Input = crate::event::JsonValueInput;
        fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
            Some(AheadCounter {
                count: u32::try_from(events.len()).expect("test uses < 2^32 events"),
            })
        }
        fn apply_event(&mut self, _event: &Event<serde_json::Value>) {}
        fn relevant_event_kinds() -> &'static [EventKind] {
            static KINDS: [EventKind; 1] = [EventKind::custom(0xF, 1)];
            &KINDS
        }
        // supports_incremental_apply() defaults to false -> external-cache path.
    }

    let dir = TempDir::new()?;
    let config = StoreConfig::new(dir.path().join("data")).with_sync_every_n_events(1);
    let store = Store::open_with_native_cache(config, dir.path().join("cache"))?;

    let coord = Coordinate::new("entity:ahead", "scope:test").expect("coordinate");
    let kind = AheadCounter::relevant_event_kinds()[0];
    // Disk holds two events -> the honest projection is count 2.
    store.append(&coord, kind, &serde_json::json!({ "n": 1 }))?;
    store.append(&coord, kind, &serde_json::json!({ "n": 2 }))?;

    // Forge an external cache row that is AHEAD of disk: it claims a future
    // watermark (u64::MAX, guaranteed > the current disk watermark) and a bogus
    // value (count 99). It is freshly stamped so the MaybeStale age check alone
    // would treat it as fresh.
    let key = projection_cache_key::<AheadCounter>("entity:ahead");
    let forged = serde_json::to_vec(&AheadCounter { count: 99 })?;
    store.cache.put(
        &key,
        &forged,
        CacheMeta {
            watermark: u64::MAX,
            cached_at_us: store.runtime.cache_now_us(),
            cached_at_mono_ns: Some(store.runtime.now_mono_ns()),
            process_boot_ns: Some(store.runtime.process_boot_ns()),
        },
    )?;

    // MaybeStale with a wide age window: age-only freshness would serve the
    // forged 99. The ahead-of-disk guard must instead force a full replay -> 2.
    let observed: Option<AheadCounter> = store.project(
        "entity:ahead",
        &Freshness::MaybeStale {
            max_stale_ms: 60_000,
        },
    )?;
    assert_eq!(
        observed,
        Some(AheadCounter { count: 2 }),
        "ahead-of-disk cache row (watermark u64::MAX > disk) must NOT be served; \
         disk is authoritative (full replay 2). Dropping the \
         `meta.watermark <= replay.watermark` guard serves the forged 99."
    );

    store.close()?;
    Ok(())
}
