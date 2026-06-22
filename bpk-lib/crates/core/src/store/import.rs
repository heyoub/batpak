//! Store-to-store event import by re-application.

use crate::coordinate::Region;
use crate::id::{EntityIdType, IdempotencyKey};
use crate::store::index::IndexEntry;
use crate::store::{
    AppendOptions, AppendReceipt, BatchAppendItem, CausationRef, EncodedBytes, ExtensionKey, Open,
    Store, StoreError,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Predicate used to skip source events during import.
pub type ImportFilter = Box<dyn Fn(&IndexEntry) -> bool + Send + Sync>;

/// Caller-owned identity for an import source log.
///
/// A non-empty opaque label that, together with the source event id, forms the
/// deterministic import idempotency key. The serde form is transparent: the
/// wire representation is identical to the inner string.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceNamespace(String);

impl SourceNamespace {
    /// Construct a source namespace, validating that it is non-empty.
    ///
    /// # Errors
    /// Returns [`StoreError::Configuration`] if the namespace is empty.
    pub fn new(value: impl Into<String>) -> Result<Self, StoreError> {
        let value = value.into();
        if value.is_empty() {
            return Err(StoreError::Configuration(
                "import source_namespace must be non-empty".to_string(),
            ));
        }
        Ok(Self(value))
    }

    /// Borrow the namespace as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SourceNamespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Source event selector for [`Store::import_events`].
#[derive(Clone, Debug)]
#[must_use]
pub struct ImportSelector {
    region: Region,
    after_global_sequence: Option<u64>,
}

impl ImportSelector {
    /// Select every visible source event.
    pub fn all() -> Self {
        Self {
            region: Region::all(),
            after_global_sequence: None,
        }
    }

    /// Select visible source events matching `region`.
    pub fn region(region: Region) -> Self {
        Self {
            region,
            after_global_sequence: None,
        }
    }

    /// Select visible source events strictly after `after_global_sequence`.
    pub fn after(after_global_sequence: u64) -> Self {
        Self {
            region: Region::all(),
            after_global_sequence: Some(after_global_sequence),
        }
    }

    /// Add or replace the exclusive source global-sequence resume point.
    pub fn with_after_global_sequence(mut self, after_global_sequence: u64) -> Self {
        self.after_global_sequence = Some(after_global_sequence);
        self
    }

    /// Borrow the region predicate used by this selector.
    pub fn region_ref(&self) -> &Region {
        &self.region
    }

    /// Return the exclusive source global-sequence resume point, if configured.
    pub fn after_global_sequence(&self) -> Option<u64> {
        self.after_global_sequence
    }
}

impl Default for ImportSelector {
    fn default() -> Self {
        Self::all()
    }
}

/// Options controlling [`Store::import_events`].
#[must_use]
pub struct ImportOptions {
    source_namespace: SourceNamespace,
    chunk_size: usize,
    filter: Option<ImportFilter>,
}

impl ImportOptions {
    /// Construct import options with the required caller-owned source namespace.
    ///
    /// # Errors
    /// Returns [`StoreError::Configuration`] if the namespace is empty.
    pub fn new(source_namespace: impl Into<String>) -> Result<Self, StoreError> {
        Ok(Self {
            source_namespace: SourceNamespace::new(source_namespace)?,
            chunk_size: 256,
            filter: None,
        })
    }

    /// Derive a namespace from a source data-directory path.
    ///
    /// This is an explicit opt-in convenience for local tooling. Durable import
    /// identity is still caller policy: passing a stable opaque namespace is
    /// preferred when the same logical source can move paths.
    ///
    /// # Errors
    /// Returns [`StoreError::Configuration`] if the path cannot be
    /// canonicalized or encoded as a namespace.
    pub fn with_source_namespace_from_data_dir(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let canonical =
            crate::store::platform::fs::canonicalize(path.as_ref()).map_err(|error| {
                StoreError::Configuration(format!(
                    "source namespace path {} could not be canonicalized: {error}",
                    path.as_ref().display()
                ))
            })?;
        let digest = crate::evidence::content_hash(canonical.as_os_str().as_encoded_bytes());
        Self::new(format!("data-dir:{}", hex_lower(&digest)))
    }

    /// Set the preferred chunk size. The import path clamps it to the
    /// destination store's configured batch maximum at execution time.
    pub fn with_chunk_size(mut self, chunk_size: usize) -> Self {
        self.chunk_size = chunk_size.max(1);
        self
    }

    /// Attach a caller predicate. Returning `false` skips the source entry.
    pub fn with_filter(mut self, filter: ImportFilter) -> Self {
        self.filter = Some(filter);
        self
    }

    /// Borrow the source namespace.
    pub fn source_namespace(&self) -> &SourceNamespace {
        &self.source_namespace
    }

    /// Return the preferred chunk size before destination clamping.
    pub fn chunk_size(&self) -> usize {
        self.chunk_size
    }
}

/// Report returned by [`Store::import_events`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImportReport {
    /// Events newly appended to the destination store.
    pub imported: u64,
    /// Events already present by deterministic import idempotency key.
    pub deduplicated: u64,
    /// Source events skipped because their kind is substrate-reserved.
    pub skipped_reserved: u64,
    /// Source events skipped by the caller filter.
    pub skipped_filtered: u64,
    /// Highest source global sequence observed by the selector.
    pub source_high_watermark: Option<u64>,
}

/// Schema version for the import provenance receipt extension.
pub const IMPORT_PROVENANCE_SCHEMA_VERSION: u16 = 1;

/// Signed receipt-extension body recording the source lineage of an import.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ImportProvenance {
    /// Extension schema version.
    pub schema_version: u16,
    /// Caller-supplied namespace for the source log.
    pub source_namespace: SourceNamespace,
    /// Source event id, as a raw `u128`.
    pub source_event_id: u128,
    /// Source global sequence.
    pub source_global_sequence: u64,
    /// Source event kind raw encoding.
    pub source_kind: u16,
    /// Source event content hash, covering the raw payload bytes.
    pub source_content_hash: [u8; 32],
}

