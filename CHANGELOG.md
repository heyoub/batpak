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
- **`cargo xtask preflight`**: runs the full CI pipeline (`xtask ci` + coverage
  + docs + rustdoc-bang-redirect strip + `.lock` cleanup + chmod) inside the
  canonical devcontainer. Bit-equivalent to the GH `Integrity
  (ubuntu-devcontainer)` job. If `preflight` passes locally, that GH job will
  pass — the single highest-leverage local gate for AI-agent-driven development.
- **`cargo xtask perf-gates`**: runs the 5 hardware-dependent perf gate tests
  on demand via `cargo nextest run --run-ignored only`. The tests are now
  `#[ignore]`'d in the normal suite so timing variance on shared CI runners
  cannot cause spurious failures; this command is the designated path to
  exercise them intentionally.
- **`check_ci_parity` structural check** (`tools/integrity/src/main.rs`): fails
  the build when `.github/workflows/ci.yml` drifts from
  `tools/xtask/src/main.rs` or `.devcontainer/Dockerfile`. Three invariants
  enforced: (1) every `cargo xtask <subcommand>` referenced in the workflow
  must exist in xtask; (2) every tool installed via `taiki-e/install-action`
  must also appear in `cargo xtask setup`; (3) version pins for
  `cargo-deny`, `cargo-llvm-cov`, `cargo-mutants`, `cargo-nextest`, and
  `mdbook` must agree between the Dockerfile and the xtask setup step.
- **Mutation testing on every PR** (`.github/workflows/ci.yml`): the `mutants`
  smoke job (1/12 shard, ~5 min) was previously `workflow_dispatch`-only and
  never ran in practice. It now runs on every `push` and `pull_request`.
  Results are report-only for this cycle; threshold gating is a follow-up.
- **`loom_model_bounded` helper** (`tests/deterministic_concurrency.rs`,
  `tests/group_commit_crash.rs`): wraps `loom::model(...)` in
  `loom::model::Builder` with `preemption_bound = Some(3)` so loom exploration
  is bounded and cannot OOM-kill a slow CI runner. Models that genuinely need
  a deeper bound will fail loud rather than spin.
- **`CHAOS_ITERATIONS=500`** in the `.github/workflows/ci.yml` env block. The
  previous in-source fallback was 10 — too few for meaningful concurrency-stress
  coverage. 500 is the CI compromise between wall-clock time and coverage depth.
- **Proptest seed persistence** (`tests/fuzz_targets.rs`, `tests/monad_laws.rs`,
  `tests/hash_chain.rs`, `tests/store_properties.rs`): all `ProptestConfig`
  instances now set
  `failure_persistence: Some(Box::new(FileFailurePersistence::SourceParallel("proptest-regressions")))`.
  Failing seeds are written next to the test source and re-exercised on every
  subsequent run. Without this, every proptest flake was effectively one-shot.
- **`round_trip_fidelity_property`** (`tests/store_properties.rs`): property
  test asserting that append + `get` returns the original payload for ANY
  shrinkable JSON value, not just a single fixed example. 64 cases per run.

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
- **Perf gates moved out of the unit suite.** The 5 timing-sensitive tests in
  `tests/perf_gates.rs` are now `#[ignore]`'d with comments pointing at
  `cargo xtask perf-gates`. Logic-only tests (synthetic contexts, no
  `Instant::now()`) continue to run on every CI cycle unaffected.
- **`subscription_ops.rs` sleeps removed.** All 8 `thread::sleep(20 ms)`
  "subscriber readiness" delays deleted. `store.subscribe()` registers
  synchronously and notifications buffer in the flume channel, so the sleeps
  were always dead weight. Net savings: ~160 ms per test run.
- **`atomic_batch.rs::batch_subscription_atomicity_no_partial_visibility`
  rewritten.** The old version spawned a thread that polled `try_recv` in a
  sleep loop bounded by `Instant::now()`. The new version drains the receiver
  immediately after each synchronous append. Runtime: ~700 ms → ~24 ms.
