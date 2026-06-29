use super::*;
use crate::coordinate::Coordinate;
use crate::store::cold_start::{validate_watermark_segment, ReservedKindFallbackStats};
use crate::store::index::{recommended_restore_chunk_count, RoutingSummary, StoreIndex};
use std::collections::BTreeMap;
use tempfile::TempDir;

use super::test_support::{touch_segment, write_legacy_checkpoint_body};

/// Build a minimal populated StoreIndex with `n` synthetic entries.
fn make_index(n: u64) -> StoreIndex {
    let idx = StoreIndex::new();
    for i in 0..n {
        let coord = Coordinate::new(format!("entity:{i}"), "test-scope").expect("valid coordinate");
        let entity_id = idx.interner.intern(coord.entity()).expect("intern");
        let scope_id = idx.interner.intern(coord.scope()).expect("intern");
        let entry = IndexEntry {
            event_id: (i + 1) as u128,
            correlation_id: (i + 1) as u128,
            causation_id: if i == 0 { None } else { Some(i as u128) },
            coord,
            entity_id,
            scope_id,
            kind: EventKind::custom(0x1, (i & 0x0FFF) as u16),
            wall_ms: 1_700_000_000_000 + i * 1000,
            clock: u32::try_from(i).expect("i fits u32"),
            dag_lane: 0,
            dag_depth: 0,
            hash_chain: HashChain::default(),
            disk_pos: DiskPos {
                segment_id: 0,
                offset: i * 256,
                length: 256,
            },
            global_sequence: i,
            receipt_extensions: BTreeMap::new(),
        };
        idx.insert(entry);
    }
    // Publish all entries so read methods see them.
    idx.publish(idx.global_sequence(), "checkpoint-test-publish")
        .expect("publish all entries");
    idx
}

#[test]
fn round_trip_empty_index() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path();
    touch_segment(dir, 0);

    let idx = StoreIndex::new();
    write_checkpoint(&idx, dir, 0, 0).expect("write");

    let result = try_load_checkpoint(dir);
    assert!(result.is_some(), "checkpoint should load");

    let loaded = result.expect("some");
    let entries = loaded.entries;
    let wm = loaded.watermark;
    assert_eq!(entries.len(), 0);
    assert_eq!(wm.watermark_segment_id, 0);
    assert_eq!(wm.watermark_offset, 0);
}

#[test]
fn round_trip_with_entries() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path();
    touch_segment(dir, 0);

    let idx = make_index(16);
    write_checkpoint(&idx, dir, 0, 4096).expect("write");

    let raw = std::fs::read(dir.join(format::CHECKPOINT_FILENAME)).expect("read checkpoint");
    assert_eq!(
        u16::from_le_bytes([raw[6], raw[7]]),
        format::CHECKPOINT_VERSION,
        "write_checkpoint must encode the current checkpoint version"
    );
    let body = &raw[12..];
    let direct: format::CheckpointDataV6 =
        crate::encoding::from_bytes(body).expect("checkpoint body should deserialize directly");
    assert_eq!(direct.entries.len(), 16);
    assert!(
        validate_watermark_segment(dir, 0, 4096).is_ok(),
        "round-trip fixture must satisfy watermark validation"
    );

    let loaded = try_load_checkpoint(dir).expect("should load");
    let routing = loaded.routing.clone();
    let entries = loaded.entries;
    let wm = loaded.watermark;
    assert_eq!(entries.len(), 16);
    assert_eq!(wm.watermark_offset, 4096);

    // Verify sort order
    let seqs: Vec<u64> = entries.iter().map(|e| e.global_sequence).collect();
    let mut sorted = seqs.clone();
    sorted.sort_unstable();
    assert_eq!(seqs, sorted, "entries must be sorted by global_sequence");
    assert_eq!(routing.entry_count, 16);
    assert!(
        !routing.entity_runs.is_empty(),
        "v4 checkpoints must persist entity-run summaries"
    );
    assert!(
        !routing.chunks.is_empty(),
        "current-version checkpoints must persist chunk summaries"
    );
}

