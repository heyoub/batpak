//! PROVES: the cold-start frame scan stays bounded and panic-free against
//! frame-length and footer-boundary pathologies — a u32::MAX frame header on
//! the tail stops the scan while preserving earlier frames, the same length in
//! a NON-tail segment fails closed, a mid-frame truncation never fabricates
//! entries, an SDX3 magic mismatch falls back to the frame scan, and a legacy
//! pre-0.8.3 SDX2 footer (tail and non-tail) still recovers every event via
//! the boundary-aware frame-scan fallback.
//! CATCHES: a scan that allocates against a bogus frame length, panics on a
//! torn frame, over-runs into footer bytes when the SIDX magic is unrecognized,
//! or treats committed-history corruption as a recoverable crash tail.
//! SEEDED: deterministic stores (single tail segment plus rotated multi-segment
//! histories) whose frame-length fields and SIDX magic are surgically mutated.

use batpak_testkit::segment_scan_hardening as ssh_support;

use batpak::coordinate::{Coordinate, Region};
use batpak::event::EventKind;
use batpak::store::segment::{SEGMENT_EXTENSION, SEGMENT_MAGIC};
use batpak::store::{Store, StoreError};
use ssh_support::*;
use tempfile::TempDir;

fn segment_paths_sorted(dir: &TempDir) -> Vec<std::path::PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read data dir")
        .filter_map(|entry| {
            let path = entry.expect("read_dir entry").path();
            (path.extension().and_then(|s| s.to_str()) == Some(SEGMENT_EXTENSION)).then_some(path)
        })
        .collect();
    paths.sort();
    paths
}

/// Strip a CRC-valid SDX3 footer so the cold start walks the slow frame scan.
/// Used by the frame-length poisoner here; inline (not shared) so the support
/// module carries no `dead_code` in the untrusted-offset binary that never
/// strips the footer.
fn strip_sidx(mut bytes: Vec<u8>) -> Vec<u8> {
    if bytes.len() >= 16 && &bytes[bytes.len() - 4..] == b"SDX3" {
        let string_table_offset = u64::from_le_bytes(
            bytes[bytes.len() - 16..bytes.len() - 8]
                .try_into()
                .expect("8-byte SIDX trailer offset"),
        );
        bytes.truncate(
            usize::try_from(string_table_offset).expect("SIDX string table offset fits usize"),
        );
    }
    bytes
}

fn poison_first_frame_length_past_max(seg: &std::path::Path) {
    let mut bytes = strip_sidx(std::fs::read(seg).expect("read segment"));
    let first_frame_offset = frame_scan_header_end(&bytes);
    assert!(
        first_frame_offset + 4 <= bytes.len(),
        "segment must contain a frame header to poison"
    );
    bytes[first_frame_offset..first_frame_offset + 4].copy_from_slice(&u32::MAX.to_be_bytes());
    std::fs::write(seg, bytes).expect("write poisoned segment");
}

/// Rewrite only the trailing 4-byte SIDX magic from the current `SDX3` to the
/// legacy pre-0.8.3 `SDX2`, leaving the whole footer (string table + entries +
/// 16-byte trailer geometry) byte-for-byte intact. This reproduces a real
/// pre-0.8.3 sealed segment on disk: a structurally-valid SIDX footer whose
/// magic the post-bump reader no longer trusts (no CRC32 in the SDX2 format),
/// so `read_footer` returns `Ok(None)` and cold start must fall back to the
/// CRC-verified frame scan.
fn downgrade_sidx_magic_to_sdx2(seg: &std::path::Path) {
    let mut bytes = std::fs::read(seg).expect("read segment");
    let n = bytes.len();
    assert!(n >= 16, "segment must hold the 16-byte SIDX trailer");
    assert_eq!(
        &bytes[n - 4..],
        b"SDX3",
        "seeded segment must carry the current SDX3 SIDX magic before downgrade"
    );
    bytes[n - 4..].copy_from_slice(b"SDX2");
    std::fs::write(seg, bytes).expect("write SDX2-downgraded segment");
}

