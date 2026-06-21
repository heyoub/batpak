use super::*;
use proptest::prelude::*;
use std::io::Cursor;
use tempfile::NamedTempFile;

prop_compose! {
    fn arb_sidx_entry()(
        event_id in any::<u128>(),
        entity_idx in any::<u32>(),
        scope_idx in any::<u32>(),
        kind in any::<u16>(),
        wall_ms in any::<u64>(),
        clock in any::<u32>(),
        dag_lane in any::<u32>(),
        dag_depth in any::<u32>(),
        prev_hash in any::<[u8; 32]>(),
        event_hash in any::<[u8; 32]>(),
        frame_offset in any::<u64>(),
        frame_length in any::<u32>(),
        global_sequence in any::<u64>(),
        correlation_id in any::<u128>(),
        causation_id in any::<u128>(),
    ) -> SidxEntry {
        SidxEntry {
            event_id,
            entity_idx,
            scope_idx,
            kind,
            wall_ms,
            clock,
            dag_lane,
            dag_depth,
            prev_hash,
            event_hash,
            frame_offset,
            frame_length,
            global_sequence,
            correlation_id,
            causation_id,
        }
    }
}

/// Construct a minimal [`SidxEntry`] with deterministic field values.
/// `entity_idx` and `scope_idx` are left at 0; `record()` will overwrite them.
fn sample_entry(n: u8) -> SidxEntry {
    SidxEntry {
        event_id: u128::from(n),
        entity_idx: 0,
        scope_idx: 0,
        kind: kind_to_raw(EventKind::custom(0x1, u16::from(n))),
        wall_ms: 1_000_000 + u64::from(n),
        clock: u32::from(n),
        dag_lane: u32::from(n % 3),
        dag_depth: u32::from(n % 5),
        prev_hash: [n; 32],
        event_hash: [n.wrapping_add(1); 32],
        frame_offset: u64::from(n) * 512,
        frame_length: 128,
        global_sequence: u64::from(n),
        correlation_id: u128::from(n),
        causation_id: 0,
    }
}

// The previous `entry_size_constant_matches_layout` test asserted that
// a `Vec<u8>` you just created with length `ENTRY_SIZE` still has length
// `ENTRY_SIZE` after `encode_into` writes in-place. That's a tautology -
// a `Vec<u8>` cannot change length under an in-place writer. The
// compile-time `_ASSERT_ENTRY_SIZE` const at the top of this file already
// covers the layout invariant. Test deleted in the Tier 1 drill sweep.

// ── encode / decode round-trip ─────────────────────────────────────────────

#[test]
fn encode_decode_round_trip() {
    let original = SidxEntry {
        event_id: 0xDEAD_BEEF_CAFE_1234_5678_9ABC_DEF0_1234_u128,
        entity_idx: 7,
        scope_idx: 3,
        kind: 0xF042,
        wall_ms: 1_700_000_000_000,
        clock: 99,
        dag_lane: 4,
        dag_depth: 2,
        prev_hash: [0xAB; 32],
        event_hash: [0xCD; 32],
        frame_offset: 0x0000_1234_5678_9ABC,
        frame_length: 4096,
        global_sequence: 0xFFFF_FFFF_0000_0001,
        correlation_id: 0x1111_1111_2222_2222_3333_3333_4444_4444_u128,
        causation_id: 0,
    };

    let mut buf = [0u8; ENTRY_SIZE];
    original.encode_into(&mut buf);
    let decoded = SidxEntry::decode_from(&buf, 1).expect("decode must succeed");
    assert_eq!(original, decoded, "round-trip must be lossless");
}

proptest! {
    #[test]
    fn encode_decode_round_trip_property(original in arb_sidx_entry()) {
        let mut buf = [0u8; ENTRY_SIZE];
        original.encode_into(&mut buf);
        let decoded = SidxEntry::decode_from(&buf, 1).expect("decode generated SIDX entry");
        prop_assert_eq!(decoded, original);
    }
}

