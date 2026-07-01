//! Cold-start artifact write on the explicit close / compaction-tail path.
//!
//! Extracted from `lifecycle.rs` to keep that production file within the
//! non-overridable 850-line size cap after the fork/import + StoreFs-seam
//! additions; behavior is unchanged.

use crate::store::cold_start::{latest_segment_watermark, ColdStartArtifactKind};
use crate::store::{Open, Store, StoreError};

/// Determine watermark from the latest segment file and write the fastest
/// available cold-start artifact. When mmap is enabled it is strictly
/// preferred over checkpoint — writing both is redundant work that doubles
/// close() cost at high event counts.
pub(super) fn write_cold_start_artifacts_on_close(store: &Store<Open>) -> Result<(), StoreError> {
    let (seg_id, offset) = latest_segment_watermark(&store.config.data_dir)?;
    let fs = store.config.fs().as_ref();
    match store.runtime.cold_start.write_target() {
        Some(ColdStartArtifactKind::MmapIndex) => {
            crate::store::cold_start::mmap::write_mmap_index_with_reserved_kind_fallbacks(
                &store.index,
                &store.config.data_dir,
                seg_id,
                offset,
                &store.cumulative_reserved_kind_fallbacks,
                fs,
            )?;
        }
        Some(ColdStartArtifactKind::Checkpoint) => {
            crate::store::cold_start::checkpoint::write_checkpoint_with_reserved_kind_fallbacks(
                &store.index,
                &store.config.data_dir,
                seg_id,
                offset,
                &store.cumulative_reserved_kind_fallbacks,
                fs,
            )?;
        }
        None => {}
    }
    // NOTE: the durable idempotency store is flushed by the CALLERS (close +
    // compaction tail) BEFORE this best-effort artifact refresh, so its error
    // is always propagated and never lost behind a warn-swallowed artifact
    // write. It is a correctness primitive, not a fast-open cache.
    // justifies: INV-IDEMPOTENCY-DURABLE-WINDOW
    Ok(())
}
