// justifies: unified red-path reader tests use unwrap/panic as assertion style and narrow bounded test counters that fit within u32.
#![allow(clippy::unwrap_used, clippy::cast_possible_truncation, clippy::panic)]

#[path = "support/unified_red.rs"]
mod unified_red_support;

use unified_red_support::*;

#[test]
fn sealed_segment_reads_via_mmap() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_segment_max_bytes(512);
    let store = Store::open(config).expect("open");
    let coord = test_coord();
    for i in 0u32..50 {
        store.append(&coord, kind_a(), &payload(i)).expect("append");
    }
    store.sync().expect("sync");
    let entries = store.stream("entity:test");
    let first = &entries[0];
    let event = store.get(first.event_id).expect("get from sealed segment");
    assert_eq!(
        event.coordinate.entity(),
        "entity:test",
        "PROPERTY: mmap read from sealed segment must return correct event.\n\
         Investigate: src/store/segment/scan.rs sealed_maps path."
    );
    store.close().expect("close");
}

#[test]
fn concurrent_sealed_reads_no_lock_contention() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_segment_max_bytes(512);
    let store = Arc::new(Store::open(config).expect("open"));
    let coord = test_coord();
    let mut ids = Vec::new();
    for i in 0u32..20 {
        let receipt = store.append(&coord, kind_a(), &payload(i)).expect("append");
        ids.push(receipt.event_id);
    }
    store.sync().expect("sync");

    let handles: Vec<_> = ids
        .iter()
        .map(|&id| {
            let store = Arc::clone(&store);
            std::thread::Builder::new()
                .name(format!("reader-{id}"))
                .spawn(move || {
                    store.get(id).expect("concurrent get");
                })
                .expect("spawn")
        })
        .collect();
    for handle in handles {
        handle.join().expect("reader thread");
    }
}

#[test]
fn evict_mmap_before_compaction_delete() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_segment_max_bytes(512);
    let store = Store::open(config).expect("open");
    let coord = test_coord();
    for i in 0u32..50 {
        store.append(&coord, kind_a(), &payload(i)).expect("append");
    }
    store.sync().expect("sync");
    let result = store.compact(&CompactionConfig::default());
    assert!(
        result.is_ok(),
        "PROPERTY: compaction must succeed even with mmap'd segments.\n\
         Investigate: src/store/segment/scan.rs evict_segment drops Mmap before delete.\n\
         Got: {result:?}"
    );
    store.close().expect("close");
}