#[test]
fn current_version_snapshot_restores_checkpoint_directly() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path();
    touch_segment(dir, 0);

    let idx = make_index(16);
    let reserved_kind_fallbacks = ReservedKindFallbackStats {
        system: 2,
        effect: 1,
        system_histogram: std::iter::once((0x000Au16, 2usize)).collect(),
        effect_histogram: std::iter::once((0x1001u16, 1usize)).collect(),
    };
    write_checkpoint_with_reserved_kind_fallbacks(&idx, dir, 0, 4096, &reserved_kind_fallbacks)
        .expect("write checkpoint");

    let loaded = try_load_checkpoint_snapshot(dir).expect("load checkpoint snapshot");

    assert_eq!(loaded.entries.len(), 16);
    assert_eq!(loaded.watermark.watermark_offset, 4096);
    assert!(
        loaded.receipt_extensions_hydrated,
        "PROPERTY: current checkpoint entries carry receipt-extension maps directly."
    );
    assert_eq!(
        loaded.cumulative_reserved_kind_fallbacks,
        reserved_kind_fallbacks,
        "PROPERTY: direct checkpoint restore must preserve persisted cumulative reserved-kind fallback stats."
    );
    assert_eq!(
        loaded.entries.first().map(|entry| entry.global_sequence),
        Some(0),
        "PROPERTY: direct checkpoint restore must preserve sorted global-sequence order."
    );
    assert_eq!(
        loaded.entries.last().map(|entry| entry.global_sequence),
        Some(15),
        "PROPERTY: direct checkpoint restore must preserve the full checkpoint entry set."
    );
}

#[test]
fn current_version_checkpoint_restores_receipt_extensions_directly() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path();
    touch_segment(dir, 0);

    let idx = StoreIndex::new();
    let coord = Coordinate::new("entity:checkpoint-ext", "scope:test").expect("coord");
    let entity_id = idx.interner.intern(coord.entity()).expect("intern");
    let scope_id = idx.interner.intern(coord.scope()).expect("intern");
    let mut receipt_extensions = BTreeMap::new();
    receipt_extensions.insert(
        ExtensionKey::new("app.audit").expect("valid extension key"),
        vec![0xCA, 0xFE, 0x01],
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
            segment_id: 0,
            offset: 0,
            length: 64,
        },
        global_sequence: 0,
        receipt_extensions: receipt_extensions.clone(),
    });
    idx.publish(idx.global_sequence(), "checkpoint-extension-test-publish")
        .expect("publish");

    write_checkpoint(&idx, dir, 0, 64).expect("write checkpoint");

    let loaded = try_load_checkpoint_snapshot(dir).expect("load checkpoint snapshot");
    assert!(
        loaded.receipt_extensions_hydrated,
        "PROPERTY: current checkpoints must not need frame hydration for receipt extensions."
    );
    assert_eq!(loaded.entries.len(), 1);
    assert_eq!(
        loaded.entries[0].receipt_extensions, receipt_extensions,
        "PROPERTY: checkpoint v6 must preserve opaque receipt-extension bytes in the snapshot artifact."
    );
}

#[test]
fn checkpoint_entry_to_cold_start_row_preserves_index_fields() {
    let entry = CheckpointEntry {
        event_id: 0xAA,
        correlation_id: 0xBB,
        causation_id: Some(0xCC),
        entity_id: 1,
        scope_id: 2,
        kind: EventKind::custom(0x2, 0x34),
        wall_ms: 1234,
        clock: 7,
        dag_lane: 3,
        dag_depth: 5,
        prev_hash: [0x11; 32],
        event_hash: [0x22; 32],
        segment_id: 9,
        offset: 256,
        length: 64,
        global_sequence: 42,
        receipt_extensions: BTreeMap::new(),
    };
    let strings = vec![
        String::new(),
        "entity:checkpoint".to_owned(),
        "scope:test".to_owned(),
    ];

    let rebuilt = entry
        .to_cold_start_row()
        .to_index_entry(&strings)
        .expect("checkpoint row to index entry");

    assert_eq!(rebuilt.event_id, entry.event_id);
    assert_eq!(rebuilt.correlation_id, entry.correlation_id);
    assert_eq!(rebuilt.causation_id, entry.causation_id);
    assert_eq!(rebuilt.coord.entity(), "entity:checkpoint");
    assert_eq!(rebuilt.coord.scope(), "scope:test");
    assert_eq!(rebuilt.kind, entry.kind);
    assert_eq!(rebuilt.wall_ms, entry.wall_ms);
    assert_eq!(rebuilt.clock, entry.clock);
    assert_eq!(rebuilt.dag_lane, entry.dag_lane);
    assert_eq!(rebuilt.dag_depth, entry.dag_depth);
    assert_eq!(rebuilt.hash_chain.prev_hash, entry.prev_hash);
    assert_eq!(rebuilt.hash_chain.event_hash, entry.event_hash);
    assert_eq!(rebuilt.disk_pos, entry.to_disk_pos());
    assert_eq!(rebuilt.global_sequence, entry.global_sequence);
}

