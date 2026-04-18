// justifies: unified red-path projection tests use unwrap/panic as the assertion style and narrow bounded test counters that fit in u32.
#![allow(clippy::unwrap_used, clippy::cast_possible_truncation, clippy::panic)]

#[path = "support/unified_red.rs"]
mod unified_red_support;

use unified_red_support::*;

#[test]
fn relevant_kinds_filters_before_disk_read() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open");
    let coord = Coordinate::new("kf:entity", "kf:scope").expect("coord");
    for i in 0u32..5 {
        store.append(&coord, kind_a(), &payload(i)).expect("target");
        store.append(&coord, kind_b(), &payload(i)).expect("noise");
    }
    let result: Option<KindFilteredCounter> = store
        .project("kf:entity", &Freshness::Consistent)
        .expect("project");
    let counter = result.expect("should have events");
    assert_eq!(
        counter.target_count, 5,
        "PROPERTY: only relevant_event_kinds events must reach from_events.\n\
         Investigate: src/store/projection/flow.rs kind filter."
    );
    assert_eq!(
        counter.noise_count, 0,
        "PROPERTY: noise events must be filtered BEFORE apply_event.\n\
         noise_count={}, expected 0.\n\
         Investigate: src/store/projection/flow.rs entries filter.",
        counter.noise_count
    );
    store.close().expect("close");
}

#[test]
fn empty_kinds_replays_all() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open");
    let coord = Coordinate::new("rek:entity", "rek:scope").expect("coord");
    for i in 0u32..3 {
        store
            .append(&coord, EventKind::custom(0xF, i as u16 + 1), &payload(i))
            .expect("append");
    }
    let result: Option<AllCounter> = store
        .project("rek:entity", &Freshness::Consistent)
        .expect("project");
    assert_eq!(
        result.expect("some").count,
        3,
        "PROPERTY: empty relevant_event_kinds = replay all events."
    );
    store.close().expect("close");
}

#[test]
fn schema_version_default_is_zero() {
    assert_eq!(
        AllCounter::schema_version(),
        0,
        "PROPERTY: default schema_version() must be 0."
    );
}

#[test]
fn schema_version_can_be_overridden() {
    assert_eq!(
        VersionedCounterV2::schema_version(),
        2,
        "PROPERTY: overridden schema_version() must return declared value."
    );
}

#[test]
fn versioned_cache_key_isolates_versions() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open");
    let coord = Coordinate::new("sv:entity", "sv:scope").expect("coord");
    for i in 0u32..5 {
        store.append(&coord, kind_a(), &payload(i)).expect("append");
    }
    let r1: Option<AllCounter> = store
        .project("sv:entity", &Freshness::Consistent)
        .expect("project v0");
    assert_eq!(r1.expect("some").count, 5);
    let r2: Option<VersionedCounterV2> = store
        .project("sv:entity", &Freshness::Consistent)
        .expect("project v2");
    assert_eq!(
        r2.expect("some").count,
        5,
        "PROPERTY: schema-versioned cache keys must isolate different projection types.\n\
         If this returned a deserialization error, the v0 cache leaked into v2.\n\
         Investigate: src/store/projection/flow.rs cache key construction."
    );
    store.close().expect("close");
}

#[test]
fn versioned_cache_key_isolates_with_native_cache() {
    let dir = TempDir::new().expect("temp dir");
    let cache_path = dir.path().join("cache");
    let config = StoreConfig::new(dir.path().join("data"));
    let store = Store::open_with_native_cache(config, &cache_path).expect("open with native cache");
    let coord = Coordinate::new("svr:entity", "svr:scope").expect("coord");
    for i in 0u32..5 {
        store.append(&coord, kind_a(), &payload(i)).expect("append");
    }
    let r1: Option<AllCounter> = store
        .project("svr:entity", &Freshness::Consistent)
        .expect("project v0");
    assert_eq!(r1.expect("some").count, 5);
    let r2: Option<VersionedCounterV2> = store
        .project("svr:entity", &Freshness::Consistent)
        .expect("project v2");
    assert_eq!(
        r2.expect("some").count,
        5,
        "PROPERTY: native-cache-backed schema-versioned cache keys must isolate types.\n\
         v0 and v2 projections must not share a cache slot.\n\
         Investigate: src/store/projection/flow.rs cache key with schema_version."
    );
    store.close().expect("close");
}

#[test]
fn supports_incremental_apply_default_is_false() {
    assert!(
        !AllCounter::supports_incremental_apply(),
        "PROPERTY: default supports_incremental_apply() must be false."
    );
}

#[test]
fn incremental_apply_delta_only() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_incremental_projection(true);
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("inc:entity", "inc:scope").expect("coord");
    for i in 0u32..5 {
        store.append(&coord, kind_a(), &payload(i)).expect("append");
    }
    let r1: Option<IncrementalCounter> = store
        .project("inc:entity", &Freshness::Consistent)
        .expect("first project");
    assert_eq!(r1.expect("some").count, 5);
    for i in 5u32..8 {
        store.append(&coord, kind_a(), &payload(i)).expect("append");
    }
    let r2: Option<IncrementalCounter> = store
        .project("inc:entity", &Freshness::Consistent)
        .expect("incremental project");
    assert_eq!(
        r2.expect("some").count,
        8,
        "PROPERTY: incremental projection must reach correct total (5 cached + 3 new = 8).\n\
         Investigate: src/store/projection/flow.rs incremental apply path."
    );
    store.close().expect("close");
}

#[test]
fn batch_read_matches_sequential() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open");
    let coord = Coordinate::new("batch:entity", "batch:scope").expect("coord");
    for i in 0u32..30 {
        store.append(&coord, kind_a(), &payload(i)).expect("append");
    }
    let result: Option<AllCounter> = store
        .project("batch:entity", &Freshness::Consistent)
        .expect("project");
    assert_eq!(
        result.expect("some").count,
        30,
        "PROPERTY: batch read projection must replay all 30 events.\n\
         Investigate: src/store/segment/scan.rs read_entries_batch."
    );
    store.close().expect("close");
}

#[test]
fn single_slot_hit_on_repeat_project() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open");
    let coord = Coordinate::new("slot:entity", "slot:scope").expect("coord");
    for i in 0u32..10 {
        store.append(&coord, kind_a(), &payload(i)).expect("append");
    }
    let r1: Option<AllCounter> = store
        .project("slot:entity", &Freshness::Consistent)
        .expect("first project");
    assert_eq!(r1.expect("some").count, 10);
    let r2: Option<AllCounter> = store
        .project("slot:entity", &Freshness::Consistent)
        .expect("second project");
    assert_eq!(
        r2.expect("some").count,
        10,
        "PROPERTY: repeated project on same entity must use single-slot cache."
    );
    store.close().expect("close");
}

#[test]
fn single_slot_miss_on_different_entity() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open");
    let coord_a = Coordinate::new("slot:a", "slot:scope").expect("coord");
    let coord_b = Coordinate::new("slot:b", "slot:scope").expect("coord");
    for i in 0u32..5 {
        store.append(&coord_a, kind_a(), &payload(i)).expect("a");
        store.append(&coord_b, kind_a(), &payload(i)).expect("b");
    }
    let r1: Option<AllCounter> = store
        .project("slot:a", &Freshness::Consistent)
        .expect("project a");
    assert_eq!(r1.expect("some").count, 5);
    let r2: Option<AllCounter> = store
        .project("slot:b", &Freshness::Consistent)
        .expect("project b");
    assert_eq!(
        r2.expect("some").count,
        5,
        "PROPERTY: projecting a different entity must not return slot:a's cached result."
    );
    store.close().expect("close");
}