#[test]
fn reserved_kind_fallback_stats_merge_accumulates_effect_histogram() {
    let mut left = ReservedKindFallbackStats::default();
    left.record_effect(0xD0AA);

    let mut right = ReservedKindFallbackStats::default();
    right.record_effect(0xD0AA);
    right.record_effect(0xD0AA);
    right.record_system(0x00AA);

    left.merge_from(&right);

    assert_eq!(
        left.effect, 3,
        "PROPERTY: effect fallback totals must accumulate across merged SIDX scan shards"
    );
    assert_eq!(
        left.effect_histogram.get(&0xD0AA),
        Some(&3),
        "PROPERTY: effect fallback histograms must add counts rather than subtracting or replacing them"
    );
    assert_eq!(
        left.system, 1,
        "SANITY: merge still carries independent system fallback counts"
    );
    assert_eq!(
        left.system_histogram.get(&0x00AA),
        Some(&1),
        "SANITY: merge still carries independent system fallback histograms"
    );
}

#[test]
fn sidx_entry_to_cold_start_row_preserves_index_and_header_fields() {
    let entry = SidxEntry {
        event_id: 0xDE,
        entity_idx: 1,
        scope_idx: 2,
        kind: kind_to_raw(EventKind::custom(0x6, 0x77)),
        wall_ms: 9_999,
        clock: 12,
        dag_lane: 4,
        dag_depth: 8,
        prev_hash: [0xAB; 32],
        event_hash: [0xCD; 32],
        frame_offset: 512,
        frame_length: 144,
        global_sequence: 123,
        correlation_id: 0xEE,
        causation_id: 0xFA,
    };
    let strings = vec![
        String::new(),
        "entity:sidx".to_owned(),
        "scope:test".to_owned(),
    ];

    let row = entry.to_cold_start_row(7);
    let rebuilt = row
        .to_index_entry(&strings)
        .expect("SIDX row to index entry");
    let header = row.to_event_header();

    assert_eq!(rebuilt.event_id, entry.event_id);
    assert_eq!(rebuilt.correlation_id, entry.correlation_id);
    assert_eq!(rebuilt.causation_id, Some(entry.causation_id));
    assert_eq!(rebuilt.coord.entity(), "entity:sidx");
    assert_eq!(rebuilt.coord.scope(), "scope:test");
    assert_eq!(rebuilt.kind, raw_to_kind(entry.kind));
    assert_eq!(rebuilt.wall_ms, entry.wall_ms);
    assert_eq!(rebuilt.clock, entry.clock);
    assert_eq!(rebuilt.dag_lane, entry.dag_lane);
    assert_eq!(rebuilt.dag_depth, entry.dag_depth);
    assert_eq!(rebuilt.hash_chain.prev_hash, entry.prev_hash);
    assert_eq!(rebuilt.hash_chain.event_hash, entry.event_hash);
    assert_eq!(rebuilt.disk_pos, entry.to_disk_pos(7));
    assert_eq!(rebuilt.global_sequence, entry.global_sequence);
    assert_eq!(header.event_id, crate::id::EventId::from(entry.event_id));
    assert_eq!(
        header.correlation_id,
        crate::id::CorrelationId::from(entry.correlation_id)
    );
    assert_eq!(
        header.causation_id,
        Some(crate::id::CausationId::from(entry.causation_id))
    );
    assert_eq!(header.position.wall_ms, entry.wall_ms);
    assert_eq!(header.position.sequence, entry.clock);
    assert_eq!(header.position.lane, entry.dag_lane);
    assert_eq!(header.position.depth, entry.dag_depth);
    assert_eq!(header.event_kind, raw_to_kind(entry.kind));
}

#[test]
fn sidx_entry_normalizes_zero_causation_to_none() {
    let entry = SidxEntry {
        causation_id: 0,
        ..sample_entry(7)
    };
    let row = entry.to_cold_start_row(11);

    assert_eq!(row.causation_id, None);
    assert_eq!(
        row.disk_pos,
        crate::store::index::DiskPos::new(11, entry.frame_offset, entry.frame_length)
    );
}

// ── kind_to_raw / raw_to_kind / event_kind round-trip ────────────────────

#[test]
fn kind_round_trip_product_kind() {
    let kind = EventKind::custom(0x5, 0x042);
    let raw = kind_to_raw(kind);
    let recovered = raw_to_kind(raw);
    assert_eq!(recovered.category(), kind.category());
    assert_eq!(recovered.type_id(), kind.type_id());
}