#[test]
fn checkpoint_entry_preserves_none_causation_in_cold_start_row() {
    let entry = CheckpointEntry {
        event_id: 1,
        correlation_id: 2,
        causation_id: None,
        entity_id: 1,
        scope_id: 2,
        kind: EventKind::DATA,
        wall_ms: 10,
        clock: 1,
        dag_lane: 0,
        dag_depth: 0,
        prev_hash: [0; 32],
        event_hash: [1; 32],
        segment_id: 3,
        offset: 4,
        length: 5,
        global_sequence: 6,
        receipt_extensions: BTreeMap::new(),
    };

    assert_eq!(entry.to_cold_start_row().causation_id, None);
}

#[test]
fn restore_rebuilds_index() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path();
    touch_segment(dir, 0);

    let src = make_index(8);
    write_checkpoint(&src, dir, 0, 0).expect("write");

    let loaded = try_load_checkpoint(dir).expect("should load");
    let entries = loaded.entries;
    let interner_strings = loaded.interner_strings;
    let stored_alloc = loaded.stored_allocator;

    let dst = StoreIndex::new();
    restore_from_checkpoint(&dst, entries, &interner_strings, stored_alloc).expect("restore");

    assert_eq!(dst.len(), 8);
}

#[test]
fn missing_file_returns_none() {
    let tmp = TempDir::new().expect("tempdir");
    assert!(
        try_load_checkpoint(tmp.path()).is_none(),
        "missing file should return None"
    );
}

#[test]
fn bad_magic_returns_none() {
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().join(format::CHECKPOINT_FILENAME);
    std::fs::write(&path, b"BADMAGIC\x00\x00\x00\x00").expect("write");
    assert!(
        try_load_checkpoint(tmp.path()).is_none(),
        "bad magic should return None"
    );
}

#[test]
fn crc_mismatch_returns_none() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path();
    touch_segment(dir, 0);

    let idx = make_index(4);
    write_checkpoint(&idx, dir, 0, 0).expect("write");

    // Corrupt the last byte of the file
    let path = dir.join(format::CHECKPOINT_FILENAME);
    let mut raw = std::fs::read(&path).expect("read");
    let last = raw.len() - 1;
    raw[last] ^= 0xFF;
    std::fs::write(&path, &raw).expect("rewrite");

    assert!(
        try_load_checkpoint(dir).is_none(),
        "CRC mismatch should return None"
    );
}

#[test]
fn missing_watermark_segment_returns_none() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path();
    // Write checkpoint referencing segment 99, but do NOT create that file.
    touch_segment(dir, 0); // segment 0 exists but 99 does not

    let idx = make_index(2);
    write_checkpoint(&idx, dir, 99, 0).expect("write");

    assert!(
        try_load_checkpoint(dir).is_none(),
        "missing watermark segment should return None"
    );
}

#[test]
fn future_version_is_a_canonical_refusal_not_a_silent_rebuild() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path();
    touch_segment(dir, 0);

    let idx = StoreIndex::new();
    write_checkpoint(&idx, dir, 0, 0).expect("write");

    // Overwrite the two version bytes (6..8, LE) with a FUTURE version. The
    // version field is OUTSIDE the CRC region (CRC at 8..12 covers body 12..),
    // and the future-version check fires before the CRC check, so no CRC fix is
    // needed — a forged version alone trips the refusal.
    let path = dir.join(format::CHECKPOINT_FILENAME);
    let mut raw = crate::store::platform::fs::read(&path).expect("read");
    let future = format::CHECKPOINT_VERSION + 1;
    raw[6..8].copy_from_slice(&future.to_le_bytes());
    std::fs::write(&path, &raw).expect("rewrite");

    // The format reader classifies it as a typed future-version refusal — NOT
    // a silent degrade to a rebuild-from-scan. (`try_load_checkpoint` collapses
    // every non-Loaded outcome to None, so we assert directly on the reader.)
    assert!(
        matches!(
            format::read_checkpoint_file(dir),
            crate::store::cold_start::FileLoad::FutureVersion { found, supported }
                if found == future && supported == format::CHECKPOINT_VERSION
        ),
        "PROPERTY: a future-version checkpoint must be FileLoad::FutureVersion, not a silent rebuild"
    );
}

