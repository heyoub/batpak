use super::*;
use crate::coordinate::Coordinate;
use crate::event::{EventKind, HashChain};
use crate::store::index::{DiskPos, IndexEntry};
use std::collections::BTreeMap;
use std::sync::Arc;

fn make_entry(kind: EventKind, seq: u64, entity: &str, scope: &str) -> Arc<IndexEntry> {
    let coord = Coordinate::new(entity, scope).expect("coord");
    Arc::new(IndexEntry {
        event_id: seq as u128,
        correlation_id: seq as u128,
        causation_id: None,
        coord,
        entity_id: crate::store::index::interner::InternId::sentinel(),
        scope_id: crate::store::index::interner::InternId::sentinel(),
        kind,
        wall_ms: seq * 1000,
        clock: u32::try_from(seq).expect("test seq fits u32"),
        dag_lane: 0,
        dag_depth: 0,
        hash_chain: HashChain::default(),
        disk_pos: DiskPos {
            segment_id: 0,
            offset: seq * 64,
            length: 64,
        },
        global_sequence: seq,
        receipt_extensions: BTreeMap::new(),
    })
}

const KIND_A: EventKind = EventKind::custom(0x1, 1);
const KIND_B: EventKind = EventKind::custom(0x1, 2);

// --- SoA ---

#[test]
fn soa_insert_and_query_by_kind() {
    let idx = ColumnarIndex::new_soa();
    for i in 0u64..10 {
        idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
    }
    for i in 10u64..15 {
        idx.insert(&make_entry(KIND_B, i, "e2", "s1"));
    }
    let a = idx.query_hits_by_kind(KIND_A);
    assert_eq!(a.len(), 10);
    for (i, h) in a.iter().enumerate() {
        assert_eq!(h.global_sequence, i as u64);
    }
    let b = idx.query_hits_by_kind(KIND_B);
    assert_eq!(b.len(), 5);
}

#[test]
fn soa_query_by_scope() {
    let idx = ColumnarIndex::new_soa();
    for i in 0u64..6 {
        idx.insert(&make_entry(KIND_A, i, "e1", "scope-x"));
    }
    for i in 6u64..10 {
        idx.insert(&make_entry(KIND_A, i, "e2", "scope-y"));
    }
    assert_eq!(idx.query_hits_by_scope("scope-x").len(), 6);
    assert_eq!(idx.query_hits_by_scope("scope-y").len(), 4);
    assert!(idx.query_hits_by_scope("scope-z").is_empty());
}

#[test]
fn soa_clear() {
    let idx = ColumnarIndex::new_soa();
    for i in 0u64..5 {
        idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
    }
    idx.clear();
    assert!(idx.query_hits_by_kind(KIND_A).is_empty());
    assert!(idx.query_hits_by_scope("s1").is_empty());
}

// --- AoSoA8 ---

#[test]
fn aosoa8_insert_spans_multiple_tiles() {
    let idx = ColumnarIndex::new_aosoa8();
    for i in 0u64..20 {
        idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
    }
    let results = idx.query_hits_by_kind(KIND_A);
    assert_eq!(results.len(), 20);
    for (i, h) in results.iter().enumerate() {
        assert_eq!(h.global_sequence, i as u64, "order must be preserved");
    }
}

#[test]
fn aosoa8_interleaved_kinds() {
    let idx = ColumnarIndex::new_aosoa8();
    for i in 0u64..12 {
        idx.insert(&make_entry(KIND_A, i * 2, "ea", "s1"));
        idx.insert(&make_entry(KIND_B, i * 2 + 1, "eb", "s1"));
    }
    assert_eq!(idx.query_hits_by_kind(KIND_A).len(), 12);
    assert_eq!(idx.query_hits_by_kind(KIND_B).len(), 12);
}

#[test]
fn aosoa8_query_by_scope() {
    let idx = ColumnarIndex::new_aosoa8();
    for i in 0u64..9 {
        idx.insert(&make_entry(KIND_A, i, "ent-a", "scope-alpha"));
    }
    for i in 9u64..14 {
        idx.insert(&make_entry(KIND_A, i, "ent-b", "scope-beta"));
    }
    assert_eq!(idx.query_hits_by_scope("scope-alpha").len(), 9);
    assert_eq!(idx.query_hits_by_scope("scope-beta").len(), 5);
}