#[test]
fn kind_round_trip_system_constants() {
    for &kind in &[
        EventKind::SYSTEM_INIT,
        EventKind::SYSTEM_SHUTDOWN,
        EventKind::SYSTEM_HEARTBEAT,
        EventKind::SYSTEM_CONFIG_CHANGE,
        EventKind::SYSTEM_CHECKPOINT,
        EventKind::SYSTEM_BATCH_BEGIN,
        EventKind::SYSTEM_BATCH_COMMIT,
        EventKind::SYSTEM_OPEN_COMPLETED,
        EventKind::SYSTEM_CLOSE_COMPLETED,
        EventKind::TOMBSTONE,
        EventKind::DATA,
    ] {
        let recovered = raw_to_kind(kind_to_raw(kind));
        assert_eq!(
            kind_to_raw(recovered),
            kind_to_raw(kind),
            "system kind round-trip failed for raw value {:#06x}",
            kind_to_raw(kind)
        );
    }
}

#[test]
fn kind_round_trip_effect_constants() {
    for &kind in &[
        EventKind::EFFECT_ERROR,
        EventKind::EFFECT_RETRY,
        EventKind::EFFECT_ACK,
        EventKind::EFFECT_BACKPRESSURE,
        EventKind::EFFECT_CANCEL,
        EventKind::EFFECT_CONFLICT,
    ] {
        let recovered = raw_to_kind(kind_to_raw(kind));
        assert_eq!(
            kind_to_raw(recovered),
            kind_to_raw(kind),
            "effect kind round-trip failed for raw value {:#06x}",
            kind_to_raw(kind)
        );
    }
}

#[test]
fn event_kind_helper_matches_raw_to_kind() {
    let entry = SidxEntry {
        kind: kind_to_raw(EventKind::custom(0x3, 0x7)),
        ..sample_entry(0)
    };
    let via_helper = entry.event_kind();
    let via_fn = raw_to_kind(entry.kind);
    assert_eq!(kind_to_raw(via_helper), kind_to_raw(via_fn));
}

#[test]
fn raw_to_kind_counted_tracks_reserved_fallbacks() {
    let mut counts = ReservedKindFallbackStats::default();
    assert_eq!(raw_to_kind_counted(0x000A, &mut counts), EventKind::DATA);
    assert_eq!(
        raw_to_kind_counted(0xD0FF, &mut counts),
        EventKind::EFFECT_ERROR
    );
    assert_eq!(counts.system, 1);
    assert_eq!(counts.effect, 1);
    assert_eq!(counts.system_histogram.get(&0x000A), Some(&1));
    assert_eq!(counts.effect_histogram.get(&0xD0FF), Some(&1));
}

// ── intern deduplicates strings ───────────────────────────────────────────

#[test]
fn intern_deduplicates_strings() {
    let mut collector = SidxEntryCollector::new();
    let i0 = collector.intern("entity:1");
    let i1 = collector.intern("scope:default");
    let i2 = collector.intern("entity:1");
    assert_eq!(i0, i2, "same string must return the same index");
    assert_ne!(i0, i1, "different strings must get different indices");
    assert_eq!(
        collector.strings().len(),
        2,
        "only 2 unique strings expected"
    );
}

// ── write_footer / read_footer round-trip ─────────────────────────────────

#[test]
fn footer_round_trip() {
    // Simulate a segment: write dummy frame bytes, then append the SIDX footer.
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(b"FBAT"); // pretend segment magic
    buf.extend_from_slice(&[0u8; 60]); // pretend frames

    let mut cursor = Cursor::new(&mut buf);
    cursor.seek(SeekFrom::End(0)).expect("seek to end");

    let mut collector = SidxEntryCollector::new();
    collector.record(sample_entry(1), "user:1", "profile");
    collector.record(sample_entry(2), "user:2", "profile");

    collector
        .write_footer(&mut cursor, /* segment_id = */ 0)
        .expect("write_footer must succeed");

    // Persist to a temporary file and read back.
    let mut tmp = NamedTempFile::new().expect("create temp file");
    tmp.write_all(&buf).expect("write buf to temp file");
    tmp.flush().expect("flush temp file");

    let (entries, strings) = read_footer(tmp.path())
        .expect("read_footer must not error")
        .expect("SIDX footer must be found");

    assert_eq!(entries.len(), 2, "expected 2 entries");
    assert!(strings.contains(&"user:1".to_owned()));
    assert!(strings.contains(&"user:2".to_owned()));
    assert!(strings.contains(&"profile".to_owned()));

    let e0_entity = &strings[entries[0].entity_idx as usize];
    let e1_entity = &strings[entries[1].entity_idx as usize];
    assert_eq!(e0_entity, "user:1");
    assert_eq!(e1_entity, "user:2");

    // Both entries share the same scope string index.
    assert_eq!(
        entries[0].scope_idx, entries[1].scope_idx,
        "shared scope must use the same string table index"
    );
}

