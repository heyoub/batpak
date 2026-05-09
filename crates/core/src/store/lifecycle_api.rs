use super::*;

impl Store<Open> {
    /// LIFECYCLE
    ///
    /// # Errors
    /// Returns `StoreError::Io` if flushing the active segment to disk fails.
    pub fn sync(&self) -> Result<(), StoreError> {
        lifecycle::sync(self)
    }

    /// Block until the durable frontier reaches `point` or `timeout` elapses.
    ///
    /// # Errors
    /// Returns [`StoreError::WaitTimeout`] if `durable_hlc` does not reach
    /// `point` before `timeout`. Returns [`StoreError::WriterCrashed`] if the
    /// writer panicked while the caller was waiting.
    pub fn wait_for_durable(
        &self,
        point: HlcPoint,
        timeout: std::time::Duration,
    ) -> Result<(), StoreError> {
        self.watermark_handle.wait_for_durable(point, timeout)
    }

    /// Block until the applied frontier reaches `point` or `timeout` elapses.
    ///
    /// `applied_hlc` is the minimum applied HLC across registered projections,
    /// so a single lagging projection can keep this wait blocked.
    ///
    /// # Errors
    /// Returns [`StoreError::WaitTimeout`] if `applied_hlc` does not reach
    /// `point` before `timeout`. Returns [`StoreError::WriterCrashed`] if the
    /// writer panicked while the caller was waiting.
    pub fn wait_for_applied(
        &self,
        point: HlcPoint,
        timeout: std::time::Duration,
    ) -> Result<(), StoreError> {
        self.watermark_handle.wait_for_applied(point, timeout)
    }

    /// Block until the visible frontier reaches `point` or `timeout` elapses.
    ///
    /// # Errors
    /// Returns [`StoreError::WaitTimeout`] if `visible_hlc` does not reach
    /// `point` before `timeout`. Returns [`StoreError::WriterCrashed`] if the
    /// writer panicked while the caller was waiting.
    pub fn wait_for_visible(
        &self,
        point: HlcPoint,
        timeout: std::time::Duration,
    ) -> Result<(), StoreError> {
        self.watermark_handle.wait_for_visible(point, timeout)
    }

    /// Snapshot the current index to a destination directory.
    ///
    /// # Errors
    /// Returns `StoreError::Io` if creating the destination directory or copying segment files fails.
    pub fn snapshot(&self, dest: &std::path::Path) -> Result<(), StoreError> {
        lifecycle::snapshot(self, dest)
    }

    /// Compact: merge sealed segments, optionally filtering events.
    /// The active (currently-written) segment is never touched.
    ///
    /// # F6 / FREEZE-4 swap contract
    ///
    /// The in-memory index is rebuilt off-side from the post-merge segment
    /// layout and then published as a single atomic swap under an exclusive
    /// lock (see `StoreIndex::replace_contents_from_fresh`). Reader-facing
    /// methods (`query`, `stream`, `cursor_guaranteed` polls, etc.) take a
    /// read guard on the same lock, so a concurrent reader observes either
    /// the pre-compact index or the post-compact index — never a cleared or
    /// partially rebuilt view.
    ///
    /// Failure modes are surfaced through the returned
    /// [`segment::CompactionResult`]:
    ///
    /// * [`segment::CompactionOutcome::Performed`] — the segment merge
    ///   happened and the live index has been swapped for the fresh one.
    /// * [`segment::CompactionOutcome::Skipped`] — the sealed-segment count
    ///   was below `min_segments`; no disk or index work was done.
    /// * [`segment::CompactionOutcome::Failed`] — the off-side rebuild
    ///   aborted before the swap point; the live index has not been
    ///   mutated, and the pending-compaction marker preserves a coherent
    ///   reopen path until cleanup completes.
    ///
    /// Appends that arrive during compaction are safe (they go to the active
    /// segment which is not compacted). `sync()` is called before and after
    /// the segment merge so the off-side rebuild sees a quiescent on-disk
    /// state; for maximum safety, avoid high-throughput appends during
    /// compaction.
    ///
    /// # Errors
    /// Returns `StoreError::Io` if reading, writing, or removing segment
    /// files fails. A rebuild failure is NOT an error — it is reported via
    /// `CompactionOutcome::Failed`.
    pub fn compact(
        &self,
        config: &CompactionConfig,
    ) -> Result<crate::store::segment::CompactionResult, StoreError> {
        lifecycle::compact(self, config).map(|(result, _report)| result)
    }

    /// Same as [`Store::compact`], plus a deterministic structural
    /// [`CompactionReportBody`] for evidence.
    ///
    /// # Errors
    /// Same error paths as [`Store::compact`].
    pub fn compact_with_report(
        &self,
        config: &CompactionConfig,
    ) -> Result<
        (
            crate::store::segment::CompactionResult,
            CompactionReportBody,
        ),
        StoreError,
    > {
        lifecycle::compact(self, config)
    }

    /// LIFECYCLE: flush pending writes and shut down the writer thread cleanly.
    ///
    /// # Errors
    /// Returns `StoreError::WriterCrashed` if the writer thread has already exited unexpectedly.
    pub fn close(self) -> Result<Closed, StoreError> {
        lifecycle::close(self)
    }
}