impl ImportProvenance {
    fn encode_extension(&self) -> Result<EncodedBytes, StoreError> {
        crate::encoding::to_bytes(self).map_err(|error| StoreError::Serialization(Box::new(error)))
    }
}

/// Decode import provenance from an append receipt when present.
#[must_use]
pub fn provenance(receipt: &AppendReceipt) -> Option<ImportProvenance> {
    provenance_from_extensions(&receipt.extensions)
}

/// Decode import provenance from a receipt-extension map when present.
#[must_use]
pub fn provenance_from_extensions(
    extensions: &BTreeMap<ExtensionKey, EncodedBytes>,
) -> Option<ImportProvenance> {
    extensions
        .get(&import_provenance_extension_key())
        .and_then(|bytes| crate::encoding::from_bytes(bytes).ok())
}

pub(crate) fn import_provenance_extension_key() -> ExtensionKey {
    ExtensionKey::reserved("batpak.import.provenance")
}

pub(crate) fn import_events<S: crate::store::StoreState>(
    destination: &Store<Open>,
    source: &Store<S>,
    selector: &ImportSelector,
    options: &ImportOptions,
) -> Result<ImportReport, StoreError> {
    let destination_batch_max = usize::try_from(destination.config.batch.max_size)
        .unwrap_or(usize::MAX)
        .max(1);
    let chunk_size = options.chunk_size.max(1).min(destination_batch_max).max(1);
    let mut after = selector.after_global_sequence;
    let pre_import_frontier = destination.frontier().visible_hlc.global_sequence;
    // Bound the import to the source frontier captured at call time. Without this,
    // a same-store import (source == destination) would keep paginating into the
    // events it just appended — they carry higher global sequences and fresh import
    // keys, so they would re-import endlessly until a disk/idempotency limit.
    let import_ceiling = source.frontier().visible_hlc.global_sequence;
    let mut report = ImportReport::default();

    loop {
        let page = source.query_entries_after(&selector.region, after, chunk_size);
        if page.is_empty() {
            break;
        }
        after = page.last().map(IndexEntry::global_sequence);

        let mut new_items = Vec::new();
        let mut reached_ceiling = false;
        for entry in page {
            if entry.global_sequence() > import_ceiling {
                // Past the call-time source frontier: stop before re-importing
                // events appended by this import itself (same-store guard).
                reached_ceiling = true;
                break;
            }
            report.source_high_watermark = Some(
                report
                    .source_high_watermark
                    .unwrap_or(0)
                    .max(entry.global_sequence()),
            );
            if entry.event_kind().is_reserved() {
                report.skipped_reserved = report.skipped_reserved.saturating_add(1);
                continue;
            }
            if let Some(filter) = options.filter.as_ref() {
                if !filter(&entry) {
                    report.skipped_filtered = report.skipped_filtered.saturating_add(1);
                    continue;
                }
            }

            let key = import_key(&options.source_namespace, entry.event_id());
            if import_key_already_present(destination, key) {
                report.deduplicated = report.deduplicated.saturating_add(1);
                continue;
            }

            let raw = source.read_raw(crate::id::EventId::from(entry.event_id()))?;
            let provenance = ImportProvenance {
                schema_version: IMPORT_PROVENANCE_SCHEMA_VERSION,
                source_namespace: options.source_namespace.clone(),
                source_event_id: entry.event_id(),
                source_global_sequence: entry.global_sequence(),
                source_kind: entry.event_kind().as_raw_u16(),
                source_content_hash: raw.event.header.content_hash,
            };
            let append_options = AppendOptions::new()
                .with_idempotency(key)
                .with_correlation(raw.event.header.correlation_id)
                .with_extension(
                    import_provenance_extension_key(),
                    provenance.encode_extension()?,
                );
            new_items.push(BatchAppendItem::from_msgpack_bytes(
                raw.coordinate,
                raw.event.header.event_kind,
                raw.event.payload,
                append_options,
                CausationRef::None,
            ));
        }

        if !new_items.is_empty() {
            let receipts = destination.append_batch(new_items)?;
            for receipt in receipts {
                if receipt.sequence < pre_import_frontier {
                    report.deduplicated = report.deduplicated.saturating_add(1);
                } else {
                    report.imported = report.imported.saturating_add(1);
                }
            }
        }

        if reached_ceiling {
            break;
        }
    }

    Ok(report)
}