// ── read_footer returns None when no SIDX magic ───────────────────────────

#[test]
fn read_footer_returns_none_without_magic() {
    let mut tmp = NamedTempFile::new().expect("create temp file");
    // Write enough bytes to pass the size guard but with no SIDX magic.
    tmp.write_all(b"FBAT\x00\x00\x00\x00some bytes that are not a sidx footer at all")
        .expect("write");
    tmp.flush().expect("flush");
    let result = read_footer(tmp.path()).expect("must not IO-error");
    assert!(result.is_none(), "non-SIDX file must return None");
}

#[test]
fn read_footer_returns_none_for_old_sidx_magic() {
    let mut tmp = NamedTempFile::new().expect("create temp file");
    tmp.write_all(&[0u8; 12]).expect("write prefix");
    tmp.write_all(b"SIDX").expect("write old magic");
    tmp.flush().expect("flush");

    let result = read_footer(tmp.path()).expect("must not IO-error");
    assert!(result.is_none(), "old SIDX magic must fall back cleanly");
}

// ── read_footer returns None for files smaller than TRAILER_SIZE ──────────

#[test]
fn read_footer_returns_none_for_tiny_file() {
    let mut tmp = NamedTempFile::new().expect("create temp file");
    tmp.write_all(b"AB").expect("write");
    tmp.flush().expect("flush");
    let result = read_footer(tmp.path()).expect("must not IO-error");
    assert!(result.is_none(), "tiny file must return None");
}

// ── read_footer returns None for an empty file ────────────────────────────

#[test]
fn read_footer_returns_none_for_empty_file() {
    let tmp = NamedTempFile::new().expect("create temp file");
    let result = read_footer(tmp.path()).expect("must not IO-error");
    assert!(result.is_none(), "empty file must return None");
}

