use crate::event::EventKind;
use crate::store::segment::scan::{Reader, ScannedIndexEntry};
use crate::store::segment::{self, sidx::SidxEntry};
use crate::store::StoreError;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::Ordering;

impl Reader {
    /// Check whether the SIDX entries cover every frame in the segment up to
    /// the SIDX footer.
    ///
    /// Returns `Some(true)` when the max (frame_offset + frame_length)
    /// across SIDX entries equals the SIDX footer start — meaning every
    /// frame in the segment is represented. Returns `Some(false)` when
    /// there are trailing frames that SIDX doesn't know about (the
    /// cross-segment batch case), or when the SIDX claims to cover bytes
    /// that overlap the footer itself. Returns `None` on I/O trouble;
    /// callers interpret as "can't prove coverage, frame-scan to be safe".
    pub(super) fn sidx_covers_segment_tail(
        path: &Path,
        sidx_entries: &[SidxEntry],
    ) -> Option<bool> {
        let file_len = std::fs::metadata(path).ok()?.len();
        let mut file = crate::store::platform::fs::open_file(path).ok()?;
        if file_len < 16 {
            return Some(false);
        }
        file.seek(SeekFrom::End(-16)).ok()?;
        let mut trailer = [0u8; 16];
        file.read_exact(&mut trailer).ok()?;
        if &trailer[12..16] != crate::store::segment::sidx::SIDX_MAGIC {
            return None;
        }
        let offset_bytes: [u8; 8] = trailer[0..8].try_into().ok()?;
        let sidx_start = u64::from_le_bytes(offset_bytes);

        let max_tail = sidx_entries
            .iter()
            .map(|e| e.frame_offset.saturating_add(u64::from(e.frame_length)))
            .max()
            .unwrap_or(0);

        Some(max_tail == sidx_start)
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

        if let Ok(Some((sidx_entries, strings))) = segment::sidx::read_footer(path) {
            let sidx_covers_tail =
                Self::sidx_covers_segment_tail(path, &sidx_entries).unwrap_or(false);
            if sidx_covers_tail {
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
                return Ok(true);
            }
        }

        Ok(false)
    }
}
