use super::*;

impl<State> Store<State> {
    /// DIAGNOSTICS
    pub fn stats(&self) -> StoreStats {
        lifecycle::stats(self)
    }

    /// Return detailed diagnostic information about the store's internal state.
    pub fn diagnostics(&self) -> StoreDiagnostics {
        lifecycle::diagnostics(self)
    }

    /// Number of keys currently held in the durable idempotency store.
    ///
    /// This is the persistent dedup authority that survives retention
    /// compaction, cold-start, and snapshot independent of event eviction. It
    /// can temporarily exceed the configured soft cap under a within-window
    /// key-rate spike (the window always wins on correctness). Exposed for
    /// diagnostics and durability tests.
    pub fn durable_idempotency_key_count(&self) -> usize {
        self.index.idemp.len()
    }

    /// Deterministic store resource evidence over stable [`StoreDiagnostics`] facts.
    ///
    /// Canonical identity excludes raw paths (uses [`store_data_dir_identity_hash`]),
    /// free-form envelope diagnostics, and timestamps outside the structured cold-start
    /// report. Metadata fields on the returned envelope are unset by default.
    ///
    /// # Errors
    /// Canonical body encoding failure while computing `body_hash`.
    pub fn store_resource_evidence_report(
        &self,
    ) -> Result<StoreResourceEvidenceReport, StoreResourceReportError> {
        store_resource_evidence_report_from_diagnostics(&lifecycle::diagnostics(self))
    }
}
