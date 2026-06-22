//! Store import proofs.
//!
//! PROVES: INV-IMPORT-CONTENT-ISOMORPHISM. Import re-applies selected source
//! events with destination-local identity while preserving raw payload bytes,
//! content hashes, correlation metadata, deterministic import keys, and import
//! provenance.
//! CATCHES: merge-shaped source identity copying, payload re-encoding, forged
//! causation, partial idempotency replay errors, compaction-lost dedup, chunk
//! instability, reserved-kind import, and oversized destination admission.
//! SEEDED: two tempfile-backed real stores, fixed source namespaces, fixed
//! EventKinds, explicit chunk sizes, stable coordinates.

use batpak::id::{EntityIdType, IdempotencyKey};
use batpak::store::{
    provenance, provenance_from_extensions, ImportFilter, ImportOptions, ImportProvenance,
    ImportReport, ImportSelector, ReadOnly, Store, StoreConfig, StoreError,
    IMPORT_PROVENANCE_SCHEMA_VERSION,
};
use batpak_testkit::prelude::*;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn test_failure(message: &str) -> std::io::Error {
    std::io::Error::other(message.to_owned())
}

fn test_store(dir: &TempDir) -> TestResult<Store> {
    Ok(Store::open(
        StoreConfig::new(dir.path())
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )?)
}

fn test_store_with_small_segments(dir: &TempDir) -> TestResult<Store> {
    Ok(Store::open(
        StoreConfig::new(dir.path())
            .with_segment_max_bytes(512)
            .with_sync_every_n_events(1)
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )?)
}

fn append_numbered_events(
    store: &Store,
    entity: &str,
    kind: EventKind,
    start: usize,
    count: usize,
) -> TestResult<Coordinate> {
    let coord = Coordinate::new(entity, "scope:import")?;
    for n in start..start + count {
        store.append(&coord, kind, &serde_json::json!({"n": n}))?;
    }
    Ok(coord)
}

fn deterministic_import_key(namespace: &str, source_event_id: u128) -> u128 {
    IdempotencyKey::for_operation(
        "batpak.import",
        &[namespace, &format!("{source_event_id:032x}")],
    )
    .as_u128()
}

#[test]
fn import_events_reimport_is_noop_and_preserves_raw_payload_bytes() -> TestResult {
    let source_dir = TempDir::new()?;
    let source = test_store(&source_dir)?;
    let dest_dir = TempDir::new()?;
    let dest = test_store(&dest_dir)?;
    let coord = Coordinate::new("entity:import:raw", "scope:import")?;
    let kind = EventKind::custom(0xF, 0x81);
    source.append(&coord, kind, &serde_json::json!({"n": 1, "label": "one"}))?;
    source.append(&coord, kind, &serde_json::json!({"n": 2, "label": "two"}))?;

    let filter: ImportFilter = Box::new(|entry| entry.event_kind().category() == 0xF);
    let options = ImportOptions::new("source-alpha")?
        .with_chunk_size(1)
        .with_filter(filter);
    assert_eq!(options.source_namespace().as_str(), "source-alpha");
    assert_eq!(options.chunk_size(), 1);
    let report: ImportReport = dest.import_events(&source, &ImportSelector::all(), &options)?;
    assert_eq!(report.imported, 2);
    assert_eq!(report.deduplicated, 0);
    assert_eq!(report.skipped_reserved, 1);

    let source_entries = source.by_entity("entity:import:raw");
    let dest_entries = dest.by_entity("entity:import:raw");
    assert_eq!(source_entries.len(), 2);
    assert_eq!(dest_entries.len(), 2);
    let source_raw = source.read_raw(source_entries[0].event_id())?;
    let dest_raw = dest.read_raw(dest_entries[0].event_id())?;
    assert_eq!(
        dest_raw.event.payload, source_raw.event.payload,
        "import must preserve raw MessagePack payload bytes"
    );
    assert_eq!(
        dest_raw.event.header.content_hash, source_raw.event.header.content_hash,
        "content hash must remain byte-isomorphic across import"
    );
    assert_eq!(
        dest_raw.event.header.payload_version, 0,
        "raw import must not upcast or stamp a typed payload version"
    );

    let replay = dest.import_events(&source, &ImportSelector::all(), &options)?;
    assert_eq!(replay.imported, 0);
    assert_eq!(replay.deduplicated, 2);
    assert_eq!(dest.by_entity("entity:import:raw").len(), 2);
    let provenance_body: ImportProvenance =
        provenance_from_extensions(dest.by_entity("entity:import:raw")[0].receipt_extensions())
            .ok_or_else(|| test_failure("deduplicated import retained provenance"))?;
    assert_eq!(provenance_body.source_namespace.as_str(), "source-alpha");

    source.close()?;
    dest.close()?;
    Ok(())
}