#[test]
fn read_footer_allows_empty_string_table_range_to_reach_decoder() {
    // 32 bytes of pre-footer "frames", then an SDX3 footer with
    // string_table_offset == entries_start (empty string table, zero entries).
    // The CRC32 covers the empty [string_table_offset .. entries_start) region,
    // so it is the CRC of an empty byte slice. With a matching CRC the layout
    // validates and the empty string-table range reaches the msgpack decoder.
    let mut bytes = vec![0xA5; 32];
    bytes.extend_from_slice(&crc32fast::hash(&[]).to_le_bytes());
    bytes.extend_from_slice(&32u64.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(SIDX_MAGIC);

    let mut tmp = NamedTempFile::new().expect("create temp file");
    tmp.write_all(&bytes).expect("write malformed footer");
    tmp.flush().expect("flush malformed footer");

    let err = read_footer(tmp.path()).expect_err("empty string table bytes are malformed");
    assert!(
        matches!(err, StoreError::Serialization(_)),
        "PROPERTY: string_table_offset == entries_start is a valid range boundary; malformed empty bytes must reach the MessagePack decoder instead of being rejected as an offset-overlap corruption"
    );
}

// ── string table interning across multiple entries ────────────────────────

#[test]
fn shared_string_table_is_compact() {
    let mut collector = SidxEntryCollector::new();
    // Three events in the same entity + scope -> string table should have exactly 2 entries.
    for n in 0u8..3 {
        collector.record(sample_entry(n), "order:999", "payments");
    }
    assert_eq!(
        collector.strings().len(),
        2,
        "only 'order:999' and 'payments' should appear in the table"
    );
    // All entries must share the same pair of indices.
    let unique_pairs: std::collections::HashSet<(u32, u32)> = collector
        .entries()
        .iter()
        .map(|e| (e.entity_idx, e.scope_idx))
        .collect();
    assert_eq!(
        unique_pairs.len(),
        1,
        "all entries sharing entity+scope must have identical index pairs"
    );
}

// ── decode_from rejects a wrong-sized buffer ──────────────────────────────

#[test]
fn decode_from_rejects_wrong_size() {
    let short = vec![0u8; ENTRY_SIZE - 1];
    assert!(
        SidxEntry::decode_from(&short, 42).is_err(),
        "decode_from must error when buffer is too short"
    );

    let long = vec![0u8; ENTRY_SIZE + 1];
    assert!(
        SidxEntry::decode_from(&long, 42).is_err(),
        "decode_from must error when buffer is too long"
    );
}

// ── zero-entry footer round-trip ──────────────────────────────────────────

#[test]
fn footer_round_trip_zero_entries() {
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(&[0u8; 32]); // pretend frames

    let mut cursor = Cursor::new(&mut buf);
    cursor.seek(SeekFrom::End(0)).expect("seek to end");

    let collector = SidxEntryCollector::new();
    collector
        .write_footer(&mut cursor, /* segment_id = */ 0)
        .expect("write_footer must succeed");

    let mut tmp = NamedTempFile::new().expect("create temp file");
    tmp.write_all(&buf).expect("write");
    tmp.flush().expect("flush");

    let (entries, strings) = read_footer(tmp.path())
        .expect("read_footer must not error")
        .expect("footer must be found");

    assert!(entries.is_empty(), "zero entries expected");
    assert!(
        strings.is_empty(),
        "zero strings expected for empty collector"
    );
}

// ── CRC integrity: a flipped covered byte must read as None (frame-scan) ───

#[test]
fn read_footer_returns_none_on_crc_mismatch() {
    // Build a real footer via write_footer, then flip a single byte inside the
    // entries block (the CRC-covered region) while leaving the 4-byte CRC, the
    // 16-byte trailer, and the SDX3 magic intact. read_footer must detect the
    // mismatch and return Ok(None) so the consumer degrades to the CRC-verified
    // frame-scan rebuild rather than trusting corrupted bytes.
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(b"FBAT");
    buf.extend_from_slice(&[0u8; 60]); // pretend frames

    let mut cursor = Cursor::new(&mut buf);
    cursor.seek(SeekFrom::End(0)).expect("seek to end");

    let mut collector = SidxEntryCollector::new();
    collector.record(sample_entry(1), "user:1", "profile");
    collector.record(sample_entry(2), "user:2", "profile");
    collector
        .write_footer(&mut cursor, /* segment_id = */ 0)
        .expect("write_footer must succeed");

    // Footer tail layout: [...entries][crc:4][string_table_offset:8][entry_count:4][magic:4].
    // Flip a byte well inside the entries block: one ENTRY_SIZE before the CRC.
    let crc_start = buf.len() - 16 - 4;
    let flip_at = crc_start - ENTRY_SIZE; // first byte of the last entry
    buf[flip_at] ^= 0xFF;

    // Sanity: trailer + magic + CRC bytes are untouched.
    assert_eq!(
        &buf[buf.len() - 4..],
        SIDX_MAGIC,
        "magic must remain intact after the entry-byte flip"
    );

    let mut tmp = NamedTempFile::new().expect("create temp file");
    tmp.write_all(&buf).expect("write");
    tmp.flush().expect("flush");

    let result = read_footer(tmp.path()).expect("read_footer must not IO-error");
    assert!(
        result.is_none(),
        "PROPERTY: a CRC mismatch over the covered region must read as None (frame-scan fallback), never trusting the bytes"
    );
}

// ── pre-0.8.3 SDX2 footers fall back cleanly ──────────────────────────────

#[test]
fn read_footer_returns_none_for_sdx2_magic() {
    // A pre-0.8.3 footer carries the old SDX2 magic and no CRC. It must read as
    // None on first reopen so the consumer rebuilds via CRC-verified frame scan,
    // mirroring the SIDX->SDX2 old-magic precedent. The magic gate alone rejects
    // it before any CRC math runs.
    let mut tmp = NamedTempFile::new().expect("create temp file");
    tmp.write_all(&[0u8; 12]).expect("write trailer prefix");
    tmp.write_all(b"SDX2").expect("write old SDX2 magic");
    tmp.flush().expect("flush");

    let result = read_footer(tmp.path()).expect("read_footer must not IO-error");
    assert!(
        result.is_none(),
        "PROPERTY: an old SDX2 footer must fall back cleanly to None, not be trusted as an SDX3 footer"
    );
}
