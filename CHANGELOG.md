# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Changed
- Root docs now treat `README.md` as the primary entrypoint and keep historical material out of the tracked repo
- The public scan-configuration model now centers on `IndexTopology` instead of older layout/view compatibility naming
- Projection replay naming now centers on `ReplayLane`, `JsonValueInput`, and `RawMsgpackInput`
- Append options now carry an explicit `AppendPositionHint` for DAG `lane`/`depth` hints while writer-owned HLC wall/counter and sequence remain authoritative
- Cold-start persistence artifacts now advance together: SIDX uses `SDX2`, checkpoints are v4, and mmap index snapshots are v3 so non-root lane/depth survives reopen across every restore path

### Added
- Root-doc truth surface centered on `README.md`, `GUIDE.md`, and `REFERENCE.md`
- Parser-backed architecture linting in `tools/integrity/src/architecture_lints.rs`
- Topology/replay/writer measurement surfaces in `benches/topology_matrix.rs`, `benches/replay_lanes.rs`, and `benches/writer_staging.rs`
- End-to-end lane/depth position-hint coverage across live append, mmap reopen, checkpoint reopen, full rebuild, and SIDX header reconstruction

### Notes
- Released sections below preserve the public names that shipped in those releases, even when newer unreleased work has renamed the live surface since then.
- Upgrade note: pre-existing artifacts remain readable. Old SIDX footers are ignored and fall back to scan; checkpoint v3 and mmap v1/v2 load with `dag_lane=0` and `dag_depth=0`, which is the correct root default for pre-feature events.

## [0.5.0] - 2026-04-13

### Changed
- **`EventSourced` trait evolution**: replaced `EventSourced<P>` generic parameter with associated `Input` type. Existing impls add `type Input = ValueInput;` — same semantics, simpler surface
- **Restore planner subsystem**: all cold-start sources (mmap, checkpoint, rebuild) now flow through one internal `RestorePlanner` producing entity-partitioned runs instead of per-entry ordered-map insertion
- **Artifact format upgrades**: mmap index v2 and checkpoint v3 carry additive routing summaries (chunk directories, entity run tables). v1/v2 artifacts remain readable via fallback decoders

### Added
- `ProjectionInput` trait with `ValueInput` (serde_json::Value replay) and `RawMsgpackInput` (raw bytes replay) — projections choose their decode mode via one associated type
- `RoutingSummary` / `EntityRunTable` internal substrate consumed by restore, projection replay, and view materialization — one traversal, many products
- Entity-partitioned bulk restore: mixed-entity workloads build per-entity streams from pre-grouped runs; single-entity corpora hit a dedicated fast path
- Raw projection example (`examples/raw_projection_counter.rs`) and correctness tests (`tests/raw_projection_mode.rs`)
- Raw vs value bench lanes in `benches/projection_latency.rs`
- `close_only` cold-start bench lane in `benches/cold_start.rs`
- Single-entity and mixed-entity restore perf gates in `tests/perf_gates.rs`
- ADR-0008: Restore Planner and Projection Trait Evolution

### Fixed
- Restore scaling at 100k+ events: entity-partitioned runs replace repeated BTreeMap insertion, eliminating O(n log n) single-entity pathology

## [0.4.1] - 2026-04-13

### Fixed
- `close()` no longer writes both mmap and checkpoint artifacts — when mmap is enabled (default), checkpoint is skipped, cutting close-path cost roughly in half at high event counts
- Projection bench (`projection_first_pass`) no longer includes `close()` in measured routine, eliminating a benchmark-shape artifact that inflated the reported regression