#[test]
fn aosoa8_with_tile_callback() {
    let idx = ColumnarIndex::new_aosoa8();
    for i in 0u64..8 {
        idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
    }
    // First tile should be full with KIND_A
    let len = idx.with_tile8(0, |t| t.len).expect("should be AoSoA8");
    assert_eq!(len, 8);
}

// --- AoSoA16 ---

#[test]
fn aosoa16_basic() {
    let idx = ColumnarIndex::new_aosoa16();
    for i in 0u64..33 {
        idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
    }
    assert_eq!(idx.query_hits_by_kind(KIND_A).len(), 33);
}

#[test]
fn aosoa16_with_tile_callback() {
    let idx = ColumnarIndex::new_aosoa16();
    for i in 0u64..16 {
        idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
    }
    let len = idx.with_tile16(0, |t| t.len).expect("should be AoSoA16");
    assert_eq!(len, 16);
}

// --- AoSoA64 ---

#[test]
fn aosoa64_basic() {
    let idx = ColumnarIndex::new_aosoa64();
    for i in 0u64..130 {
        idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
    }
    assert_eq!(idx.query_hits_by_kind(KIND_A).len(), 130);
}

#[test]
fn aosoa64_with_tile_callback() {
    let idx = ColumnarIndex::new_aosoa64();
    for i in 0u64..64 {
        idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
    }
    let len = idx.with_tile64(0, |t| t.len).expect("should be AoSoA64");
    assert_eq!(len, 64);
}

// --- SoAoS ---

#[test]
fn soaos_insert_and_query_by_kind() {
    let idx = ColumnarIndex::new_soaos();
    for i in 0u64..10 {
        idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
    }
    for i in 10u64..15 {
        idx.insert(&make_entry(KIND_B, i, "e2", "s1"));
    }
    assert_eq!(idx.query_hits_by_kind(KIND_A).len(), 10);
    assert_eq!(idx.query_hits_by_kind(KIND_B).len(), 5);
}

#[test]
fn soaos_query_by_scope() {
    let idx = ColumnarIndex::new_soaos();
    for i in 0u64..8 {
        idx.insert(&make_entry(KIND_A, i, "e1", "scope-x"));
    }
    for i in 8u64..12 {
        idx.insert(&make_entry(KIND_A, i, "e2", "scope-y"));
    }
    assert_eq!(idx.query_hits_by_scope("scope-x").len(), 8);
    assert_eq!(idx.query_hits_by_scope("scope-y").len(), 4);
}

#[test]
fn soaos_clear() {
    let idx = ColumnarIndex::new_soaos();
    for i in 0u64..5 {
        idx.insert(&make_entry(KIND_A, i, "e1", "s1"));
    }
    assert_eq!(idx.query_hits_by_kind(KIND_A).len(), 5);
    idx.clear();
    assert_eq!(idx.query_hits_by_kind(KIND_A).len(), 0);
}

// --- ScanIndex ---

#[test]
fn scan_index_maps_variant_insert_and_query() {
    let si = ScanIndex::for_config(&crate::store::IndexConfig {
        topology: crate::store::IndexTopology::aos(),
        incremental_projection: false,
        enable_checkpoint: true,
        enable_mmap_index: true,
    });
    for i in 0u64..7 {
        si.insert(&make_entry(KIND_A, i, "e1", "s1"));
    }
    assert_eq!(si.query_hits_by_kind(KIND_A).len(), 7);
}

#[test]
fn scan_index_soa_variant_insert_and_query() {
    let si = ScanIndex::for_config(&crate::store::IndexConfig {
        topology: crate::store::IndexTopology::scan(),
        incremental_projection: false,
        enable_checkpoint: true,
        enable_mmap_index: true,
    });
    for i in 0u64..12 {
        si.insert(&make_entry(KIND_A, i, "e1", "s2"));
    }
    assert_eq!(si.query_hits_by_kind(KIND_A).len(), 12);
}

