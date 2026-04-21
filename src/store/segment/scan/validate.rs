use super::{IndexScanEvent, Reader, FRAME_HEADER_BYTES, MAX_BATCH_RECOVERY_ITEMS};
use crate::event::{EventKind, HashChain};
use crate::store::segment;
use crate::store::StoreError;

impl Reader {
    pub(super) fn checked_frame_len(segment_id: u64, length: u32) -> Result<usize, StoreError> {
        let frame_len = usize::try_from(length).map_err(|_| {
            StoreError::corrupt_frame(segment_id, "stored frame length does not fit in usize")
        })?;
        let max_frame_len = FRAME_HEADER_BYTES + segment::MAX_FRAME_PAYLOAD;
        if frame_len < FRAME_HEADER_BYTES || frame_len > max_frame_len {
            return Err(StoreError::corrupt_frame(
                segment_id,
                format!(
                    "stored frame length {frame_len} is outside valid range [{FRAME_HEADER_BYTES}, {max_frame_len}]"
                ),
            ));
        }
        Ok(frame_len)
    }

    pub(super) fn checked_frame_range(
        segment_id: u64,
        offset: u64,
        length: u32,
        available_len: usize,
    ) -> Result<std::ops::Range<usize>, StoreError> {
        let start = usize::try_from(offset).map_err(|_| StoreError::corrupt_eof(segment_id))?;
        let frame_len = Self::checked_frame_len(segment_id, length)?;
        let end = start
            .checked_add(frame_len)
            .ok_or_else(|| StoreError::corrupt_frame(segment_id, "frame offset overflow"))?;
        if end > available_len {
            return Err(StoreError::corrupt_eof(segment_id));
        }
        Ok(start..end)
    }

    pub(super) fn checked_batch_count(
        segment_id: u64,
        offset: u64,
        batch_count: u32,
    ) -> Result<u32, StoreError> {
        if batch_count == 0 || batch_count > MAX_BATCH_RECOVERY_ITEMS {
            return Err(StoreError::corrupt_frame(
                segment_id,
                format!(
                    "invalid batch marker count {batch_count} at offset {offset}; expected 1..={MAX_BATCH_RECOVERY_ITEMS}"
                ),
            ));
        }
        Ok(batch_count)
    }

    pub(super) fn required_index_hash_chain(
        event: &IndexScanEvent,
        segment_id: u64,
        offset: u64,
    ) -> Result<HashChain, StoreError> {
        match &event.hash_chain {
            Some(chain) => Ok(chain.clone()),
            None if matches!(
                event.header.event_kind,
                EventKind::SYSTEM_BATCH_BEGIN | EventKind::SYSTEM_BATCH_COMMIT
            ) =>
            {
                Err(StoreError::corrupt_frame(
                    segment_id,
                    format!(
                        "batch marker at offset {offset} should not reach hash-chain validation"
                    ),
                ))
            }
            None => Err(StoreError::corrupt_frame(
                segment_id,
                format!(
                    "event at offset {offset} is missing hash_chain; recovery no longer defaults missing hashes"
                ),
            )),
        }
    }
}
