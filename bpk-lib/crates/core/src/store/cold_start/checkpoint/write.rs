use super::{checkpoint_entries_to_index_entries, format, CheckpointEntry};
use crate::store::cold_start::ReservedKindFallbackStats;
use crate::store::index::{recommended_restore_chunk_count, RoutingSummary, StoreIndex};
use crate::store::StoreError;
use std::path::Path;

/// Serialise the entire in-memory index to `<data_dir>/index.ckpt`.
///
/// The write is atomic: data is first written to a same-directory temporary file,
/// fsynced, then renamed over the final path. A partial write caused by a crash
/// therefore never corrupts the previous good checkpoint.
///
/// # Errors
///
/// Returns [`StoreError::Serialization`] if msgpack serialisation fails.
/// Returns [`StoreError::Io`] if any filesystem operation (open, write, fsync,
/// rename) fails.
#[cfg(test)]
pub(crate) fn write_checkpoint(
    index: &StoreIndex,
    data_dir: &Path,
    watermark_segment_id: u64,
    watermark_offset: u64,
) -> Result<(), StoreError> {
    write_checkpoint_with_reserved_kind_fallbacks(
        index,
        data_dir,
        watermark_segment_id,
        watermark_offset,
        &ReservedKindFallbackStats::default(),
    )
}

pub(crate) fn write_checkpoint_with_reserved_kind_fallbacks(
    index: &StoreIndex,
    data_dir: &Path,
    watermark_segment_id: u64,
    watermark_offset: u64,
    reserved_kind_fallbacks: &ReservedKindFallbackStats,
) -> Result<(), StoreError> {
    // all_entries() is not a linearisable snapshot (DashMap limitation), but
    // checkpoints are written from orchestration points that quiesce the writer.
    let mut entries: Vec<CheckpointEntry> = index
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
            receipt_extensions: e.receipt_extensions.clone(),
        })
        .collect();

    entries.sort_by_key(|e| e.global_sequence);

    let mut interner_strings = vec![String::new()];
    interner_strings.extend(index.interner.to_snapshot());
    tracing::debug!(
        "checkpoint: {} entries, {} interned strings",
        entries.len(),
        index.interner.len()
    );

    let routing = RoutingSummary::from_sorted_entries(
        &checkpoint_entries_to_index_entries(&entries, &interner_strings)?,
        recommended_restore_chunk_count(entries.len()),
    );

    let data = format::CheckpointDataV6 {
        global_sequence: index.global_sequence(),
        watermark_segment_id,
        watermark_offset,
        interner_strings,
        routing,
        reserved_kind_fallbacks: reserved_kind_fallbacks.clone(),
        entries,
    };

    let body = format::encode_checkpoint_body(&data)
        .map_err(|e| StoreError::Serialization(Box::new(e)))?;
    format::write_checkpoint_file(data_dir, &body)?;

    tracing::debug!(
        target: "batpak::checkpoint",
        entries = data.global_sequence,
        watermark_segment_id,
        watermark_offset,
        body_bytes = body.len(),
        "checkpoint written"
    );

    Ok(())
}