### Added
- `ProjectionStrategy` enum (`pub(crate)`) with `compute_strategy()` pure function — replaces cascading if/else in `project()` with flat enum dispatch for code clarity and testability
- `read_events_batch()` / `read_event_only()` on `Reader` — projection-specific batch reader that skips `Coordinate::new()` and `StoredEvent` wrapping, returning `Vec<Event<Value>>` directly
- `ProjectionTimings` with per-phase timing breakdown (plan build, group-local lookup, cache key, prefetch, external cache probe, disk read, event extract, replay fold, cache store)
- `OpenIndexReport` diagnostics (path taken, restored/tail entry counts, elapsed time) exposed via `store.diagnostics().open_report`
- `CacheCapabilities.is_noop` field for honest `NoCache` detection in projection strategy
- Bench lane splits: `reopen_open_only` (open without close), `projection_first_pass_with_close` (lifecycle), `cold_nocache` / `cold_native_cache` (strategy-specific)
- `ProjectionColdPathGate` perf gate (50ms threshold for 1k events, observed 8.4ms)
- Prefetch moved to Phase 1c (before group-local check) for I/O overlap on cold path
- `GroupLocalIncremental` strategy — incremental apply from group-local cache baseline

### Changed
- Cancelled visibility fence ranges now use `Arc`-backed immutable snapshots (readers pay refcount bump, not vec copy)

## [0.4.0] - 2026-04-12

### Added
- Nonblocking submission: `submit()`, `submit_batch()`, `submit_reaction()`
  return `AppendTicket` / `BatchAppendTicket` with `wait()`, `try_check()`,
  and `receiver()` for async interop.
- Pressure-aware submission: `try_submit()`, `try_submit_batch()`,
  `try_submit_reaction()` return `Outcome<...Ticket>` with `Retry` when
  writer mailbox exceeds configurable threshold.
- `WriterPressure` struct and `Store::writer_pressure()` for mailbox queue
  depth inspection.
- `Outbox` staging buffer: `stage()` validates eagerly, `flush()` /
  `submit_flush()` commits as batch.
- `VisibilityFence`: writes are durable but invisible until `commit()`;
  auto-cancels on `Drop`.
- `Store<ReadOnly>` typestate: opens without writer thread, compile-time
  prevents writes, supports all read/query/project/cursor APIs.
- Multi-view index: base AoS + simultaneous SoA, SoAoS, and AoSoA64
  overlays; queries auto-route by access pattern.
- `ViewConfig` for per-overlay control; defaults to all enabled.
- Per-entity generation counters via `entity_generation()` and
  `project_if_changed()`.
- `scan()` combinator on `SubscriptionOps` for live lossy folds.
- `cursor_worker()` with supervised restart from last committed position.
- mmap-first cold-start via `index.fbati` artifact with parallel SIDX
  fallback.
- `cursor_guaranteed()` now available on both `Store<Open>` and
  `Store<ReadOnly>`.

### Changed
- Existing `append()` / `append_batch()` are now wrappers over
  `submit().wait()` (source-compatible).
- Cancelled visibility fence ranges use `Arc`-backed immutable snapshots
  (readers pay refcount bump, not vec copy).
- Writer shutdown drain now explicitly cancels any active visibility fence
  and unblocks pending tickets.
- Default `IndexConfig` enables all view overlays and mmap index.

### Fixed
- Writer shutdown with active visibility fence no longer leaks deferred
  ticket responses.

## [0.3.0] - 2026-04-12

### Changed (breaking — red team hardening pass)
- **Public delivery semantics are now explicit in the API surface:**
  `Store::subscribe_lossy()` replaces `subscribe()`,
  `Store::cursor_guaranteed()` replaces `cursor()`, and
  `Freshness::MaybeStale { max_stale_ms }` replaces `Freshness::BestEffort`.
- **Store lifecycle now carries an open-state typestate marker**
  (`Store<Open>`) and `close(self)` returns a terminal `Closed` token.
  `Drop` remains best-effort only and now logs loudly when callers skip
  explicit close.
- **`dangerous-test-hooks` replaces the old `test-support` feature name**
  across code, CI, docs, and traceability so downstream enablement risk is
  explicit instead of euphemistic.

### Security
- Checkpoint and native-cache persistence now use same-directory
  `tempfile`-backed atomic writes, reject symlink leaf targets, and keep the
  rename-after-fsync durability boundary intact.