#[test]
fn scan_capabilities_follow_topology_truth() {
    let cases = [
        (
            crate::store::IndexTopology::aos(),
            ScanCapabilities {
                by_kind: ScanRoute::BaseAoS,
                by_scope: ScanRoute::BaseAoS,
                by_category: ScanRoute::BaseAoS,
                projection: ProjectionSupport {
                    entity_generation_fast_path: false,
                    cached_projection: false,
                    projection_candidates: false,
                },
                topology_name: "aos",
                tile_count: 0,
            },
        ),
        (
            crate::store::IndexTopology::scan(),
            ScanCapabilities {
                by_kind: ScanRoute::SoA,
                by_scope: ScanRoute::SoA,
                by_category: ScanRoute::SoA,
                projection: ProjectionSupport {
                    entity_generation_fast_path: false,
                    cached_projection: false,
                    projection_candidates: false,
                },
                topology_name: "scan",
                tile_count: 0,
            },
        ),
        (
            crate::store::IndexTopology::entity_local(),
            ScanCapabilities {
                by_kind: ScanRoute::SoAoS,
                by_scope: ScanRoute::SoAoS,
                by_category: ScanRoute::SoAoS,
                projection: ProjectionSupport {
                    entity_generation_fast_path: true,
                    cached_projection: true,
                    projection_candidates: true,
                },
                topology_name: "entity-local",
                tile_count: 0,
            },
        ),
        (
            crate::store::IndexTopology::tiled(),
            ScanCapabilities {
                by_kind: ScanRoute::AoSoA64,
                by_scope: ScanRoute::AoSoA64,
                by_category: ScanRoute::AoSoA64,
                projection: ProjectionSupport {
                    entity_generation_fast_path: false,
                    cached_projection: false,
                    projection_candidates: false,
                },
                topology_name: "tiled",
                tile_count: 0,
            },
        ),
        (
            crate::store::IndexTopology::tiled_simd(),
            ScanCapabilities {
                by_kind: ScanRoute::AoSoA64Simd,
                by_scope: ScanRoute::AoSoA64Simd,
                by_category: ScanRoute::AoSoA64Simd,
                projection: ProjectionSupport {
                    entity_generation_fast_path: false,
                    cached_projection: false,
                    projection_candidates: false,
                },
                topology_name: "tiled-simd",
                tile_count: 0,
            },
        ),
        (
            crate::store::IndexTopology::all(),
            ScanCapabilities {
                by_kind: ScanRoute::SoA,
                by_scope: ScanRoute::SoAoS,
                by_category: ScanRoute::SoA,
                projection: ProjectionSupport {
                    entity_generation_fast_path: true,
                    cached_projection: true,
                    projection_candidates: true,
                },
                topology_name: "all",
                tile_count: 0,
            },
        ),
    ];

    for (topology, expected) in cases {
        let si = ScanIndex::for_config(&crate::store::IndexConfig {
            topology,
            incremental_projection: false,
            enable_checkpoint: true,
            enable_mmap_index: true,
        });
        assert_eq!(
            si.capabilities(),
            expected,
            "ScanCapabilities must be the single routing truth for `{}`",
            expected.topology_name
        );
    }
}

#[test]
fn scan_index_aosoa8_variant() {
    let si = ScanIndex::for_config(&crate::store::IndexConfig {
        topology: crate::store::IndexTopology::tiled(),
        incremental_projection: false,
        enable_checkpoint: true,
        enable_mmap_index: true,
    });
    for i in 0u64..20 {
        si.insert(&make_entry(KIND_A, i, "e1", "s1"));
    }
    assert_eq!(si.query_hits_by_kind(KIND_A).len(), 20);
}

#[test]
fn scan_index_maps_scope_entity_set() {
    let si = ScanIndex::for_config(&crate::store::IndexConfig {
        topology: crate::store::IndexTopology::aos(),
        incremental_projection: false,
        enable_checkpoint: true,
        enable_mmap_index: true,
    });
    si.insert(&make_entry(KIND_A, 0, "ent-1", "my-scope"));
    si.insert(&make_entry(KIND_A, 1, "ent-2", "my-scope"));
    let set = si
        .scope_entity_set("my-scope")
        .expect("should be Some for Maps");
    assert!(set.contains("ent-1" as &str));
    assert!(set.contains("ent-2" as &str));
}

