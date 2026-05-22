use super::*;
use crate::coordinate::Coordinate;
use crate::event::{EventKind, HashChain};
use crate::store::index::{DiskPos, StoreIndex};
use std::collections::BTreeMap;
use tempfile::TempDir;

fn make_index(count: u64) -> StoreIndex {
    let idx = StoreIndex::new();
    for i in 0..count {
        let coord = Coordinate::new(format!("entity:{i}"), "scope:test").expect("valid coordinate");
        let entity_id = idx.interner.intern(coord.entity());
        let scope_id = idx.interner.intern(coord.scope());
        idx.insert(IndexEntry {
            event_id: (i + 1) as u128,
            correlation_id: (i + 1) as u128,
            causation_id: (i > 0).then_some(i as u128),
            coord,
            entity_id,
            scope_id,
            kind: EventKind::custom(
                0x1,
                u16::try_from(i & 0x0FFF).expect("masked to 12 bits, fits u16"),
            ),
            wall_ms: 10_000 + i,
            clock: u32::try_from(i).expect("fits u32"),
            dag_lane: 0,
            dag_depth: 0,
            hash_chain: HashChain::default(),
            disk_pos: DiskPos {
                segment_id: 7,
                offset: i * 64,
                length: 64,
            },
            global_sequence: i,
            receipt_extensions: BTreeMap::new(),
        });
    }
    idx
}

#[test]
fn mmap_index_roundtrip_restores_entries() {
    let tmp = TempDir::new().expect("temp dir");
    let segment_path = tmp.path().join(crate::store::segment::segment_filename(7));
    crate::store::platform::fs::write_derivative_file_atomically(
        tmp.path(),
        &segment_path,
        "test segment",
        &vec![0u8; 4096],
    )
    .expect("segment file");

    let src = make_index(8);
    write_mmap_index(&src, tmp.path(), 7, 512).expect("write mmap index");

    let snapshot = try_load_mmap_snapshot(tmp.path(), &crate::store::SystemClock::new())
        .expect("load snapshot");
    assert_eq!(snapshot.routing.entry_count, 8);
    assert!(
        !snapshot.routing.chunks.is_empty(),
        "v2 mmap index must persist chunk summaries"
    );

    let dst = StoreIndex::new();
    let restored = try_restore_mmap_index(&dst, tmp.path()).expect("restore");
    assert_eq!(restored.0.watermark_segment_id, 7);
    assert_eq!(restored.0.watermark_offset, 512);
    assert_eq!(dst.len(), 8);
    assert_eq!(dst.visible_sequence(), 8);
}

#[test]
fn mmap_index_roundtrip_restores_receipt_extensions() {
    let tmp = TempDir::new().expect("temp dir");
    let segment_path = tmp.path().join(crate::store::segment::segment_filename(7));
    crate::store::platform::fs::write_derivative_file_atomically(
        tmp.path(),
        &segment_path,
        "test segment",
        &vec![0u8; 4096],
    )
    .expect("segment file");

    let idx = StoreIndex::new();
    let coord = Coordinate::new("entity:mmap-ext", "scope:test").expect("coord");
    let entity_id = idx.interner.intern(coord.entity());
    let scope_id = idx.interner.intern(coord.scope());
    let mut receipt_extensions = BTreeMap::new();
    receipt_extensions.insert(
        ExtensionKey::new("app.audit").expect("valid extension key"),
        vec![0xFA, 0xCE, 0x05],
    );
    idx.insert(IndexEntry {
        event_id: 1,
        correlation_id: 1,
        causation_id: None,
        coord,
        entity_id,
        scope_id,
        kind: EventKind::DATA,
        wall_ms: 1_700_000_000_000,
        clock: 1,
        dag_lane: 0,
        dag_depth: 0,
        hash_chain: HashChain::default(),
        disk_pos: DiskPos {
            segment_id: 7,
            offset: 0,
            length: 64,
        },
        global_sequence: 0,
        receipt_extensions: receipt_extensions.clone(),
    });

    write_mmap_index(&idx, tmp.path(), 7, 512).expect("write mmap index");

    let snapshot = try_load_mmap_snapshot(tmp.path(), &crate::store::SystemClock::new())
        .expect("load snapshot");
    assert!(
        snapshot.receipt_extensions_hydrated,
        "PROPERTY: mmap v5 snapshots must carry receipt-extension maps directly."
    );
    assert_eq!(snapshot.entries.len(), 1);
    assert_eq!(
        snapshot.entries[0].receipt_extensions, receipt_extensions,
        "PROPERTY: mmap v5 extension blob table must preserve opaque receipt-extension bytes."
    );
}
