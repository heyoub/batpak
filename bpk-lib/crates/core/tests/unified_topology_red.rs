//! Red-path tests keep the `unified_*_red` names for cross-surface edge cases
//! that should fail fast or prove defensive behavior across the unified store.

use batpak_testkit::red_kind_b;
use batpak_testkit::red_kinds;

use red_kind_b::*;
use red_kinds::*;

use batpak_testkit::prelude::*;
use tempfile::TempDir;

#[test]
fn index_topology_aos_is_default() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path());
    let store = Store::open(config).expect("open");
    store.close().expect("close");
}

#[test]
fn index_topology_scan_by_fact_correct() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_index_topology(IndexTopology::scan());
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("soa:entity", "soa:scope").expect("coord");
    for i in 0u32..10 {
        let _ = store.append(&coord, kind_a(), &payload(i)).expect("a");
    }
    for i in 0u32..5 {
        let _ = store.append(&coord, kind_b(), &payload(i)).expect("b");
    }
    let results = store.by_fact(kind_a());
    assert_eq!(
        results.len(),
        10,
        "PROPERTY: scan topology by_fact must return correct count.\n\
         Investigate: src/store/index/columnar.rs query_by_kind."
    );
    store.close().expect("close");
}

#[test]
fn index_topology_tiled_by_fact_correct() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_index_topology(IndexTopology::tiled());
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("tile:entity", "tile:scope").expect("coord");
    for i in 0u32..20 {
        let _ = store.append(&coord, kind_a(), &payload(i)).expect("append");
    }
    let results = store.by_fact(kind_a());
    assert_eq!(
        results.len(),
        20,
        "PROPERTY: tiled topology by_fact must return correct count.\n\
         Investigate: src/store/index/columnar.rs AoSoA64 query."
    );
    store.close().expect("close");
}

#[test]
fn index_topology_entity_local_by_fact_correct() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_index_topology(IndexTopology::entity_local());
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("tile16:entity", "tile16:scope").expect("coord");
    for i in 0u32..40 {
        let _ = store.append(&coord, kind_a(), &payload(i)).expect("append");
    }
    let results = store.by_fact(kind_a());
    assert_eq!(
        results.len(),
        40,
        "PROPERTY: entity-local topology by_fact must return correct count.\n\
         Investigate: src/store/index/columnar.rs SoAoSInner::query_by_kind."
    );
    store.close().expect("close");
}

#[test]
fn index_topology_all_by_fact_correct() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_index_topology(IndexTopology::all());
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("tile64:entity", "tile64:scope").expect("coord");
    for i in 0u32..150 {
        let _ = store.append(&coord, kind_a(), &payload(i)).expect("append");
    }
    let results = store.by_fact(kind_a());
    assert_eq!(
        results.len(),
        150,
        "PROPERTY: all-topology by_fact must return correct count.\n\
         Investigate: src/store/index/columnar.rs overlay fanout."
    );
    store.close().expect("close");
}

#[test]
fn topology_parity_aos_vs_scan() {
    let dir_aos = TempDir::new().expect("dir aos");
    let dir_scan = TempDir::new().expect("dir scan");
    let kind = kind_a();

    let populate = |store: &Store| {
        let coord = Coordinate::new("parity:entity", "parity:scope").expect("coord");
        for i in 0u32..20 {
            let _ = store.append(&coord, kind, &payload(i)).expect("append");
        }
    };

    let store_aos = Store::open(StoreConfig::new(dir_aos.path())).expect("open aos");
    populate(&store_aos);

    let store_scan =
        Store::open(StoreConfig::new(dir_scan.path()).with_index_topology(IndexTopology::scan()))
            .expect("open scan");
    populate(&store_scan);

    let events_aos = store_aos.by_fact(kind);
    let events_scan = store_scan.by_fact(kind);
    assert_eq!(
        events_aos.len(),
        events_scan.len(),
        "PROPERTY: aos and scan must return identical by_fact results.\n\
         aos={}, scan={}.",
        events_aos.len(),
        events_scan.len()
    );
    store_aos.close().expect("close");
    store_scan.close().expect("close");
}

#[test]
fn index_topology_entity_local_mixed_fact_correct() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_index_topology(IndexTopology::entity_local());
    let store = Store::open(config).expect("open");
    let coord = Coordinate::new("soaos:entity", "soaos:scope").expect("coord");
    for i in 0u32..15 {
        let _ = store.append(&coord, kind_a(), &payload(i)).expect("a");
    }
    for i in 0u32..5 {
        let _ = store.append(&coord, kind_b(), &payload(i)).expect("b");
    }
    let results = store.by_fact(kind_a());
    assert_eq!(
        results.len(),
        15,
        "PROPERTY: entity-local topology by_fact must return correct count.\n\
         Investigate: src/store/index/columnar.rs SoAoSInner::query_by_kind."
    );
    store.close().expect("close");
}

#[test]
fn topology_parity_aos_vs_entity_local() {
    let dir_aos = TempDir::new().expect("dir aos");
    let dir_entity_local = TempDir::new().expect("dir entity-local");
    let kind = kind_a();

    let populate = |store: &Store| {
        let coord = Coordinate::new("parity:entity", "parity:scope").expect("coord");
        for i in 0u32..20 {
            let _ = store.append(&coord, kind, &payload(i)).expect("append");
        }
    };

    let store_aos = Store::open(StoreConfig::new(dir_aos.path())).expect("open aos");
    populate(&store_aos);

    let store_entity_local = Store::open(
        StoreConfig::new(dir_entity_local.path())
            .with_index_topology(IndexTopology::entity_local()),
    )
    .expect("open entity-local");
    populate(&store_entity_local);

    let events_aos = store_aos.by_fact(kind);
    let events_entity_local = store_entity_local.by_fact(kind);
    assert_eq!(
        events_aos.len(),
        events_entity_local.len(),
        "PROPERTY: aos and entity-local must return identical by_fact results.\n\
         aos={}, entity-local={}.",
        events_aos.len(),
        events_entity_local.len()
    );
    store_aos.close().expect("close");
    store_entity_local.close().expect("close");
}