- Single-event appends are now bounded by `StoreConfig::single_append_max_bytes`
  instead of being effectively limited only by `u32::MAX`.
- Coordinate components now have fixed length limits at construction time,
  bounding pathological entity/scope growth before it fans out into interner
  pressure.
- `react_loop` now consumes a private committed-event envelope from the writer
  instead of re-reading the just-committed event from disk.
- `watch_projection` now tails from an index watermark and catches up by
  delta replay after lossy notifications instead of re-projecting from
  genesis on every update.
- Cold-start rebuild now streams scanned entries directly into replay
  insertion instead of building a per-segment intermediate vector.
- `cargo xtask deny` is now a hard gate again: `cargo deny check` and
  `cargo audit --deny warnings` both have to pass with zero exceptions, and
  `cargo xtask release --dry-run` inherits that gate. No advisory ignore
  entries remain.

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
  `tools/xtask/src/main.rs` or `.devcontainer/Dockerfile`. Four invariants
  enforced: (1) every `cargo xtask <subcommand>` referenced in the workflow
  must exist in xtask; (2) every `taiki-e/install-action` tool entry in the
  workflow must use the canonical `name@version` form (a bare `tool: nextest`
  is rejected so Windows CI cannot silently drift from Linux pins); (3) every
  workflow tool pin must match the `cargo xtask setup` pin for the same
  binary; (4) version pins for `cargo-deny`, `cargo-llvm-cov`, `cargo-mutants`,
  `cargo-nextest`, and `mdbook` must agree between the Dockerfile and the
  xtask setup step. Together, these close the drift vector identified by the
  infrastructure-QA pass where unpinned Windows installs could resolve to a
  newer release than the canonical container.
- **`stale_terms` tripwire** (`tools/integrity/src/main.rs::check_for_stale_references`):
  seven removed-API identifiers (`RedbCache`, `LmdbCache`, `entity_locks`,
  `cache_map_size_bytes`, `with_cache_map_size_bytes`, `open_with_redb_cache`,
  `open_with_lmdb_cache`) are now structurally denied anywhere outside the
  allowlist (`CHANGELOG.md`, the historical spec snapshots,
  `docs/adr/ADR-0003-cache-safety-assumptions.md`, the historical audit
  snapshot, `AGENTS.md`, the tool source itself). Any reintroduction via lazy copy-paste
  from old docs fails `cargo xtask structural` (called by `cargo xtask ci`).
- **Test hardening — variant matching, seeded fuzz, `GOLDEN_UPDATE` sentinel**
  (`tests/chaos_testing.rs`, `tests/fuzz_chaos_feedback.rs`, `tests/wire_format.rs`).
  `chaos_corrupted_segment_bytes` replaced string-matching error checks
  (`msg.contains("CRC") || msg.contains("corrupt") || ...`) with a typed
  `matches!(e, StoreError::CrcMismatch{..} | StoreError::CorruptSegment{..} | ...)`
  guard so adding a new `StoreError` variant cannot silently widen the
  acceptable set. `run_extended_fuzz_chaos` replaced OS-seeded `rand::rng()`
  with `StdRng::seed_from_u64(seed)` where `seed` is read from the
  `FUZZ_CHAOS_SEED` env var and printed to stderr, making any failure in the
  50k-iteration fuzz loop exactly reproducible. Golden-file tests in
  `wire_format.rs` now refuse to rewrite fixtures unless the explicit
  `GOLDEN_UPDATE=I_KNOW_WHAT_IM_DOING` sentinel is set (a stray `=1` no
  longer trips the regenerator).
- **Mutation testing on every PR** (`.github/workflows/ci.yml`): the `mutants`
  smoke job (1/12 shard, ~5 min) was previously `workflow_dispatch`-only and
  never ran in practice. It now runs on every `push` and `pull_request`.
  Results gate the PR: `cargo-mutants 27.0` exits non-zero on any missed
  mutant by default, and a manual percentage-threshold backup in
  `tools/xtask/src/main.rs::assert_mutation_score` requires >= 20% catch
  rate. Removing tests will fail the PR.