#[test]
fn pathological_frame_length_is_bounded_not_panicking() {
    // Seed a segment with several real frames, then overwrite a frame-header
    // length field with the u32::MAX sentinel. The scan must see the length
    // exceeds MAX_FRAME_PAYLOAD (256 MB), log a warning, and stop scanning
    // — preserving every earlier frame.
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 4);

    let seg = segment_path(&dir);
    let mut bytes = std::fs::read(&seg).expect("read segment");
    assert_eq!(
        &bytes[..SEGMENT_MAGIC.len()],
        SEGMENT_MAGIC,
        "seeded segment must start with the canonical segment magic"
    );
    assert!(
        bytes.len() >= 16,
        "segment must have at least a 16-byte SIDX trailer"
    );

    // Strip the SIDX footer so the cold-start walks the slow path.
    // The trailer format is [string_table_offset:u64 LE][count:u32 LE][b"SDX3"].
    let trailer_start = bytes.len() - 16;
    let string_table_offset = u64::from_le_bytes(
        bytes[trailer_start..trailer_start + 8]
            .try_into()
            .expect("8 bytes"),
    );
    bytes.truncate(string_table_offset.try_into().expect("offset fits usize"));

    // Find the first frame header — it lives right after magic(4) +
    // header_len(4) + msgpack header bytes. The msgpack header starts at
    // offset 8; its length is the u32 BE at bytes[4..8].
    let header_len = u32::from_be_bytes(bytes[4..8].try_into().expect("4 bytes")) as usize;
    let first_frame_offset = 8 + header_len;

    // Walk past the first two real frames so at least one user-authored
    // frame remains recoverable before the pathological header even though
    // mutable open now writes a lifecycle event first.
    let first_len = u32::from_be_bytes(
        bytes[first_frame_offset..first_frame_offset + 4]
            .try_into()
            .expect("4 bytes"),
    ) as usize;
    let second_frame_offset = first_frame_offset + 8 + first_len;
    let second_len = u32::from_be_bytes(
        bytes[second_frame_offset..second_frame_offset + 4]
            .try_into()
            .expect("4 bytes"),
    ) as usize;
    let poison_frame_offset = second_frame_offset + 8 + second_len;

    assert!(
        poison_frame_offset + 4 <= bytes.len(),
        "segment must contain a third frame to poison; size={}, target={}",
        bytes.len(),
        poison_frame_offset + 4
    );

    // Overwrite the frame's length field with u32::MAX — far beyond
    // MAX_FRAME_PAYLOAD so the scan terminates immediately.
    bytes[poison_frame_offset..poison_frame_offset + 4].copy_from_slice(&u32::MAX.to_be_bytes());
    std::fs::write(&seg, &bytes).expect("write poisoned segment");

    // Reopen must not panic or error. The scan stops at the poisoned frame.
    let store = Store::open(config(&dir)).expect("reopen with poisoned frame");
    let entries: Vec<_> = store
        .query(&Region::all())
        .into_iter()
        .filter(|entry| {
            !matches!(
                entry.event_kind(),
                EventKind::SYSTEM_OPEN_COMPLETED | EventKind::SYSTEM_CLOSE_COMPLETED
            )
        })
        .collect();

    assert!(
        !entries.is_empty(),
        "PROPERTY: pre-corruption frames must survive a pathological frame-length poison; got 0 entries"
    );
    assert!(
        entries.len() < 4,
        "PROPERTY: poisoning the second frame's length must prevent it and later frames from surfacing; \
         got {} entries (max 3 expected if only the first frame survives)",
        entries.len()
    );

    // The store remains usable.
    let coord = Coordinate::new("entity:scan", "scope:test").expect("valid coord");
    store
        .append(&coord, KIND, &serde_json::json!({"post_poison": true}))
        .expect("append after corrupt reopen");
    store.close().expect("close");
}

#[test]
fn non_tail_pathological_frame_length_fails_closed_on_reopen() {
    // Only the latest existing segment is allowed to use torn-tail recovery.
    // An impossible frame length in older history means committed segment
    // corruption, not a recoverable crash tail.
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(config(&dir).with_segment_max_bytes(512)).expect("open store");
    let coord = Coordinate::new("entity:scan-historical", "scope:test").expect("valid coord");
    for i in 0..40 {
        store
            .append(
                &coord,
                KIND,
                &serde_json::json!({"i": i, "pad": "x".repeat(96)}),
            )
            .expect("append");
    }
    store.close().expect("close");

    let segments = segment_paths_sorted(&dir);
    assert!(
        segments.len() >= 2,
        "test must create historical and latest segments; got {}",
        segments.len()
    );
    poison_first_frame_length_past_max(&segments[0]);

    let err = Store::open(config(&dir).with_segment_max_bytes(512))
        .map(|_| ())
        .expect_err("PROPERTY: non-tail impossible frame length must fail closed during reopen");

    assert!(
        matches!(
            err,
            StoreError::CorruptFrame { ref reason, .. }
            if reason.contains("exceeds MAX_FRAME_PAYLOAD")
        ),
        "PROPERTY: non-tail impossible frame length must surface as CorruptFrame; got {err:?}"
    );
}

