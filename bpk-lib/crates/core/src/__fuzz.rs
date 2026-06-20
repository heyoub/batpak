//! Fuzz-only decode entry points (GAUNT-FUZZ-1).
//!
//! This module exists **solely** so the workspace-excluded `batpak-fuzz`
//! cargo-fuzz crate (a path dependency built with
//! `--features dangerous-test-hooks`) can reach the **real** on-disk / untrusted
//! DECODE entry points of the store — with no copies. Every wrapper here calls
//! production code directly so a libFuzzer crash is a crash in real parse logic.
//!
//! The whole module is gated behind `#[cfg(feature = "dangerous-test-hooks")]`
//! and `#[doc(hidden)]`, so:
//!   * a default build never compiles it (no production API-surface change), and
//!   * even with the feature on it never appears in published docs.
//!
//! ## Contract for the fuzz target authors
//!
//! Each `__fuzz_*` function takes arbitrary `&[u8]` (plus a small scalar where the
//! decoder needs one) and **must never panic by construction beyond what the
//! decoder itself does** — it simply forwards to the real decoder and returns its
//! `Result`/`Option`/discriminant. The fuzz target asserts no-panic; these
//! wrappers add no assertions of their own. Some return types of the underlying
//! decoders are crate-private, so those wrappers collapse the success value to a
//! `bool` / `&'static str` discriminant — the fuzz contract is "does decoding this
//! arbitrary buffer panic", not "what did it decode to".
//!
//! File-path decoders (those that take a directory / open a file rather than a
//! `&[u8]`) get a wrapper that writes the bytes to a freshly-created `tempfile`
//! tree under the real on-disk filename, calls the real loader, and drops the
//! tempfile on return (RAII cleanup). `tempfile` is a normal `[dependencies]`
//! entry of this crate, so it is available in `src` without any feature plumbing.

use crate::store::StoreError;

// ---------------------------------------------------------------------------
// Direct `&[u8]` decoders.
// ---------------------------------------------------------------------------

/// `encoding::from_bytes::<SegmentHeader>(&[u8])`.
///
/// `encoding::from_bytes` and `store::segment::SegmentHeader` are both already
/// `pub`, so a fuzz target *could* call them directly; this wrapper pins the
/// concrete monomorphization (`SegmentHeader`) into one stable entry point and
/// collapses the decoded header to `()` so the target need not name the header
/// type. Returns the real `rmp_serde` decode error on failure.
#[doc(hidden)]
pub fn __fuzz_segment_header(bytes: &[u8]) -> Result<(), rmp_serde::decode::Error> {
    crate::encoding::from_bytes::<crate::store::segment::SegmentHeader>(bytes).map(|_| ())
}

/// `SidxEntry::decode_from(&[u8], segment_id)` (segment/sidx.rs).
///
/// `SidxEntry` and its decoder are `pub(crate)`; the decoded entry type cannot
/// appear in a `pub fn` signature, so this wrapper discards it and returns
/// `Result<(), StoreError>`. The required SIDX entry buffer length is a fixed
/// `ENTRY_SIZE` (162 bytes); a buffer of any other length is the canonical typed
/// `Err` path. `segment_id` is a free scalar fed straight through to the decoder
/// — the fuzz target can pass any `u64` (e.g. `0`).
#[doc(hidden)]
pub fn __fuzz_sidx_entry(buf: &[u8], segment_id: u64) -> Result<(), StoreError> {
    crate::store::segment::sidx::SidxEntry::decode_from(buf, segment_id).map(|_| ())
}

/// `decode_checkpoint_data(path, version, &[u8])` (cold_start/checkpoint).
///
/// Wraps the `pub(super)` decoder via the crate-visible
/// `checkpoint::__fuzz_decode_checkpoint_data` shim, which supplies a throwaway
/// `Path` internally. `version` selects the checkpoint body version; `body` is
/// the msgpack body. Returns `true` when the decoder produced `Some`, `false`
/// when it returned `None` (the typed "ignore corrupt checkpoint" path).
#[doc(hidden)]
pub fn __fuzz_checkpoint_data(version: u16, body: &[u8]) -> bool {
    crate::store::cold_start::checkpoint::__fuzz_decode_checkpoint_data(version, body)
}

