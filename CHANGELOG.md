# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added
- `SequenceGate`: in-memory visibility watermark separating sequence allocation
  from reader-visible publication. Batch entries are now invisible to
  `query`/`stream`/`get_by_id`/`get_latest` until the entire batch is published
  via a single Release-ordered store. Proven by a loom model and a
  fault-injection concurrent-read test.
- `NativeCache`: built-in file-backed projection cache (no feature flag, no
  external dependency). Atomic writes via tempfile + atomic rename. Replaces
  the removed `RedbCache` and `LmdbCache` backends.
- `cargo xtask doctor --strict` now runs an fsync probe and prints the local
  filesystem's median fsync latency and the implied per-event durable
  throughput, with a hint when the value indicates a virtualized or remote
  mount. This makes environment-dependent durable benchmark numbers
  self-explanatory: users see their disk's physical limit before they wonder
  why their `durable_write_throughput` numbers are lower on a devcontainer
  than on bare-metal NVMe.
- `Store::open_with_native_cache(config, cache_path)` convenience constructor.
- `MAX_FRAME_PAYLOAD = 256 MB` bound in the segment scanner. A corrupt or
  malicious frame header that claims a multi-gigabyte payload now causes the
  scan to stop gracefully instead of attempting an unbounded allocation.
- `BatchConfig`, `WriterConfig`, `SyncConfig`, `IndexConfig`: grouped
  sub-structs on `StoreConfig`. Field paths moved from flat
  (`config.sync_every_n_events`) to nested (`config.sync.every_n_events`).
  All `with_*` builder methods kept their existing names and signatures —
  callers using the fluent API are unaffected.

### Changed
- **`NativeCache::put` no longer fsyncs.** The projection cache is rebuildable
  from segments, so per-write durability is unnecessary. Atomicity (no torn
  reads) still comes from `std::fs::rename`. This drops cache-write latency
  from ~13 ms to ~140 µs on the `projection_cache_native/cache_miss`
  benchmark — a **94x speedup**. Users who explicitly want crash-resilient
  cache state can call `cache.sync()`.
- `StoreError::cache_error()` helper added; replaces 8 occurrences of the
  verbose `.map_err(|e| StoreError::CacheFailed(Box::new(e)))` pattern.
- Segment rotation logic in `writer.rs` extracted to a single
  `maybe_rotate_segment()` helper, eliminating four near-identical copies.
- `handle_append_batch` decomposed from a 601-line monolith into a ~140-line
  orchestrator plus 6 helper methods (`validate_batch`,
  `preflight_batch_idempotency`, `precompute_batch_items`,
  `write_batch_marker_frame`, `write_batch_event_frames`,
  `stage_batch_index_entries`, `broadcast_batch_notifications`).
- `WriterCommand::Append` now carries the full `Coordinate` instead of
  separate `entity` / `scope` `Arc<str>` fields. The writer no longer
  reconstructs a `Coordinate` (and re-runs validation) on the hot path —
  it uses the one the caller already built.
- Test helpers consolidated into `tests/common/mod.rs` (`small_segment_store`,
  `medium_segment_store`, `test_coord`, `test_kind`). Five duplicate
  `test_store()` definitions across test files removed.
- Checkpoint restore now correctly preserves the allocator position when the
  global sequence is sparse (e.g., after a burned batch slot). Previously
  `insert()` advanced the allocator by one per restored entry, which lost the
  original `global_sequence` watermark and could lead to sequence reuse on the
  next append.
- `NativeCache::get()` now propagates real IO errors as
  `StoreError::CacheFailed` instead of silently degrading to a cache miss.
  `NotFound` still returns `Ok(None)` (the only legitimate cache miss).

### Removed
- `StoreIndex::entity_locks` (the per-entity `DashMap<Arc<str>, Mutex<()>>`)
  and the corresponding `lock.lock()` acquisitions in `handle_append` and
  `handle_append_batch`. These were dead code in the single-writer
  architecture: only the writer thread ever acquired them, so they guarded
  against a race that could not happen. Removing them strips a
  `DashMap::entry` + `Arc::clone` + `parking_lot::Mutex::lock`/unlock from
  every single-event append, plus the equivalent N-entity work from every
  batch.
- The `fsync_dir` helper from `NativeCache` (no longer needed after the
  cache write path stopped fsyncing).

