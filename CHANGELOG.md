# Changelog

All notable changes to this project will be documented in this file.

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
- 7 traceability artifacts + 5 new invariants
- 473 tests, 0 clippy errors, full integrity checks
