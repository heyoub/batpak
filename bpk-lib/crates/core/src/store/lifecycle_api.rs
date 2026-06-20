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

    /// Block until the accepted frontier reaches `point` or `timeout` elapses.
    ///
    /// # Errors
    /// Returns [`StoreError::WaitTimeout`] if `accepted_hlc` does not reach
    /// `point` before `timeout`. Returns [`StoreError::WriterCrashed`] if the
    /// writer panicked while the caller was waiting.
    pub fn wait_for_accepted(
        &self,
        point: HlcPoint,
        timeout: std::time::Duration,
    ) -> Result<(), StoreError> {
        self.watermark_handle.wait_for_accepted(point, timeout)
    }

    /// Block until one lane's logical accepted frontier reaches `point`.
    ///
    /// # Errors
    /// Returns [`StoreError::WaitTimeout`] if that lane's accepted frontier does
    /// not reach `point` before `timeout`. Returns [`StoreError::WriterCrashed`]
    /// if the writer panicked while the caller was waiting.
    pub fn wait_for_accepted_lane(
        &self,
        lane: u32,
        point: HlcPoint,
        timeout: std::time::Duration,
    ) -> Result<(), StoreError> {
        self.watermark_handle
            .wait_for_accepted_on_lane(lane, point, timeout)
    }

    /// Block until the written frontier reaches `point` or `timeout` elapses.
    ///
    /// # Errors
    /// Returns [`StoreError::WaitTimeout`] if `written_hlc` does not reach
    /// `point` before `timeout`. Returns [`StoreError::WriterCrashed`] if the
    /// writer panicked while the caller was waiting.
    pub fn wait_for_written(
        &self,
        point: HlcPoint,
        timeout: std::time::Duration,
    ) -> Result<(), StoreError> {
        self.watermark_handle.wait_for_written(point, timeout)
    }

    /// Block until one lane's logical written frontier reaches `point`.
    ///
    /// # Errors
    /// Returns [`StoreError::WaitTimeout`] if that lane's written frontier does
    /// not reach `point` before `timeout`. Returns [`StoreError::WriterCrashed`]
    /// if the writer panicked while the caller was waiting.
    pub fn wait_for_written_lane(
        &self,
        lane: u32,
        point: HlcPoint,
        timeout: std::time::Duration,
    ) -> Result<(), StoreError> {
        self.watermark_handle
            .wait_for_written_on_lane(lane, point, timeout)
    }

    /// Block until one lane's logical durable frontier reaches `point`.
    ///
    /// # Errors
    /// Returns [`StoreError::WaitTimeout`] if that lane's durable frontier does
    /// not reach `point` before `timeout`. Returns [`StoreError::WriterCrashed`]
    /// if the writer panicked while the caller was waiting.
    pub fn wait_for_durable_lane(
        &self,
        lane: u32,
        point: HlcPoint,
        timeout: std::time::Duration,
    ) -> Result<(), StoreError> {
        self.watermark_handle
            .wait_for_durable_on_lane(lane, point, timeout)
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

    /// Block until one lane's logical applied frontier reaches `point`.
    ///
    /// # Errors
    /// Returns [`StoreError::WaitTimeout`] if that lane's applied frontier does
    /// not reach `point` before `timeout`. Returns [`StoreError::WriterCrashed`]
    /// if the writer panicked while the caller was waiting.
    pub fn wait_for_applied_lane(
        &self,
        lane: u32,
        point: HlcPoint,
        timeout: std::time::Duration,
    ) -> Result<(), StoreError> {
        self.watermark_handle
            .wait_for_applied_on_lane(lane, point, timeout)
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

    /// Block until one lane's logical visible frontier reaches `point`.
    ///
    /// # Errors
    /// Returns [`StoreError::WaitTimeout`] if that lane's visible frontier does
    /// not reach `point` before `timeout`. Returns [`StoreError::WriterCrashed`]
    /// if the writer panicked while the caller was waiting.
    pub fn wait_for_visible_lane(
        &self,
        lane: u32,
        point: HlcPoint,
        timeout: std::time::Duration,
    ) -> Result<(), StoreError> {
        self.watermark_handle
            .wait_for_visible_on_lane(lane, point, timeout)
    }

    /// Block until the emitted frontier reaches `point` or `timeout` elapses.
    ///
    /// # Errors
    /// Returns [`StoreError::WaitTimeout`] if `emitted_hlc` does not reach
    /// `point` before `timeout`. Returns [`StoreError::WriterCrashed`] if the
    /// writer panicked while the caller was waiting.
    pub fn wait_for_emitted(
        &self,
        point: HlcPoint,
        timeout: std::time::Duration,
    ) -> Result<(), StoreError> {
        self.watermark_handle.wait_for_emitted(point, timeout)
    }

    /// Block until one lane's logical emitted frontier reaches `point`.
    ///
    /// # Errors
    /// Returns [`StoreError::WaitTimeout`] if that lane's emitted frontier does
    /// not reach `point` before `timeout`. Returns [`StoreError::WriterCrashed`]
    /// if the writer panicked while the caller was waiting.
    pub fn wait_for_emitted_lane(
        &self,
        lane: u32,
        point: HlcPoint,
        timeout: std::time::Duration,
    ) -> Result<(), StoreError> {
        self.watermark_handle
            .wait_for_emitted_on_lane(lane, point, timeout)
    }

    /// Snapshot the current index to a destination directory and return
    /// deterministic snapshot evidence.
    ///
    /// # Errors
    /// Returns `StoreError::Io` if creating the destination directory or copying segment files fails.
    pub fn snapshot_with_evidence(
        &self,
        dest: &std::path::Path,
    ) -> Result<SnapshotEvidenceReport, StoreError> {
        lifecycle::snapshot(self, dest)
    }

    /// Deprecated snapshot wrapper that drops [`SnapshotEvidenceReport`].
    ///
    /// # Errors
    /// Returns `StoreError::Io` if creating the destination directory or copying segment files fails.
    #[deprecated(note = "use snapshot_with_evidence; snapshot evidence is now first-class")]
    pub fn snapshot(&self, dest: &std::path::Path) -> Result<(), StoreError> {
        self.snapshot_with_evidence(dest).map(|_| ())
    }

    /// Fork the current store into a self-contained destination directory and
    /// return deterministic fork evidence.
    ///
    /// The destination is not opened by this method. Callers that want to use
    /// the fork should open it explicitly after this method returns, preserving
    /// the copied directory without appending lifecycle events during the copy.
    ///
    /// # Errors
    /// Returns `StoreError::Io` if creating the destination, clearing stale
    /// store artifacts, or copying/linking source files fails.
    pub fn fork_with_evidence(
        &self,
        dest: &std::path::Path,
        options: ForkOptions,
    ) -> Result<ForkReport, StoreError> {
        lifecycle::fork(self, dest, options)
    }

    /// Fork the current store with default [`ForkOptions`], dropping the
    /// deterministic evidence report.
    ///
    /// # Errors
    /// Returns any error surfaced by [`Store::fork_with_evidence`].
    pub fn fork(&self, dest: &std::path::Path) -> Result<(), StoreError> {
        self.fork_with_evidence(dest, ForkOptions::default())
            .map(|_| ())
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
    /// [`segment::CompactionResult`]. The accompanying
    /// [`CompactionReportBody`] is always returned as deterministic evidence
    /// for the compaction decision and observed outcome.
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