#[test]
fn import_events_preserves_correlation_clears_causation_and_records_provenance() -> TestResult {
    let source_dir = TempDir::new()?;
    let source = test_store(&source_dir)?;
    let dest_dir = TempDir::new()?;
    let dest = test_store(&dest_dir)?;
    let coord = Coordinate::new("entity:import:lineage", "scope:import")?;
    let kind = EventKind::custom(0xF, 0x82);
    let root = source.append(&coord, kind, &serde_json::json!({"step": "root"}))?;
    source.append_reaction(
        &coord,
        kind,
        &serde_json::json!({"step": "reaction"}),
        CorrelationId::from(u128::from(root.event_id)),
        CausationId::from(u128::from(root.event_id)),
    )?;

    let options = ImportOptions::new("source-lineage")?;
    let report = dest.import_events(
        &source,
        &ImportSelector::region(Region::entity("entity:import")),
        &options,
    )?;
    assert_eq!(report.imported, 2);
    assert_eq!(report.skipped_reserved, 0);

    let source_entries = source.by_entity("entity:import:lineage");
    let dest_entries = dest.by_entity("entity:import:lineage");
    assert_eq!(source_entries.len(), 2);
    assert_eq!(dest_entries.len(), 2);
    assert_eq!(
        dest_entries[1].correlation_id(),
        source_entries[1].correlation_id(),
        "import must preserve correlation as opaque metadata"
    );
    assert_eq!(
        dest_entries[1].causation_id(),
        None,
        "import must not forge a destination-local causation edge from a source event id"
    );
    assert_ne!(
        dest_entries[0].event_id(),
        source_entries[0].event_id(),
        "import is re-application with regenerated destination identity"
    );

    let provenance_body: ImportProvenance =
        provenance_from_extensions(dest_entries[0].receipt_extensions())
            .ok_or_else(|| test_failure("import provenance extension"))?;
    assert_eq!(
        provenance_body.schema_version,
        IMPORT_PROVENANCE_SCHEMA_VERSION
    );
    assert_eq!(provenance_body.source_namespace.as_str(), "source-lineage");
    assert_eq!(
        provenance_body.source_event_id,
        source_entries[0].event_id().as_u128()
    );
    assert_eq!(
        provenance_body.source_global_sequence,
        source_entries[0].global_sequence()
    );
    let source_raw = source.read_raw(source_entries[0].event_id())?;
    assert_eq!(
        provenance_body.source_content_hash, source_raw.event.header.content_hash,
        "provenance must record the source CONTENT hash, not the chain event hash"
    );

    let ordinary_receipt = dest.append(
        &Coordinate::new("entity:import:ordinary", "scope:import")?,
        kind,
        &serde_json::json!({"ordinary": true}),
    )?;
    assert!(
        provenance(&ordinary_receipt).is_none(),
        "non-import receipts must not decode import provenance"
    );

    source.close()?;
    dest.close()?;
    Ok(())
}

#[test]
fn import_options_reject_empty_namespace() -> TestResult {
    let err = ImportOptions::new("")
        .err()
        .ok_or_else(|| test_failure("PROPERTY: empty import source namespace must be rejected"))?;
    assert!(
        matches!(err, StoreError::Configuration(_)),
        "empty namespace must be a configuration error, got {err:?}"
    );

    let dir = TempDir::new()?;
    let derived = ImportOptions::with_source_namespace_from_data_dir(dir.path())?;
    assert!(
        derived.source_namespace().as_str().starts_with("data-dir:"),
        "path-derived source namespace must be explicit and prefixed"
    );
    Ok(())
}

#[test]
fn import_selector_after_resumes_exclusively() -> TestResult {
    let source_dir = TempDir::new()?;
    let source = test_store(&source_dir)?;
    let dest_dir = TempDir::new()?;
    let dest = test_store(&dest_dir)?;
    let coord = Coordinate::new("entity:import:after", "scope:import")?;
    let kind = EventKind::custom(0xF, 0x83);
    source.append(&coord, kind, &serde_json::json!({"n": 1}))?;
    let first_user_seq = source.by_entity("entity:import:after")[0].global_sequence();
    source.append(&coord, kind, &serde_json::json!({"n": 2}))?;
    source.close()?;
    let source = Store::<ReadOnly>::open_read_only(StoreConfig::new(source_dir.path()))?;

    let options = ImportOptions::new("source-after")?;
    let default_selector = ImportSelector::default();
    assert!(default_selector.after_global_sequence().is_none());
    assert!(default_selector.region_ref().fact().is_none());
    let after_selector = ImportSelector::after(first_user_seq);
    assert_eq!(after_selector.after_global_sequence(), Some(first_user_seq));
    let selector = ImportSelector::all().with_after_global_sequence(first_user_seq);
    assert_eq!(selector.after_global_sequence(), Some(first_user_seq));
    let report = dest.import_events(&source, &selector, &options)?;
    assert_eq!(report.imported, 1);
    assert_eq!(dest.by_entity("entity:import:after").len(), 1);

    dest.close()?;
    Ok(())
}