### Removed (continued — earlier in this Unreleased cycle)
- `RedbCache`, `LmdbCache`, the `redb` and `lmdb` Cargo features, and the
  `redb`/`heed` dependencies. Replaced by `NativeCache`.
- `cache_map_size_bytes` field on `StoreConfig` and the
  `with_cache_map_size_bytes()` builder (LMDB-specific, dead after removal).
- `Store::open_with_redb_cache` and `Store::open_with_lmdb_cache` (use
  `Store::open_with_native_cache` instead).
- LMDB linking logic in `build.rs` and `liblmdb-dev` from the devcontainer
  image.
- The `RUSTSEC-2025-0141` advisory exception in `deny.toml` (no longer
  needed without `heed` in the dependency tree).

### Migration
Callers using the fluent builder API (`StoreConfig::new(dir).with_*()`) need
no changes. Callers using struct-literal initialization need to group fields
under their new sub-structs:

```rust
// Old:
StoreConfig {
    data_dir: "/data".into(),
    sync_every_n_events: 1,
    sync_mode: SyncMode::SyncData,
    ..StoreConfig::new("")
}

// New:
StoreConfig {
    data_dir: "/data".into(),
    sync: SyncConfig {
        every_n_events: 1,
        mode: SyncMode::SyncData,
    },
    ..StoreConfig::new("")
}
```

For LMDB/redb users: replace `Store::open_with_lmdb_cache(config, path)` or
`Store::open_with_redb_cache(config, path)` with
`Store::open_with_native_cache(config, path)`. Old `.redb` files and LMDB
`data.mdb` files in your cache directory can be safely deleted.

## [0.3.0] - 2026-04-09

### Added
- Atomic batch append: `Store::append_batch()` and `append_reaction_batch()` APIs
- `SYSTEM_BATCH_BEGIN` envelope marker for durable batch commit semantics
- `BatchAppendItem` with explicit `CausationRef` for intra-batch causation linking
- `BatchStage` and `StoreError::BatchFailed` for detailed batch failure reporting
- Cold-start batch recovery: global streaming scan with committedness enforcement
- Batch size limits (`batch_max_size`) and byte limits (`batch_max_bytes`) in config
- Marker invisibility: batch envelope frames never appear in queries/cursors/subscriptions
- Fault injection framework (`test-support` feature): `InjectionPoint`, `FaultInjector` trait, `CountdownInjector`, `ProbabilisticInjector` for chaos testing write paths
- `batch_append.rs` example demonstrating atomic multi-event commit with intra-batch causation

### Changed
- Segment scan logic now stages batch frames until commit marker confirmed
- SIDX remains advisory; frame stream is source of truth for committedness

## [0.1.0] - 2026-04-04

### Added
- Initial implementation of batpak v0.1.0
- Coordinate-addressed append-only causal log
- Typestate-aware transitions with EventSourced projection replay
- Outcome<T> algebraic type with monad laws (verified by proptest)
- Gate/Pipeline system with sealed Receipt for TOCTOU prevention
- Store with background writer thread, flume-based bisync channels
- Blake3 hash chains (optional), CRC32 frame verification
- ProjectionCache trait with NoCache, RedbCache, LmdbCache backends
- Region-based query, subscription (push), and cursor (pull) APIs
- Group commit: batch N appends per fsync via `group_commit_max_batch`
- Index checkpoint v2: interner snapshot + InternId entries for fast cold start
- Memory-mapped sealed segment reads via memmap2 (zero-copy)
- SIDX segment footer: compact binary index per sealed segment for fast rebuild
- IndexLayout enum: AoS (default), SoA, AoSoA8, AoSoA16, AoSoA64, SoAoS
- Columnar ScanIndex: replaces by_fact + scope_entities DashMaps for SoA/AoSoA
- StringInterner: compact InternId(u32) for entity/scope keys
- Incremental projection: `supports_incremental_apply()` opt-in delta replay
- Schema versioning: `schema_version()` + TypeId-based cache key isolation
- `watch_projection`: reactive projection watcher (subscribe + auto-reproject)
- Config validation: rejects invalid field values at Store::open()
- Idempotency enforcement: `IdempotencyRequired` error when batch > 1 without key
- Arc<IndexEntry> shared across all index maps (single allocation per event)
- Release/Acquire ordering on global_sequence (was SeqCst)
- StoreDiagnostics with index_layout and tile_count fields
- 7 traceability artifacts + 5 new invariants (at time of release)
- 473 tests, 0 clippy errors, full integrity checks (at time of release)