#[test]
fn scan_index_columnar_scope_entity_set_uses_base_aos_view() {
    let si = ScanIndex::for_config(&crate::store::IndexConfig {
        topology: crate::store::IndexTopology::scan(),
        incremental_projection: false,
        enable_checkpoint: true,
        enable_mmap_index: true,
    });
    si.insert(&make_entry(KIND_A, 0, "ent-1", "my-scope"));
    let set = si
        .scope_entity_set("my-scope")
        .expect("base AoS scope-entity map stays active across layouts");
    assert!(set.contains("ent-1" as &str));
}

#[test]
fn scan_index_clear() {
    let si = ScanIndex::for_config(&crate::store::IndexConfig {
        topology: crate::store::IndexTopology::scan(),
        incremental_projection: false,
        enable_checkpoint: true,
        enable_mmap_index: true,
    });
    for i in 0u64..5 {
        si.insert(&make_entry(KIND_A, i, "e1", "s1"));
    }
    si.clear();
    assert!(si.query_hits_by_kind(KIND_A).is_empty());
}

#[test]
fn scan_index_soaos_variant() {
    let si = ScanIndex::for_config(&crate::store::IndexConfig {
        topology: crate::store::IndexTopology::entity_local(),
        incremental_projection: false,
        enable_checkpoint: true,
        enable_mmap_index: true,
    });
    for i in 0u64..10 {
        si.insert(&make_entry(KIND_A, i, "e1", "s1"));
    }
    assert_eq!(si.query_hits_by_kind(KIND_A).len(), 10);
    assert_eq!(si.query_hits_by_scope("s1").len(), 10);
    si.clear();
    assert!(si.query_hits_by_kind(KIND_A).is_empty());
}

#[test]
fn entity_local_projection_fast_paths_round_trip() {
    let si = ScanIndex::for_config(&crate::store::IndexConfig {
        topology: crate::store::IndexTopology::entity_local(),
        incremental_projection: false,
        enable_checkpoint: true,
        enable_mmap_index: true,
    });
    si.insert(&make_entry(KIND_A, 0, "entity:projection", "scope:test"));
    si.insert(&make_entry(KIND_A, 1, "entity:projection", "scope:test"));

    assert_eq!(
            si.entity_generation("entity:projection"),
            Some(2),
            "PROPERTY: entity-local topology must expose an entity generation fast path for projection watchers"
        );

    let type_id = std::any::TypeId::of::<u64>();
    let stored = si.store_cached_projection("entity:projection", type_id, b"cached".to_vec(), 1);
    assert!(stored.is_stored());
    let slot = si
        .cached_projection("entity:projection", type_id)
        .expect("cached projection slot");
    assert_eq!(slot.bytes, b"cached");
    assert_eq!(slot.watermark, 1);
    assert_eq!(
            slot.generation, 2,
            "PROPERTY: cached projection slots must be stamped with the entity group's current generation"
        );
}

#[test]
fn scan_capabilities_track_tile_count_for_tiled_views() {
    let si = ScanIndex::for_config(&crate::store::IndexConfig {
        topology: crate::store::IndexTopology::tiled(),
        incremental_projection: false,
        enable_checkpoint: true,
        enable_mmap_index: true,
    });
    for i in 0u64..130 {
        si.insert(&make_entry(KIND_A, i, "e1", "s1"));
    }
    let capabilities = si.capabilities();
    assert_eq!(capabilities.topology_name, "tiled");
    assert_eq!(capabilities.by_kind, ScanRoute::AoSoA64);
    assert_eq!(capabilities.by_scope, ScanRoute::AoSoA64);
    assert_eq!(capabilities.by_category, ScanRoute::AoSoA64);
    assert_eq!(capabilities.tile_count, 3);
    assert!(!capabilities.projection.cached_projection);
    assert!(!capabilities.projection.projection_candidates);
}

//
// This test is the correctness contract that makes the AoSoA64 SIMD
// specialization (Step 4) safe to add: any specialized executor must
// produce the same output as SoA on the same corpus.

const KIND_C: EventKind = EventKind::custom(0x2, 1); // different category from KIND_A/KIND_B