#[test]
fn import_events_partial_overlap_imports_only_missing_without_partial_batch_error() -> TestResult {
    let source_dir = TempDir::new()?;
    let source = test_store(&source_dir)?;
    let dest_dir = TempDir::new()?;
    let dest = test_store(&dest_dir)?;
    let kind = EventKind::custom(0xF, 0x84);
    append_numbered_events(&source, "entity:import:partial", kind, 0, 2)?;

    let options = ImportOptions::new("source-partial")?.with_chunk_size(16);
    let first = dest.import_events(&source, &ImportSelector::all(), &options)?;
    assert_eq!(first.imported, 2);
    assert_eq!(first.deduplicated, 0);

    append_numbered_events(&source, "entity:import:partial", kind, 2, 1)?;
    let replay = dest.import_events(&source, &ImportSelector::all(), &options)?;
    assert_eq!(replay.imported, 1);
    assert_eq!(replay.deduplicated, 2);
    assert_eq!(dest.by_entity("entity:import:partial").len(), 3);

    source.close()?;
    dest.close()?;
    Ok(())
}

#[test]
fn import_events_reimport_is_noop_after_destination_compaction() -> TestResult {
    let source_dir = TempDir::new()?;
    let source = test_store(&source_dir)?;
    let dest_dir = TempDir::new()?;
    let dest = test_store_with_small_segments(&dest_dir)?;
    let kind = EventKind::custom(0xF, 0x85);
    append_numbered_events(&source, "entity:import:compact", kind, 0, 8)?;

    let options = ImportOptions::new("source-compact")?.with_chunk_size(2);
    let first = dest.import_events(&source, &ImportSelector::all(), &options)?;
    assert_eq!(first.imported, 8);
    let _ = dest.compact(&CompactionConfig {
        min_segments: 1,
        strategy: CompactionStrategy::Merge,
    })?;

    let replay = dest.import_events(&source, &ImportSelector::all(), &options)?;
    assert_eq!(replay.imported, 0);
    assert_eq!(replay.deduplicated, 8);
    assert_eq!(dest.by_entity("entity:import:compact").len(), 8);

    source.close()?;
    dest.close()?;
    Ok(())
}

#[test]
fn import_events_uses_deterministic_key_and_regenerates_destination_identity() -> TestResult {
    let source_dir = TempDir::new()?;
    let source = test_store(&source_dir)?;
    let dest_dir = TempDir::new()?;
    let dest = test_store(&dest_dir)?;
    let kind = EventKind::custom(0xF, 0x86);
    append_numbered_events(&source, "entity:import:key", kind, 0, 1)?;

    let options = ImportOptions::new("source-key")?;
    let report = dest.import_events(&source, &ImportSelector::all(), &options)?;
    assert_eq!(report.imported, 1);

    let source_entry = source.by_entity("entity:import:key")[0].clone();
    let dest_entry = dest.by_entity("entity:import:key")[0].clone();
    assert_eq!(
        dest_entry.event_id().as_u128(),
        deterministic_import_key("source-key", source_entry.event_id().as_u128()),
        "destination event id must be the deterministic import idempotency key"
    );
    assert_ne!(
        dest_entry.event_id(),
        source_entry.event_id(),
        "import is re-application, not source identity copy"
    );

    source.close()?;
    dest.close()?;
    Ok(())
}

#[test]
fn import_events_chunk_size_does_not_change_imported_event_identity() -> TestResult {
    let source_dir = TempDir::new()?;
    let source = test_store(&source_dir)?;
    let kind = EventKind::custom(0xF, 0x87);
    append_numbered_events(&source, "entity:import:chunks", kind, 0, 5)?;

    let small_chunk_dir = TempDir::new()?;
    let small_chunk_dest = test_store(&small_chunk_dir)?;
    let large_chunk_dir = TempDir::new()?;
    let large_chunk_dest = test_store(&large_chunk_dir)?;

    let small_options = ImportOptions::new("source-chunks")?.with_chunk_size(1);
    let large_options = ImportOptions::new("source-chunks")?.with_chunk_size(99);
    let small_report =
        small_chunk_dest.import_events(&source, &ImportSelector::all(), &small_options)?;
    let large_report =
        large_chunk_dest.import_events(&source, &ImportSelector::all(), &large_options)?;
    assert_eq!(small_report.imported, 5);
    assert_eq!(large_report.imported, 5);

    let small_entries = small_chunk_dest.by_entity("entity:import:chunks");
    let large_entries = large_chunk_dest.by_entity("entity:import:chunks");
    assert_eq!(small_entries.len(), large_entries.len());
    for (small, large) in small_entries.iter().zip(large_entries.iter()) {
        assert_eq!(small.event_id(), large.event_id());
        assert_eq!(small.hash_chain().event_hash, large.hash_chain().event_hash);
        assert_eq!(small.global_sequence(), large.global_sequence());
    }

    source.close()?;
    small_chunk_dest.close()?;
    large_chunk_dest.close()?;
    Ok(())
}