/// `decode_checkpoint_snapshot_v6(path, &[u8])` (cold_start/checkpoint).
///
/// Wraps the `pub(super)` v6 snapshot decoder via the crate-visible shim. Returns
/// `true` for `Some`, `false` for `None`.
#[doc(hidden)]
pub fn __fuzz_checkpoint_snapshot_v6(body: &[u8]) -> bool {
    crate::store::cold_start::checkpoint::__fuzz_decode_checkpoint_snapshot_v6(body)
}

/// `MmapIndexEntry::decode_from(&[u8], version)` (cold_start/mmap).
///
/// Wraps the `pub(super)` fixed-width mmap entry decoder via the crate-visible
/// `mmap::__fuzz_decode_mmap_entry` shim. Returns `true` for a successful decode,
/// `false` for the typed `Err` path. `version` is fed straight through.
#[doc(hidden)]
pub fn __fuzz_mmap_entry(buf: &[u8], version: u16) -> bool {
    crate::store::cold_start::mmap::__fuzz_decode_mmap_entry(buf, version)
}

/// `CacheMeta::decode_from_bytes(&[u8])` (projection/mod.rs).
///
/// `CacheMeta` is `pub`, so the real `(remaining_bytes, CacheMeta)` tuple can be
/// returned directly. The decoder splits a small fixed-size meta header off the
/// front and returns the trailing state bytes alongside the parsed meta.
#[doc(hidden)]
pub fn __fuzz_cache_meta(
    bytes: &[u8],
) -> Result<(Vec<u8>, crate::store::projection::CacheMeta), StoreError> {
    crate::store::projection::CacheMeta::decode_from_bytes(bytes)
}

/// Representative concrete state for `decode_cached_state::<T>` fuzzing.
///
/// `decode_cached_state<T>` (projection/flow/mod.rs) is generic over
/// `T: serde::de::DeserializeOwned` and its body is exactly
/// `serde_json::from_slice::<T>(bytes)` — the decode path is identical for every
/// `T`. The crate's real projection-state types are all test-local (private), so
/// there is no public production state type to monomorphize on; this struct
/// stands in for a typical projection state (a couple of scalar counters plus a
/// map), exercising the same monomorphized `serde_json::from_slice` decode path
/// the production callers hit. Documented as the chosen `T` for the contract.
#[derive(Debug, serde::Deserialize)]
#[doc(hidden)]
pub struct FuzzProjectionState {
    /// A monotonic counter, the most common projection-state shape.
    pub count: u64,
    /// A signed accumulator, exercises numeric coercion paths.
    pub balance: i64,
    /// A per-key map, exercises the nested-collection decode path.
    pub by_key: std::collections::BTreeMap<String, u64>,
}

/// `decode_cached_state::<T>(entity, &[u8], warn)` (projection/flow/mod.rs),
/// monomorphized on [`FuzzProjectionState`].
///
/// `decode_cached_state` is a private free fn; this wrapper re-implements its
/// one-line body against the **same** representative `T` so the fuzz crate drives
/// the identical `serde_json::from_slice` decode path the production callers use.
/// Returns `true` when the JSON deserialized, `false` on the warn-and-`None`
/// path. (The underlying fn is private and cannot be re-exported; mirroring its
/// trivial body here keeps the fuzzed code path byte-for-byte identical.)
#[doc(hidden)]
pub fn __fuzz_projection_state(bytes: &[u8]) -> bool {
    serde_json::from_slice::<FuzzProjectionState>(bytes).is_ok()
}

// ---------------------------------------------------------------------------
// File-path decoders. The wrapper owns a `tempfile::TempDir`/`NamedTempFile`,
// writes the untrusted bytes to the real on-disk filename, calls the real
// loader, and drops the tempfile on return.
// ---------------------------------------------------------------------------

