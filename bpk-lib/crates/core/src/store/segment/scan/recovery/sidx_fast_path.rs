use crate::event::EventKind;
use crate::store::segment::scan::{Reader, ScannedIndexEntry};
use crate::store::segment::{self, sidx::SidxEntry};
use crate::store::StoreError;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::Ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SidxTailCoverage {
    Complete,
    Incomplete,
    Unreadable,
}

impl Reader {
    /// Check whether the SIDX entries cover every frame in the segment up to
    /// the SIDX footer.
    ///
    /// Returns `Complete` when the max (frame_offset + frame_length) across
    /// SIDX entries equals the SIDX footer start, meaning every frame in the
    /// segment is represented. Returns `Incomplete` when there are trailing
    /// frames that SIDX does not know about (the cross-segment batch case), or
    /// when the SIDX claims to cover bytes that overlap the footer itself.
    /// Returns `Unreadable` when disk/footer evidence cannot prove coverage.
    /// Callers frame-scan unless this returns `Complete`.
    pub(super) fn sidx_covers_segment_tail(
        path: &Path,
        sidx_entries: &[SidxEntry],
    ) -> SidxTailCoverage {
        let file_len = match crate::store::platform::fs::metadata(path) {
            Ok(metadata) => metadata.len(),
            Err(_) => return SidxTailCoverage::Unreadable,
        };
        let mut file = match crate::store::platform::fs::open_file(path) {
            Ok(file) => file,
            Err(_) => return SidxTailCoverage::Unreadable,
        };
        if file_len < 16 {
            return SidxTailCoverage::Incomplete;
        }
        if file.seek(SeekFrom::End(-16)).is_err() {
            return SidxTailCoverage::Unreadable;
        }
        let mut trailer = [0u8; 16];
        if file.read_exact(&mut trailer).is_err() {
            return SidxTailCoverage::Unreadable;
        }
        if &trailer[12..16] != crate::store::segment::sidx::SIDX_MAGIC {
            return SidxTailCoverage::Unreadable;
        }
        let offset_bytes: [u8; 8] = match trailer[0..8].try_into() {
            Ok(bytes) => bytes,
            Err(_) => return SidxTailCoverage::Unreadable,
        };
        let sidx_start = u64::from_le_bytes(offset_bytes);

        // R15: compute the max frame tail with checked arithmetic. A garbage or
        // overflowing frame_offset/frame_length pair is corruption, not a value to
        // silently clamp to u64::MAX, so any overflow degrades to Incomplete
        // (frame-scan fallback with CRC verification) rather than saturating.
        let mut max_tail = 0u64;
        for entry in sidx_entries {
            let tail = match entry
                .frame_offset
                .checked_add(u64::from(entry.frame_length))
            {
                Some(tail) => tail,
                None => return SidxTailCoverage::Incomplete,
            };
            if tail > max_tail {
                max_tail = tail;
            }
        }

        if max_tail == sidx_start {
            SidxTailCoverage::Complete
        } else {
            SidxTailCoverage::Incomplete
        }
    }

    pub(super) fn try_sidx_fast_path<F>(
        &self,
        path: &Path,
        segment_id: u64,
        batch_in_progress: bool,
        sink: &mut F,
    ) -> Result<bool, StoreError>
    where
        F: FnMut(ScannedIndexEntry) -> Result<(), StoreError>,
    {
        let is_active = self.active_segment_id.load(Ordering::Acquire) == segment_id;
        if is_active || batch_in_progress {
            return Ok(false);
        }

        match segment::sidx::read_footer(path) {
            Ok(Some((sidx_entries, strings)))
                if Self::sidx_covers_segment_tail(path, &sidx_entries)
                    == SidxTailCoverage::Complete =>
            {
                for se in sidx_entries {
                    let row = se.to_cold_start_row(segment_id);
                    let kind = row.kind;
                    if kind == EventKind::SYSTEM_BATCH_BEGIN
                        || kind == EventKind::SYSTEM_BATCH_COMMIT
                    {
                        continue;
                    }
                    sink(ScannedIndexEntry::from_cold_start_row(&row, &strings)?)?;
                }
                Ok(true)
            }
            Ok(Some(_)) | Ok(None) | Err(_) => Ok(false),
        }
    }
}