fn build_oracle_corpus() -> Vec<Arc<IndexEntry>> {
    // 20 KIND_A across two entities + 10 KIND_B + 5 KIND_C, two scopes.
    // Interleaved insertion to stress tile bucketing in AoSoA.
    let mut entries = Vec::new();
    let mut seq = 0u64;
    for _ in 0..10 {
        entries.push(make_entry(KIND_A, seq, "entity-alpha", "scope-one"));
        seq += 1;
        entries.push(make_entry(KIND_B, seq, "entity-beta", "scope-one"));
        seq += 1;
    }
    for _ in 0..10 {
        entries.push(make_entry(KIND_A, seq, "entity-gamma", "scope-two"));
        seq += 1;
        entries.push(make_entry(KIND_C, seq, "entity-gamma", "scope-two"));
        seq += 1;
    }
    entries
}

fn seq_ids(v: &[QueryHit]) -> Vec<u64> {
    v.iter().map(|h| h.global_sequence).collect()
}

#[test]
fn all_layouts_agree_on_by_kind() {
    let corpus = build_oracle_corpus();
    let soa = ColumnarIndex::new_soa();
    let aosoa64 = ColumnarIndex::new_aosoa64();
    let aosoa64_simd = ColumnarIndex::new_aosoa64_simd();
    let soaos = ColumnarIndex::new_soaos();
    for entry in &corpus {
        soa.insert(entry);
        aosoa64.insert(entry);
        aosoa64_simd.insert(entry);
        soaos.insert(entry);
    }
    for kind in [KIND_A, KIND_B, KIND_C] {
        let reference = seq_ids(&soa.query_hits_by_kind(kind));
        assert_eq!(
            seq_ids(&aosoa64.query_hits_by_kind(kind)),
            reference,
            "AoSoA64 by_kind({kind:?}) must match SoA"
        );
        assert_eq!(
            seq_ids(&aosoa64_simd.query_hits_by_kind(kind)),
            reference,
            "AoSoA64Simd by_kind({kind:?}) must match SoA"
        );
        assert_eq!(
            seq_ids(&soaos.query_hits_by_kind(kind)),
            reference,
            "SoAoS by_kind({kind:?}) must match SoA"
        );
    }
}

#[test]
fn all_layouts_agree_on_by_category() {
    let corpus = build_oracle_corpus();
    let soa = ColumnarIndex::new_soa();
    let aosoa64 = ColumnarIndex::new_aosoa64();
    let aosoa64_simd = ColumnarIndex::new_aosoa64_simd();
    let soaos = ColumnarIndex::new_soaos();
    for entry in &corpus {
        soa.insert(entry);
        aosoa64.insert(entry);
        aosoa64_simd.insert(entry);
        soaos.insert(entry);
    }
    for category in [0x1u8, 0x2u8] {
        let reference = seq_ids(&soa.query_hits_by_category(category));
        assert_eq!(
            seq_ids(&aosoa64.query_hits_by_category(category)),
            reference,
            "AoSoA64 by_category(0x{category:x}) must match SoA"
        );
        assert_eq!(
            seq_ids(&aosoa64_simd.query_hits_by_category(category)),
            reference,
            "AoSoA64Simd by_category(0x{category:x}) must match SoA"
        );
        assert_eq!(
            seq_ids(&soaos.query_hits_by_category(category)),
            reference,
            "SoAoS by_category(0x{category:x}) must match SoA"
        );
    }
}

#[test]
fn all_layouts_agree_on_by_kind_after() {
    let corpus = build_oracle_corpus();
    let soa = ColumnarIndex::new_soa();
    let aosoa64 = ColumnarIndex::new_aosoa64();
    let aosoa64_simd = ColumnarIndex::new_aosoa64_simd();
    let soaos = ColumnarIndex::new_soaos();
    for entry in &corpus {
        soa.insert(entry);
        aosoa64.insert(entry);
        aosoa64_simd.insert(entry);
        soaos.insert(entry);
    }
    for kind in [KIND_A, KIND_B, KIND_C] {
        let reference = seq_ids(&soa.query_hits_by_kind_after(kind, 7, true, 5));
        assert_eq!(
            seq_ids(&aosoa64.query_hits_by_kind_after(kind, 7, true, 5)),
            reference,
            "AoSoA64 by_kind_after({kind:?}) must match SoA"
        );
        assert_eq!(
            seq_ids(&aosoa64_simd.query_hits_by_kind_after(kind, 7, true, 5)),
            reference,
            "AoSoA64Simd by_kind_after({kind:?}) must match SoA"
        );
        assert_eq!(
            seq_ids(&soaos.query_hits_by_kind_after(kind, 7, true, 5)),
            reference,
            "SoAoS by_kind_after({kind:?}) must match SoA"
        );
    }
}