#[test]
fn older_unsupported_version_still_degrades_gracefully() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path();
    touch_segment(dir, 0);

    let idx = StoreIndex::new();
    write_checkpoint(&idx, dir, 0, 0).expect("write");

    // Forge an OLDER, no-longer-decodable version (1: rejected by
    // decode_checkpoint_data). Fix the CRC so the version branch — not a CRC
    // mismatch — is what classifies it. An older artifact must keep its
    // graceful-rebuild path (decode returns None → silent rebuild), distinct
    // from the future-version hard refusal above.
    let path = dir.join(format::CHECKPOINT_FILENAME);
    let mut raw = crate::store::platform::fs::read(&path).expect("read");
    raw[6] = 1;
    raw[7] = 0;
    let body_crc = crc32fast::hash(&raw[12..]);
    raw[8..12].copy_from_slice(&body_crc.to_le_bytes());
    // Route the forge rewrite through the platform boundary (the same atomic
    // write the store itself uses) rather than a raw `std::fs::write`.
    crate::store::platform::fs::write_file_atomically(dir, &path, "checkpoint-forge", |file| {
        std::io::Write::write_all(file, &raw).map_err(crate::store::StoreError::Io)
    })
    .expect("rewrite forged older-version checkpoint");

    assert!(
        try_load_checkpoint(dir).is_none(),
        "older unsupported version must degrade to None (rebuild), not refuse"
    );
}

#[test]
fn v2_checkpoint_fallback_is_still_readable() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path();
    touch_segment(dir, 0);

    let idx = make_index(6);
    let mut entries: Vec<CheckpointEntry> = idx
        .all_entries()
        .into_iter()
        .map(|e| CheckpointEntry {
            event_id: e.event_id,
            correlation_id: e.correlation_id,
            causation_id: e.causation_id,
            entity_id: e.entity_id.as_u32(),
            scope_id: e.scope_id.as_u32(),
            kind: e.kind,
            wall_ms: e.wall_ms,
            clock: e.clock,
            dag_lane: e.dag_lane,
            dag_depth: e.dag_depth,
            prev_hash: e.hash_chain.prev_hash,
            event_hash: e.hash_chain.event_hash,
            segment_id: e.disk_pos.segment_id,
            offset: e.disk_pos.offset,
            length: e.disk_pos.length,
            global_sequence: e.global_sequence,
            receipt_extensions: BTreeMap::new(),
        })
        .collect();
    entries.sort_by_key(|entry| entry.global_sequence);
    let mut interner_strings = vec![String::new()];
    interner_strings.extend(idx.interner.to_snapshot());
    let body = crate::encoding::to_bytes(&format::CheckpointDataV2 {
        global_sequence: idx.global_sequence(),
        watermark_segment_id: 0,
        watermark_offset: 0,
        interner_strings,
        entries,
    })
    .expect("serialize v2 checkpoint");
    let crc = crc32fast::hash(&body);
    let path = dir.join(format::CHECKPOINT_FILENAME);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(format::CHECKPOINT_MAGIC);
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&crc.to_le_bytes());
    bytes.extend_from_slice(&body);
    std::fs::write(&path, bytes).expect("write v2 checkpoint");

    let loaded = try_load_checkpoint(dir).expect("load v2 checkpoint");
    assert_eq!(loaded.entries.len(), 6);
    assert_eq!(loaded.routing.entry_count, 6);
    assert!(
        !loaded.routing.chunks.is_empty(),
        "v2 fallback should synthesize chunk summaries on load"
    );
}