#[test]
fn sidx_footer_magic_mismatch_falls_back_to_frame_scan() {
    // Overwriting the SIDX magic is a common real-world corruption: the
    // trailer looks present but does not match the sentinel. The loader
    // must treat it as "no SIDX present" and fall back to the frame scan,
    // which still recovers every frame.
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 8);

    let seg = segment_path(&dir);
    let mut bytes = std::fs::read(&seg).expect("read segment");
    assert_eq!(
        &bytes[bytes.len() - 4..],
        b"SDX3",
        "seeded segment must have the SIDX magic"
    );
    // Corrupt the last byte of the SIDX magic.
    let magic_offset = bytes.len() - 1;
    bytes[magic_offset] = b'Z';
    std::fs::write(&seg, &bytes).expect("write bad-magic segment");

    let store = Store::open(config(&dir)).expect("reopen with SIDX magic corruption");
    let entries: Vec<_> = store
        .query(&Region::all())
        .into_iter()
        .filter(|entry| {
            !matches!(
                entry.event_kind(),
                EventKind::SYSTEM_OPEN_COMPLETED | EventKind::SYSTEM_CLOSE_COMPLETED
            )
        })
        .collect();

    // The frame scan recovers every frame despite the SIDX trailer being
    // unreadable — SIDX is an accelerator, not the durability oracle.
    assert_eq!(
        entries.len(),
        8,
        "PROPERTY: a SIDX magic corruption must fall back to the frame scan without data loss; \
         got {} entries (expected 8)",
        entries.len()
    );
    store.close().expect("close");
}

#[test]
fn truncating_segment_mid_frame_never_panics() {
    // Truncate a segment inside a frame body. The scanner sees an
    // UnexpectedEof on read_exact for the payload and stops cleanly.
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 4);

    let seg = segment_path(&dir);
    let bytes = std::fs::read(&seg).expect("read segment");
    // Strip SIDX trailer first so the scan takes the slow path.
    let trailer_offset = u64::from_le_bytes(
        bytes[bytes.len() - 16..bytes.len() - 8]
            .try_into()
            .expect("8 bytes"),
    );
    let truncated_len = (usize::try_from(trailer_offset).expect("offset fits usize")) / 2;
    std::fs::write(&seg, &bytes[..truncated_len]).expect("write truncated segment");

    let store = Store::open(config(&dir)).expect("reopen with mid-frame truncation");
    let entries = store.query(&Region::all());
    assert!(
        entries.len() <= 4,
        "PROPERTY: truncated segment scan must not fabricate entries; got {} (max 4)",
        entries.len()
    );
    store.close().expect("close");
}

#[test]
fn legacy_sdx2_tail_segment_recovers_all_events_via_frame_scan() {
    // BACKWARD-COMPAT (P1): a pre-0.8.3 sealed segment carries an SDX2 footer
    // with no CRC32. After the SDX2->SDX3 magic bump, `read_footer` refuses to
    // trust SDX2 content (Ok(None)) and cold start frame-scans. The scan must
    // still honor the SDX2 footer's BOUNDARY (string_table_offset) so it stops
    // at the true end of frames instead of over-running into the footer bytes.
    let dir = TempDir::new().expect("temp dir");
    seed_store(&dir, 8);

    let seg = segment_path(&dir);
    downgrade_sidx_magic_to_sdx2(&seg);

    let store = Store::open(config(&dir)).expect("reopen pre-0.8.3 SDX2 tail segment");
    let entries = user_entries(&store);
    assert_eq!(
        entries.len(),
        8,
        "PROPERTY: a pre-0.8.3 SDX2 sealed segment must recover ALL events via the \
         frame-scan fallback; got {} (expected 8)",
        entries.len()
    );
    store.close().expect("close");
}

#[test]
fn legacy_sdx2_non_tail_segment_recovers_all_events_via_frame_scan() {
    // The dangerous case the P1 actually bricked: a NON-TAIL (historical) SDX2
    // segment frame-scans under the fail-closed tail policy. Before the boundary
    // fix, `detect_sidx_boundary` matched only SDX3, returned None for SDX2, set
    // frames_end = file_len, and the scan over-ran into the SDX2 string-table
    // bytes — whose first msgpack byte reads as an oversized frame length,
    // surfacing CorruptFrame and FAILING the entire store reopen. Recognizing
    // the SDX2 magic as a boundary marker makes frames_end land exactly at the
    // end of the frame region, so every committed event is recovered.
    let dir = TempDir::new().expect("temp dir");
    let store =
        Store::open(config(&dir).with_segment_max_bytes(512)).expect("open store for rotation");
    let coord = Coordinate::new("entity:scan-legacy", "scope:test").expect("valid coord");
    for i in 0..40 {
        store
            .append(
                &coord,
                KIND,
                &serde_json::json!({"i": i, "pad": "x".repeat(96)}),
            )
            .expect("append");
    }
    store.close().expect("close");

    let segments = segment_paths_sorted(&dir);
    assert!(
        segments.len() >= 2,
        "test must create at least one historical (non-tail) segment plus a tail; got {}",
        segments.len()
    );
    // Downgrade the FIRST (oldest, non-tail) sealed segment to the SDX2 format.
    downgrade_sidx_magic_to_sdx2(&segments[0]);

    let store = Store::open(config(&dir).with_segment_max_bytes(512))
        .expect("reopen must succeed: a non-tail SDX2 segment must not brick cold start");
    let entries = user_entries(&store);
    assert_eq!(
        entries.len(),
        40,
        "PROPERTY: every event across all segments must survive when an older \
         segment is in the legacy SDX2 format; got {} (expected 40)",
        entries.len()
    );
    store.close().expect("close");
}