#[test]
fn all_layouts_agree_on_by_category_after() {
    let corpus = build_oracle_corpus();
    let soa = ColumnarIndex::new_soa();
    let aosoa64 = ColumnarIndex::new_aosoa64();
    let aosoa64_simd = ColumnarIndex::new_aosoa64_simd();
    let soaos = ColumnarIndex::new_soaos();
    for entry in &corpus {
        soa.insert(entry);
        aosoa64.insert(entry);
        aosoa64_simd.insert(entry);
        soaos.insert(entry);
    }
    for category in [0x1u8, 0x2u8] {
        let reference = seq_ids(&soa.query_hits_by_category_after(category, 7, true, 5));
        assert_eq!(
            seq_ids(&aosoa64.query_hits_by_category_after(category, 7, true, 5)),
            reference,
            "AoSoA64 by_category_after(0x{category:x}) must match SoA"
        );
        assert_eq!(
            seq_ids(&aosoa64_simd.query_hits_by_category_after(category, 7, true, 5)),
            reference,
            "AoSoA64Simd by_category_after(0x{category:x}) must match SoA"
        );
        assert_eq!(
            seq_ids(&soaos.query_hits_by_category_after(category, 7, true, 5)),
            reference,
            "SoAoS by_category_after(0x{category:x}) must match SoA"
        );
    }
}

// --- B2 contract: overlay scope queries are a subset of ground truth ---
//
// Every overlay's `query_hits_by_scope` output must be a subset of the
// ground-truth "entries whose coord.scope == scope" set computed from the
// raw corpus. Overlays may return fewer results (the shared filter
// pipeline in StoreIndex::query_hits re-validates) but must never leak
// events from other scopes.
fn ground_truth_by_scope(corpus: &[Arc<IndexEntry>], scope: &str) -> Vec<u64> {
    let mut v: Vec<u64> = corpus
        .iter()
        .filter(|e| e.coord.scope() == scope)
        .map(|e| e.global_sequence)
        .collect();
    v.sort_unstable();
    v
}

fn is_subset_of_truth(overlay: &[QueryHit], truth: &[u64]) -> bool {
    let truth_set: std::collections::HashSet<u64> = truth.iter().copied().collect();
    overlay
        .iter()
        .all(|h| truth_set.contains(&h.global_sequence))
}

#[test]
fn overlay_scope_queries_are_subset_of_ground_truth() {
    let corpus = build_oracle_corpus();
    let soa = ColumnarIndex::new_soa();
    let aosoa64 = ColumnarIndex::new_aosoa64();
    let aosoa64_simd = ColumnarIndex::new_aosoa64_simd();
    let soaos = ColumnarIndex::new_soaos();
    for entry in &corpus {
        soa.insert(entry);
        aosoa64.insert(entry);
        aosoa64_simd.insert(entry);
        soaos.insert(entry);
    }
    for scope in ["scope-one", "scope-two", "scope-missing"] {
        let truth = ground_truth_by_scope(&corpus, scope);
        for (name, overlay_hits) in [
            ("SoA", soa.query_hits_by_scope(scope)),
            ("AoSoA64", aosoa64.query_hits_by_scope(scope)),
            ("AoSoA64Simd", aosoa64_simd.query_hits_by_scope(scope)),
            ("SoAoS", soaos.query_hits_by_scope(scope)),
        ] {
            assert!(
                is_subset_of_truth(&overlay_hits, &truth),
                "{name} overlay leaked events outside scope {scope:?}: hits={:?} truth={:?}",
                overlay_hits
                    .iter()
                    .map(|h| h.global_sequence)
                    .collect::<Vec<_>>(),
                truth,
            );
        }
    }
}