#[test]
fn v3_checkpoint_defaults_lane_depth_to_zero() {
    #[derive(Serialize)]
    struct LegacyCheckpointEntryV3 {
        #[serde(with = "crate::wire::u128_bytes")]
        event_id: u128,
        #[serde(with = "crate::wire::u128_bytes")]
        correlation_id: u128,
        #[serde(with = "crate::wire::option_u128_bytes")]
        causation_id: Option<u128>,
        entity_id: u32,
        scope_id: u32,
        kind: EventKind,
        wall_ms: u64,
        clock: u32,
        prev_hash: [u8; 32],
        event_hash: [u8; 32],
        segment_id: u64,
        offset: u64,
        length: u32,
        global_sequence: u64,
    }

    #[derive(Serialize)]
    struct LegacyCheckpointDataV3 {
        global_sequence: u64,
        watermark_segment_id: u64,
        watermark_offset: u64,
        interner_strings: Vec<String>,
        routing: RoutingSummary,
        entries: Vec<LegacyCheckpointEntryV3>,
    }

    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path();
    touch_segment(dir, 0);

    let idx = make_index(4);
    let mut legacy_entries: Vec<LegacyCheckpointEntryV3> = idx
        .all_entries()
        .into_iter()
        .map(|e| LegacyCheckpointEntryV3 {
            event_id: e.event_id,
            correlation_id: e.correlation_id,
            causation_id: e.causation_id,
            entity_id: e.entity_id.as_u32(),
            scope_id: e.scope_id.as_u32(),
            kind: e.kind,
            wall_ms: e.wall_ms,
            clock: e.clock,
            prev_hash: e.hash_chain.prev_hash,
            event_hash: e.hash_chain.event_hash,
            segment_id: e.disk_pos.segment_id,
            offset: e.disk_pos.offset,
            length: e.disk_pos.length,
            global_sequence: e.global_sequence,
        })
        .collect();
    legacy_entries.sort_by_key(|entry| entry.global_sequence);
    let mut interner_strings = vec![String::new()];
    interner_strings.extend(idx.interner.to_snapshot());
    let mut sorted_entries = idx.all_entries();
    sorted_entries.sort_by_key(|entry| entry.global_sequence);
    let routing = RoutingSummary::from_sorted_entries(
        &sorted_entries,
        recommended_restore_chunk_count(sorted_entries.len()),
    );
    let body = crate::encoding::to_bytes(&LegacyCheckpointDataV3 {
        global_sequence: idx.global_sequence(),
        watermark_segment_id: 0,
        watermark_offset: 0,
        interner_strings,
        routing,
        entries: legacy_entries,
    })
    .expect("serialize v3 checkpoint");
    let crc = crc32fast::hash(&body);
    let path = dir.join(format::CHECKPOINT_FILENAME);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(format::CHECKPOINT_MAGIC);
    bytes.extend_from_slice(&3u16.to_le_bytes());
    bytes.extend_from_slice(&crc.to_le_bytes());
    bytes.extend_from_slice(&body);
    std::fs::write(&path, bytes).expect("write v3 checkpoint");

    let loaded = try_load_checkpoint_snapshot(dir).expect("load v3 checkpoint snapshot");
    assert!(loaded.entries.iter().all(|entry| entry.dag_lane == 0));
    assert!(loaded.entries.iter().all(|entry| entry.dag_depth == 0));
}