/// `load_cancelled_ranges(dir)` (hidden_ranges.rs).
///
/// Writes `data` to `<tmp>/visibility_ranges.fbv` (the real
/// `VISIBILITY_RANGES_FILENAME`) inside a throwaway `TempDir`, then calls the real
/// loader. The loader returns `Ok(Some(ranges))` on a valid file, `Ok(None)` when
/// absent (never here), or a typed `Err` on corruption. The success value is
/// collapsed to `Result<bool, StoreError>` (`bool` = "ranges present"). Returns
/// the I/O error as a `StoreError::Io` if the temp write itself fails (it should
/// not, but the wrapper never panics).
#[doc(hidden)]
pub fn __fuzz_hidden_ranges(data: &[u8]) -> Result<bool, StoreError> {
    let dir = tempfile::tempdir().map_err(StoreError::Io)?;
    let path = dir
        .path()
        .join(crate::store::hidden_ranges::VISIBILITY_RANGES_FILENAME);
    std::fs::write(&path, data).map_err(StoreError::Io)?;
    crate::store::hidden_ranges::load_cancelled_ranges(dir.path()).map(|ranges| ranges.is_some())
}

/// `load_mmap_index(dir, &clock)` (cold_start/mmap/load.rs).
///
/// Writes `data` to `<tmp>/index.fbati` (the real `MMAP_INDEX_FILENAME`) inside a
/// throwaway `TempDir`, then calls the real loader via the crate-visible
/// `mmap::__fuzz_load_mmap_index` shim (which supplies a real `SystemClock`). The
/// loader's private `FileLoad` outcome is returned as a stable `&'static str`
/// discriminant: `"missing" | "loaded" | "invalid" | "future_version"`. Returns
/// `"io_error"` if the temp write itself fails. Never panics.
#[doc(hidden)]
pub fn __fuzz_mmap_index_load(data: &[u8]) -> &'static str {
    let Ok(dir) = tempfile::tempdir() else {
        return "io_error";
    };
    let path = dir
        .path()
        .join(crate::store::cold_start::mmap::MMAP_INDEX_FILENAME);
    if std::fs::write(&path, data).is_err() {
        return "io_error";
    }
    crate::store::cold_start::mmap::__fuzz_load_mmap_index(dir.path())
}

/// `footer::read_layout(&mut Read+Seek, seg_id)` then
/// `read_entries_unauthenticated` (segment/sidx/footer.rs).
///
/// Writes `data` to a throwaway `NamedTempFile`, opens it `Read + Seek`, and
/// drives BOTH footer-parse paths on the same untrusted file:
///   * `authenticated_string_table_offset` — which internally calls
///     `footer::read_layout` (the private footer fns are reachable only through
///     these `pub(crate)` sidx-module wrappers), and
///   * `read_entries_unauthenticated`.
///
/// Both are seeked from the file start before each call. Returns
/// `Result<(bool, usize), StoreError>` = `(layout authenticated an offset,
/// number of unauthenticated entries parsed)`. `segment_id` is fed through to
/// both; the fuzz target can pass any `u64`.
#[doc(hidden)]
pub fn __fuzz_sidx_footer(data: &[u8], segment_id: u64) -> Result<(bool, usize), StoreError> {
    use std::io::{Seek, SeekFrom, Write};

    let mut file = tempfile::NamedTempFile::new().map_err(StoreError::Io)?;
    file.write_all(data).map_err(StoreError::Io)?;
    file.flush().map_err(StoreError::Io)?;
    let f = file.as_file_mut();

    f.seek(SeekFrom::Start(0)).map_err(StoreError::Io)?;
    let layout_authenticated =
        crate::store::segment::sidx::authenticated_string_table_offset(f, segment_id)?.is_some();

    f.seek(SeekFrom::Start(0)).map_err(StoreError::Io)?;
    let entries = crate::store::segment::sidx::read_entries_unauthenticated(f, segment_id)?;

    Ok((layout_authenticated, entries.len()))
}