- **`continue-on-error: true` on `actions/deploy-pages@v4` step**
  (`.github/workflows/ci.yml`): the deploy step requires GitHub Pages
  to be enabled in repository Settings → Pages with the "GitHub
  Actions" deployment source — a one-time admin task the codebase
  cannot perform. The build and upload steps above remain hard gates,
  so rustdoc/mdbook breakage still fails CI; only the final
  deployment is best-effort. Once Pages is enabled, the
  continue-on-error becomes a no-op.
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

#### Atomic batch append (originally in `[0.3.0] - 2026-04-09`, merged here)
- Atomic batch append: `Store::append_batch()` and `append_reaction_batch()` APIs
- `SYSTEM_BATCH_BEGIN` envelope marker for durable batch commit semantics
- `BatchAppendItem` with explicit `CausationRef` for intra-batch causation linking
- `BatchStage` and `StoreError::BatchFailed` for detailed batch failure reporting
- Cold-start batch recovery: global streaming scan with committedness enforcement
- Batch size limits (`batch_max_size`) and byte limits (`batch_max_bytes`) in config
- Marker invisibility: batch envelope frames never appear in queries/cursors/subscriptions
- Fault injection framework (`dangerous-test-hooks` feature): `InjectionPoint`, `FaultInjector` trait, `CountdownInjector`, `ProbabilisticInjector` for chaos testing write paths
- `batch_append.rs` example demonstrating atomic multi-event commit with intra-batch causation

### Changed
- **`NativeCache::put` no longer fsyncs.** The projection cache is rebuildable
  from segments, so per-write durability is unnecessary. Atomicity (no torn
  reads) still comes from `std::fs::rename`. This drops cache-write latency
  from ~13 ms to ~140 µs on the `projection_cache_native/cache_miss`
  benchmark — a **94x speedup**. `NativeCache::sync()` is a documented no-op
  on this backend and there is no public `Store` API path to invoke it; a
  power-loss-recoverable cache is an explicit non-goal because the segment
  log is the source of truth and a missing cache entry triggers replay on
  the next `project()` call. Custom `ProjectionCache` backends that buffer
  writes are still expected to implement `sync()` properly.
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

#### Atomic batch append (originally in `[0.3.0] - 2026-04-09`, merged here)
- Segment scan logic now stages batch frames until commit marker confirmed
- SIDX remains advisory; frame stream is source of truth for committedness

### Fixed
- **Atomic batch hash chain corrupted for multi-item same-entity batches**
  (`src/store/writer.rs::precompute_batch_items` /
  `write_batch_event_frames` / `stage_batch_index_entries`). Two
  independent bugs that produced the same symptom on disk and in memory:
  (a) `precompute_batch_items` populated `entity_prev_hashes.insert(entity, [0u8; 32])`
  *before* `event_hash` was known, so any second-or-later same-entity item
  in a batch wrote `prev_hash = [0; 32]` into its on-disk frame instead of
  the previous item's actual `event_hash`; (b) `stage_batch_index_entries`
  read `event_hash` for every staged `IndexEntry` and SIDX entry from the
  shared `entity_prev_hashes` map, which the write phase populated by
  `insert(entity, event_hash)` per item — so the map only ever held the
  entity's LAST item's hash, and every staged entry on that entity got the
  same wrong value. Net effect: `walk_ancestors` from any non-first
  same-entity batch item would terminate at `[0; 32]` instead of traversing
  predecessors; in-memory `IndexEntry`/SIDX hash chains diverged from the
  on-disk frame chain; cold-start rebuilds via the SIDX fast path and the
  slow-path frame scan reconstructed different state. Fix: compute blake3
  eagerly inside `precompute_batch_items`, store `event_hash` per item in
  `BatchItemComputed`, and have both the write and stage phases consume
  the per-item value verbatim. The `entity_prev_hashes` scratch map is
  gone. **No previously-shipped release contained this code path** — it
  was introduced in the v0.3.0 prep cycle and is fixed before publish.
