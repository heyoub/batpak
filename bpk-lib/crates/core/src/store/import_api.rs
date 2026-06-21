use super::*;

impl Store<Open> {
    /// WRITE: import visible events from another store by re-applying their
    /// raw payload bytes into this destination store.
    ///
    /// Imported events receive new destination event ids, global sequences, and
    /// destination-local hash-chain predecessors. Payload bytes and content
    /// hashes are preserved. Causation is cleared so no source-store event id is
    /// forged as a local causation edge; correlation is preserved as opaque
    /// metadata. Each imported event carries signed import provenance in a
    /// substrate-owned receipt extension.
    ///
    /// # Errors
    /// Returns any source read, destination append, serialization, or
    /// configuration error surfaced while importing.
    pub fn import_events<S>(
        &self,
        source: &Store<S>,
        selector: &ImportSelector,
        options: &ImportOptions,
    ) -> Result<ImportReport, StoreError> {
        crate::store::import::import_events(self, source, selector, options)
    }
}