fn import_key(source_namespace: &SourceNamespace, source_event_id: u128) -> IdempotencyKey {
    let source_event_id = format!("{source_event_id:032x}");
    IdempotencyKey::for_operation(
        "batpak.import",
        &[source_namespace.as_str(), &source_event_id],
    )
}

fn import_key_already_present(destination: &Store<Open>, key: IdempotencyKey) -> bool {
    destination.index.idemp.get(key.as_u128()).is_some()
        || destination.index.get_by_id(key.as_u128()).is_some()
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinate::Coordinate;
    use crate::event::EventKind;
    use crate::id::EventId;
    use crate::store::{AppendOptions, StoreConfig};

    #[test]
    fn hex_lower_is_exact_lowercase() {
        // Exact value pins both nibbles of every byte: high nibble (0xA, 0xC),
        // low nibble (0xB, 0xD), and the all-zero byte. A constant-return
        // mutant (`String::new()` or `"xyzzy".into()`) cannot reproduce this.
        assert_eq!(hex_lower(&[0xAB, 0xCD, 0x01, 0x00]), "abcd0100");
    }

    /// Covers the public `provenance(&AppendReceipt)` wrapper (the fn at the top
    /// of this file) by capturing a REAL `AppendReceipt` whose extension envelope
    /// carries genuine import provenance, then decoding it through the wrapper.
    /// The `provenance -> None` mutant cannot satisfy the `Some(p)` field
    /// assertions. `provenance(&receipt)` is called directly so the wrapper —
    /// not `provenance_from_extensions` — is the covered seam.
    #[test]
    fn provenance_wrapper_decodes_real_import_receipt() {
        let source_dir = tempfile::tempdir().expect("source tempdir");
        let source = Store::open(StoreConfig::new(source_dir.path())).expect("open source");
        let dest_dir = tempfile::tempdir().expect("dest tempdir");
        let dest = Store::open(StoreConfig::new(dest_dir.path())).expect("open dest");

        let coord = Coordinate::new("entity:prov:wrapper", "scope:import").expect("coord");
        let kind = EventKind::custom(0xF, 0x8A);
        source
            .append(&coord, kind, &serde_json::json!({"n": 1}))
            .expect("source append");

        // Drive a real import so the source event is genuinely re-applied.
        let options = ImportOptions::new("source-prov-wrapper").expect("options");
        let report =
            import_events(&dest, &source, &ImportSelector::all(), &options).expect("import");
        assert_eq!(report.imported, 1, "exactly one source event must import");

        // Rebuild the SAME provenance the import wrote, from the real source
        // entry + raw bytes, and persist it on a real receipt via append. This
        // yields a genuine `AppendReceipt` carrying the import provenance
        // extension — the exact envelope shape an imported event receipt holds.
        let source_entry = source.by_entity("entity:prov:wrapper")[0].clone();
        let raw = source
            .read_raw(EventId::from(source_entry.event_id()))
            .expect("read source raw");
        let key = import_key(options.source_namespace(), source_entry.event_id());
        let provenance_body = ImportProvenance {
            schema_version: IMPORT_PROVENANCE_SCHEMA_VERSION,
            source_namespace: options.source_namespace().clone(),
            source_event_id: source_entry.event_id(),
            source_global_sequence: source_entry.global_sequence(),
            source_kind: source_entry.event_kind().as_raw_u16(),
            source_content_hash: raw.event.header.content_hash,
        };
        let append_options = AppendOptions::new().with_idempotency(key).with_extension(
            import_provenance_extension_key(),
            provenance_body
                .encode_extension()
                .expect("encode provenance"),
        );
        let receipt = dest
            .append_with_options(
                &Coordinate::new("entity:prov:wrapper:receipt", "scope:import").expect("coord"),
                kind,
                &serde_json::json!({"n": 1}),
                append_options,
            )
            .expect("append with provenance extension");

        let decoded = provenance(&receipt).expect("wrapper must decode import provenance");
        assert_eq!(
            decoded.source_event_id,
            source_entry.event_id(),
            "wrapper-decoded source_event_id must match the source event"
        );
        assert_eq!(
            decoded.source_namespace.as_str(),
            "source-prov-wrapper",
            "wrapper-decoded source_namespace must match the configured source namespace"
        );

        source.close().expect("close source");
        dest.close().expect("close dest");
    }
}