- **Durably-committed batches discarded on cold start when the segment had
  no SIDX footer** (`src/store/reader.rs::scan_segment_index`). The slow
  path tracked `batch_committed_indices` and removed every batch entry
  from `entries` if `has_sidx_footer == false`, on the documented premise
  that "SIDX is written after sync, so its absence indicates sync didn't
  complete." That premise was wrong: `SidxEntryCollector::write_sidx_footer`
  is only ever invoked from `maybe_rotate_segment` and the writer-thread
  shutdown drain — *never* per batch. `handle_append_batch` issues its
  own `sync_with_mode` after writing the COMMIT marker, so a batch whose
  `append_batch` returned `Ok(receipts)` is durably on disk regardless of
  whether SIDX has been written yet. The discard logic was therefore
  silently dropping confirmed-committed batches whenever the process died
  between the batch sync and the next segment rotation / clean shutdown
  (OOM kill, kernel panic, host reboot, writer-thread panic with no clean
  drain) — an asymmetric "single events survive but batches vanish"
  failure mode that violated `[INV-BATCH-ATOMIC-VISIBILITY]`. Fix: delete
  the discard branch and the `batch_committed_indices` tracking. The
  COMMIT marker plus the existing CRC / decode-error mid-loop discards
  are the actual oracles for batch durability. **No previously-shipped
  release contained this code path** — same provenance as the hash-chain
  fix above.
- **Batch wall_ms could regress under a non-monotonic clock**
  (`src/store/writer.rs`). The single-append path applied
  `raw_ms.max(last_ms)` per entity to keep `ClockKey` ordering monotonic
  in `StoreIndex::streams`, but the batch path called `self.config.now_us()`
  twice per item (once for the frame header, once for the `IndexEntry`
  in the stage step) and never clamped against the entity's prior
  `wall_ms`. A regressing injected/test clock could (a) reorder
  `stream()` results within a batch and (b) write divergent header /
  IndexEntry / SIDX wall_ms values for the same item depending on which
  recovery path read it. Fix: capture a single `now_us` at the top of
  `precompute_batch_items`, clamp `wall_ms = now_ms.max(entity_last_ms)`
  per entity (consulting both the index and prior batch items), and
  store both `wall_us` and the clamped `wall_ms` in `BatchItemComputed`
  so the frame header, `IndexEntry`, and SIDX entry all consume the same
  per-item value. Same provenance — never shipped.

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
- **6 surviving mutants in `src/wire.rs` (caught and fixed)** — the first
  run of the mutation gate caught 6 surviving mutants in `u128_bytes`,
  `option_u128_bytes`, and `vec_u128_bytes` visitor methods (`expecting()`
  and `OptU128Visitor::visit_bytes()`). The visitors were local structs
  defined inside their `deserialize` functions, making them unreachable
  from any test and invisible to mutation testing. Fixed by extracting
  each visitor to a module-level `pub(super) struct` and adding 8 unit
  tests in `src/wire.rs::tests` that exercise every visitor method
  directly with known inputs. Every previously-missed mutation now has
  a corresponding assertion.
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

## [0.2.0] - 2026-04-05

### Added
- Group-commit writer with configurable `group_commit_max_batch`.
- Memory-mapped sealed-segment reader with LRU FD cache.
- Six `IndexLayout` variants (AoS, columnar tiles) with runtime selection.
- SIDX segment footer for fast cold-start rebuild.
- Checkpoint v2 with CRC32 integrity and sparse-sequence support.
- Incremental projection apply (skip already-applied events on cache hit).
- Schema versioning in segment headers.
- `watch_projection` reactive projection watcher.

### Changed
- Cold-start rebuild uses SIDX fast path when available, falls back to
  frame-by-frame scan only for the active (unsealed) segment.

### Fixed
- `Arc<str>` serialization through rmp-serde (required enabling the serde
  `rc` feature flag for `Coordinate` round-trip through MessagePack).

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