#[test]
fn v4_checkpoint_preserves_lane_depth_and_defaults_reserved_stats() {
    #[derive(Serialize)]
    struct LegacyCheckpointEntryV4 {
        #[serde(with = "crate::wire::u128_bytes")]
        event_id: u128,
        #[serde(with = "crate::wire::u128_bytes")]
        correlation_id: u128,
        #[serde(with = "crate::wire::option_u128_bytes")]
        causation_id: Option<u128>,
        entity_id: u32,
        scope_id: u32,
        kind: EventKind,
        wall_ms: u64,
        clock: u32,
        dag_lane: u32,
        dag_depth: u32,
        prev_hash: [u8; 32],
        event_hash: [u8; 32],
        segment_id: u64,
        offset: u64,
        length: u32,
        global_sequence: u64,
    }

    #[derive(Serialize)]
    struct LegacyCheckpointDataV4 {
        global_sequence: u64,
        watermark_segment_id: u64,
        watermark_offset: u64,
        interner_strings: Vec<String>,
        routing: RoutingSummary,
        entries: Vec<LegacyCheckpointEntryV4>,
    }

    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path();
    touch_segment(dir, 0);

    let idx = make_index(4);
    let mut legacy_entries: Vec<LegacyCheckpointEntryV4> = idx
        .all_entries()
        .into_iter()
        .map(|e| LegacyCheckpointEntryV4 {
            event_id: e.event_id,
            correlation_id: e.correlation_id,
            causation_id: e.causation_id,
            entity_id: e.entity_id.as_u32(),
            scope_id: e.scope_id.as_u32(),
            kind: e.kind,
            wall_ms: e.wall_ms,
            clock: e.clock,
            dag_lane: 7,
            dag_depth: 3,
            prev_hash: e.hash_chain.prev_hash,
            event_hash: e.hash_chain.event_hash,
            segment_id: e.disk_pos.segment_id,
            offset: e.disk_pos.offset,
            length: e.disk_pos.length,
            global_sequence: e.global_sequence,
        })
        .collect();
    legacy_entries.sort_by_key(|entry| entry.global_sequence);
    let mut interner_strings = vec![String::new()];
    interner_strings.extend(idx.interner.to_snapshot());
    let mut sorted_entries = idx.all_entries();
    sorted_entries.sort_by_key(|entry| entry.global_sequence);
    let routing = RoutingSummary::from_sorted_entries(
        &sorted_entries,
        recommended_restore_chunk_count(sorted_entries.len()),
    );

    write_legacy_checkpoint_body(
        dir,
        4,
        &LegacyCheckpointDataV4 {
            global_sequence: idx.global_sequence(),
            watermark_segment_id: 0,
            watermark_offset: 0,
            interner_strings,
            routing,
            entries: legacy_entries,
        },
    );

    let loaded = try_load_checkpoint_snapshot(dir).expect("load v4 checkpoint snapshot");
    assert!(
        loaded.entries.iter().all(|entry| entry.dag_lane == 7),
        "PROPERTY: checkpoint v4 must preserve persisted DAG lane coordinates."
    );
    assert!(
        loaded.entries.iter().all(|entry| entry.dag_depth == 3),
        "PROPERTY: checkpoint v4 must preserve persisted DAG depth coordinates."
    );
    assert_eq!(
        loaded.cumulative_reserved_kind_fallbacks,
        ReservedKindFallbackStats::default(),
        "PROPERTY: checkpoint v4 must default missing cumulative reserved-kind fallback stats to empty."
    );
    assert!(
        !loaded.receipt_extensions_hydrated,
        "PROPERTY: checkpoint v4 must require authoritative frame hydration for receipt extensions."
    );
}

#[test]
fn v5_checkpoint_preserves_reserved_stats_and_requires_extension_hydration() {
    #[derive(Serialize)]
    struct LegacyCheckpointEntryV5 {
        #[serde(with = "crate::wire::u128_bytes")]
        event_id: u128,
        #[serde(with = "crate::wire::u128_bytes")]
        correlation_id: u128,
        #[serde(with = "crate::wire::option_u128_bytes")]
        causation_id: Option<u128>,
        entity_id: u32,
        scope_id: u32,
        kind: EventKind,
        wall_ms: u64,
        clock: u32,
        dag_lane: u32,
        dag_depth: u32,
        prev_hash: [u8; 32],
        event_hash: [u8; 32],
        segment_id: u64,
        offset: u64,
        length: u32,
        global_sequence: u64,
    }

    #[derive(Serialize)]
    struct LegacyCheckpointDataV5 {
        global_sequence: u64,
        watermark_segment_id: u64,
        watermark_offset: u64,
        interner_strings: Vec<String>,
        routing: RoutingSummary,
        reserved_kind_fallbacks: ReservedKindFallbackStats,
        entries: Vec<LegacyCheckpointEntryV5>,
    }

    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path();
    touch_segment(dir, 0);

    let idx = make_index(4);
    let mut legacy_entries: Vec<LegacyCheckpointEntryV5> = idx
        .all_entries()
        .into_iter()
        .map(|e| LegacyCheckpointEntryV5 {
            event_id: e.event_id,
            correlation_id: e.correlation_id,
            causation_id: e.causation_id,
            entity_id: e.entity_id.as_u32(),
            scope_id: e.scope_id.as_u32(),
            kind: e.kind,
            wall_ms: e.wall_ms,
            clock: e.clock,
            dag_lane: 5,
            dag_depth: 8,
            prev_hash: e.hash_chain.prev_hash,
            event_hash: e.hash_chain.event_hash,
            segment_id: e.disk_pos.segment_id,
            offset: e.disk_pos.offset,
            length: e.disk_pos.length,
            global_sequence: e.global_sequence,
        })
        .collect();
    legacy_entries.sort_by_key(|entry| entry.global_sequence);
    let mut interner_strings = vec![String::new()];
    interner_strings.extend(idx.interner.to_snapshot());
    let mut sorted_entries = idx.all_entries();
    sorted_entries.sort_by_key(|entry| entry.global_sequence);
    let routing = RoutingSummary::from_sorted_entries(
        &sorted_entries,
        recommended_restore_chunk_count(sorted_entries.len()),
    );
    let reserved_kind_fallbacks = ReservedKindFallbackStats {
        system: 2,
        effect: 1,
        system_histogram: std::iter::once((0x000Au16, 2usize)).collect(),
        effect_histogram: std::iter::once((0x1001u16, 1usize)).collect(),
    };

    write_legacy_checkpoint_body(
        dir,
        5,
        &LegacyCheckpointDataV5 {
            global_sequence: idx.global_sequence(),
            watermark_segment_id: 0,
            watermark_offset: 0,
            interner_strings,
            routing,
            reserved_kind_fallbacks: reserved_kind_fallbacks.clone(),
            entries: legacy_entries,
        },
    );

    let loaded = try_load_checkpoint_snapshot(dir).expect("load v5 checkpoint snapshot");
    assert!(loaded.entries.iter().all(|entry| entry.dag_lane == 5));
    assert!(loaded.entries.iter().all(|entry| entry.dag_depth == 8));
    assert_eq!(
        loaded.cumulative_reserved_kind_fallbacks, reserved_kind_fallbacks,
        "PROPERTY: checkpoint v5 must preserve cumulative reserved-kind fallback stats."
    );
    assert!(
        loaded
            .entries
            .iter()
            .all(|entry| entry.receipt_extensions.is_empty()),
        "PROPERTY: checkpoint v5 does not directly carry receipt-extension maps."
    );
    assert!(
        !loaded.receipt_extensions_hydrated,
        "PROPERTY: checkpoint v5 must require authoritative frame hydration for receipt extensions."
    );
}