- **`store_restart_policy.rs` writer-death waits replaced with
  poll-with-deadline.** The two `thread::sleep(100 ms)` calls after
  `panic_writer_for_test` are replaced with retry-append loops bounded by a 5 s
  deadline. The post-exhaustion error is now asserted to be specifically
  `StoreError::WriterCrashed` rather than a generic failure.
- **`read_dir` results sorted before indexing** (`tests/store_edge_cases.rs`,
  `tests/store_advanced.rs`). POSIX `readdir` makes no ordering guarantee; the
  previous `remove(0)` / `[0]` indexing picked a non-deterministic file on
  exotic filesystems. Both call-sites now sort the `DirEntry` vec before use.
- **Bench fix — `bench_layout_by_fact`** (`benches/unified_bench.rs`): the
  `assert_eq!(results.len(), 1_000)` assertion was inside `b.iter`, which
  (a) crashes the bench on regression instead of failing a test cleanly and
  (b) adds measurement noise. Moved outside the loop; `criterion::black_box`
  added around the result inside.
- **Bench fix — `compaction.rs`**: deprecated `b.iter_with_setup` replaced with
  `b.iter_batched` + `BatchSize::SmallInput`.

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
- **`law_003_store_public_api_exercised`** (`tests/store_properties.rs`):
  pure tautology — asserted `!"name".is_empty()` over a hardcoded slice of
  method name strings. Would have passed with zero `Store` methods. The
  comment in the source even acknowledged it was "documentation, not a runtime
  test."
- **`entry_size_constant_matches_layout`** (`src/store/sidx.rs`): asserted
  that a `Vec<u8>` constructed with length N still has length N after
  `encode_into` writes in-place. The compile-time `_ASSERT_ENTRY_SIZE` already
  enforces this statically; the runtime test added nothing.
- **3 reader internal-state tests** (`src/store/reader.rs`): the old tests
  directly locked the private `buffer_pool` and asserted on the `Vec`'s
  internal length. Replaced with behavior-based versions that assert the
  observable contract — recycled buffers must be exactly the requested size
  and zero-filled on re-acquire — without coupling to private implementation
  details.

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

### Fixed
- **`ReplayCursor::commit` empty-replay off-by-one** (`src/store/index.rs`).
  Every fresh-store cold start goes through `rebuild_from_segments` →
  `cursor.commit(0)`. The old logic unconditionally set the allocator to
  `max_seen.saturating_add(1)`, which is `1` for an empty cursor — so the very
  first append on a brand-new store received sequence `1` instead of `0`. Fix:
  gate the `+1` on a new `inserted_any` flag; empty replays leave the allocator
  at the supplied `hint` unchanged.
- **`ReplayCursor::synthesize_next` first-call off-by-one** (`src/store/index.rs`).
  The slow-path rebuild (active segment with no SIDX footer) calls
  `synthesize_next()` for every entry. The old implementation returned
  `1, 2, 3, …` instead of `0, 1, 2, …` — same root cause as above, same
  `inserted_any` guard fix. Caught by
  `tests/replay_consistency.rs::snapshot_checkpoint_matches_source_projection`
  after the empty-replay fix landed and the test began diverging at
  `live=6, snap=7`.
- **Reader info-disclosure via recycled buffers** (`src/store/reader.rs`,
  `acquire_buffer`). `Vec::resize(min_size, 0)` only zeroes newly appended
  elements — existing bytes are untouched when the buffer is shrunk or
  returned at the same size. A recycled buffer therefore leaked the previous
  caller's bytes to the next acquirer. Fix: `buf.clear(); buf.resize(min_size, 0);`
  so the entire range is unconditionally zeroed. Caught by a new
  behavior-based test that fills a buffer with `0xAB`, releases it back to the
  pool, re-acquires it, and asserts every byte is `0x00`.

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