#[test]
fn import_recomputes_valid_prev_hash_chain_in_target() -> TestResult {
    let source_dir = TempDir::new()?;
    let source = test_store(&source_dir)?;
    let dest_dir = TempDir::new()?;
    let dest = test_store(&dest_dir)?;
    let entity = "entity:import:chain";
    let kind = EventKind::custom(0xF, 0x89);

    // The destination ALREADY holds a local event for this entity, so the
    // destination chain has a non-genesis head BEFORE the import. The writer
    // must recompute the imported chain over THIS head — not copy the source
    // prev_hash (which links to the source's genesis).
    let local_coord = Coordinate::new(entity, "scope:import")?;
    dest.append(
        &local_coord,
        kind,
        &serde_json::json!({"local": "pre-existing"}),
    )?;
    let dest_local_head = dest.by_entity(entity)[0].hash_chain().event_hash;
    assert_ne!(
        dest_local_head, [0u8; 32],
        "pre-existing destination event must establish a non-genesis chain head"
    );

    // Source carries a real per-entity chain (two events) for the SAME entity.
    append_numbered_events(&source, entity, kind, 0, 2)?;
    let source_entries = source.by_entity(entity);
    assert_eq!(source_entries.len(), 2);
    assert_eq!(
        source_entries[0].hash_chain().prev_hash,
        [0u8; 32],
        "source's first event links to source genesis — this is what a copy bug would leak"
    );

    let options = ImportOptions::new("source-chain")?;
    let report = dest.import_events(&source, &ImportSelector::all(), &options)?;
    assert_eq!(report.imported, 2);

    // Destination now holds: [local, imported0, imported1] in chain order.
    let dest_entries = dest.by_entity(entity);
    assert_eq!(dest_entries.len(), 3);

    // The FIRST imported event must link to the destination's pre-existing head,
    // NOT to genesis. A "copied source prev_hash" bug would set this to all-zeros.
    assert_eq!(
        dest_entries[1].hash_chain().prev_hash,
        dest_local_head,
        "first imported event's prev_hash must be the destination head, not the copied source genesis prev_hash"
    );
    // The whole destination chain must be a valid linear chain recomputed locally.
    for window in dest_entries.windows(2) {
        assert_eq!(
            window[1].hash_chain().prev_hash,
            window[0].hash_chain().event_hash,
            "each event's prev_hash must equal the previous TARGET event_hash"
        );
    }

    source.close()?;
    dest.close()?;
    Ok(())
}

#[test]
fn import_events_oversized_destination_item_is_rejected_cleanly() -> TestResult {
    let source_dir = TempDir::new()?;
    let source = test_store(&source_dir)?;
    let dest_dir = TempDir::new()?;
    let dest = Store::open(
        StoreConfig::new(dest_dir.path())
            .with_single_append_max_bytes(96)
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )?;
    let coord = Coordinate::new("entity:import:oversized", "scope:import")?;
    let kind = EventKind::custom(0xF, 0x88);
    source.append(&coord, kind, &serde_json::json!({"blob": "x".repeat(80)}))?;

    let options = ImportOptions::new("source-oversized")?;
    let err = dest
        .import_events(&source, &ImportSelector::all(), &options)
        .err()
        .ok_or_else(|| test_failure("oversized import unexpectedly succeeded"))?;
    assert!(
        matches!(err, StoreError::BatchItemTooLarge { .. }),
        "oversized import must surface BatchItemTooLarge, got {err:?}"
    );

    source.close()?;
    dest.close()?;
    Ok(())
}

#[test]
fn import_events_from_same_store_is_bounded_not_self_amplifying() -> TestResult {
    // Same-store import must import exactly the events present at call time and
    // then terminate — it must never re-import its own freshly-appended output
    // (which carries higher sequences and fresh import keys).
    let dir = TempDir::new()?;
    let store = test_store(&dir)?;
    let coord = Coordinate::new("entity:import:self", "scope:import")?;
    let kind = EventKind::custom(0xF, 0x75);
    for i in 0..4 {
        store.append(&coord, kind, &serde_json::json!({ "i": i }))?;
    }

    let options = ImportOptions::new("self-source")?;
    let report = store.import_events(&store, &ImportSelector::all(), &options)?;
    assert_eq!(
        report.imported, 4,
        "same-store import must import exactly the 4 pre-call events, then stop"
    );
    store.close()?;
    Ok(())
}