#[test]
fn restore_advances_global_sequence() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path();
    touch_segment(dir, 0);

    let src = make_index(16);
    write_checkpoint(&src, dir, 0, 0).expect("write");

    let loaded = try_load_checkpoint(dir).expect("should load");
    let entries = loaded.entries;
    let interner_strings = loaded.interner_strings;
    let stored_alloc = loaded.stored_allocator;
    assert_eq!(entries.len(), 16);

    let dst = StoreIndex::new();
    restore_from_checkpoint(&dst, entries, &interner_strings, stored_alloc).expect("restore");

    // After restoring 16 entries, global_sequence should be 16
    // (each insert() call increments the counter by 1).
    assert_eq!(
        dst.global_sequence(),
        16,
        "PROPERTY: global_sequence after restore must equal the number of restored entries."
    );
    // Visibility watermark must also advance to 16 (restore_from_checkpoint
    // calls publish(global_sequence()) at the end).
    assert_eq!(
        dst.visible_sequence(),
        16,
        "PROPERTY: visible_sequence after restore must equal global_sequence."
    );
}

#[test]
fn to_cold_start_row_normalizes_zero_causation() {
    let entry = CheckpointEntry {
        event_id: 1,
        correlation_id: 1,
        causation_id: Some(0),
        entity_id: 0,
        scope_id: 0,
        kind: EventKind::custom(0x1, 1),
        wall_ms: 1_700_000_000_000,
        clock: 1,
        dag_lane: 0,
        dag_depth: 0,
        prev_hash: [0u8; 32],
        event_hash: [0u8; 32],
        segment_id: 0,
        offset: 0,
        length: 64,
        global_sequence: 0,
        receipt_extensions: BTreeMap::new(),
    };
    let row = entry.to_cold_start_row();
    assert_eq!(
        row.causation_id, None,
        "INVARIANT: Some(0) causation in checkpoint must normalize to None on restore"
    );
}

#[test]
fn to_cold_start_row_preserves_nonzero_causation() {
    let entry = CheckpointEntry {
        event_id: 2,
        correlation_id: 1,
        causation_id: Some(99),
        entity_id: 0,
        scope_id: 0,
        kind: EventKind::custom(0x1, 1),
        wall_ms: 1_700_000_000_000,
        clock: 1,
        dag_lane: 0,
        dag_depth: 0,
        prev_hash: [0u8; 32],
        event_hash: [0u8; 32],
        segment_id: 0,
        offset: 0,
        length: 64,
        global_sequence: 1,
        receipt_extensions: BTreeMap::new(),
    };
    let row = entry.to_cold_start_row();
    assert_eq!(row.causation_id, Some(99));
}