#[test]
fn overlay_scope_queries_after_respect_limit_and_subset() {
    let corpus = build_oracle_corpus();
    let soa = ColumnarIndex::new_soa();
    let aosoa64 = ColumnarIndex::new_aosoa64();
    let aosoa64_simd = ColumnarIndex::new_aosoa64_simd();
    let soaos = ColumnarIndex::new_soaos();
    for entry in &corpus {
        soa.insert(entry);
        aosoa64.insert(entry);
        aosoa64_simd.insert(entry);
        soaos.insert(entry);
    }
    for scope in ["scope-one", "scope-two"] {
        let truth = ground_truth_by_scope(&corpus, scope);
        for limit in [1usize, 3, 10, usize::MAX] {
            for (name, overlay_hits) in [
                ("SoA", soa.query_hits_by_scope_after(scope, 0, false, limit)),
                (
                    "AoSoA64",
                    aosoa64.query_hits_by_scope_after(scope, 0, false, limit),
                ),
                (
                    "AoSoA64Simd",
                    aosoa64_simd.query_hits_by_scope_after(scope, 0, false, limit),
                ),
                (
                    "SoAoS",
                    soaos.query_hits_by_scope_after(scope, 0, false, limit),
                ),
            ] {
                assert!(
                    overlay_hits.len() <= limit,
                    "{name} scope-after limit honoured: got {} > {}",
                    overlay_hits.len(),
                    limit
                );
                assert!(
                    is_subset_of_truth(&overlay_hits, &truth),
                    "{name} scope-after overlay leaked events outside scope {scope:?}"
                );
            }
        }
    }
}

#[test]
fn scan_index_after_queries_honor_kind_category_and_scope() {
    let corpus = build_oracle_corpus();
    let scan = ScanIndex::for_config(&crate::store::IndexConfig {
        topology: crate::store::IndexTopology::all(),
        ..crate::store::IndexConfig::default()
    });
    let soa = ColumnarIndex::new_soa();
    for entry in &corpus {
        scan.insert(entry);
        soa.insert(entry);
    }

    let by_kind = seq_ids(&scan.query_hits_by_kind_after(KIND_A, 7, true, 5));
    assert_eq!(
        by_kind,
        seq_ids(&soa.query_hits_by_kind_after(KIND_A, 7, true, 5)),
        "scan by_kind_after should stay wired through the overlay route"
    );

    let by_category = seq_ids(&scan.query_hits_by_category_after(0x1, 7, true, 5));
    assert_eq!(
        by_category,
        seq_ids(&soa.query_hits_by_category_after(0x1, 7, true, 5)),
        "scan by_category_after should stay wired through the overlay route"
    );

    let by_scope = seq_ids(&scan.query_hits_by_scope_after("scope-two", 7, true, 5));
    assert_eq!(
        by_scope,
        seq_ids(&soa.query_hits_by_scope_after("scope-two", 7, true, 5)),
        "scan by_scope_after should stay wired through the overlay route"
    );
}

#[test]
fn all_layouts_agree_on_by_scope() {
    let corpus = build_oracle_corpus();
    let soa = ColumnarIndex::new_soa();
    let aosoa64 = ColumnarIndex::new_aosoa64();
    let aosoa64_simd = ColumnarIndex::new_aosoa64_simd();
    let soaos = ColumnarIndex::new_soaos();
    for entry in &corpus {
        soa.insert(entry);
        aosoa64.insert(entry);
        aosoa64_simd.insert(entry);
        soaos.insert(entry);
    }
    for scope in ["scope-one", "scope-two", "scope-missing"] {
        let reference = seq_ids(&soa.query_hits_by_scope(scope));
        assert_eq!(
            seq_ids(&aosoa64.query_hits_by_scope(scope)),
            reference,
            "AoSoA64 by_scope({scope:?}) must match SoA"
        );
        assert_eq!(
            seq_ids(&aosoa64_simd.query_hits_by_scope(scope)),
            reference,
            "AoSoA64Simd by_scope({scope:?}) must match SoA"
        );
        assert_eq!(
            seq_ids(&soaos.query_hits_by_scope(scope)),
            reference,
            "SoAoS by_scope({scope:?}) must match SoA"
        );
    }
}
