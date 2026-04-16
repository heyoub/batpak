# Reference

This is the compact technical reference for `batpak`. Use it for architecture,
topology, replay lanes, tuning, invariants, and authoritative paths. Use
[`README.md`](README.md) for orientation and [`GUIDE.md`](GUIDE.md) for
workflow-driven usage.

## Truth Hierarchy

When repo surfaces disagree, trust them in this order:

1. live code in `src/`
2. root docs: `README.md`, `GUIDE.md`, `REFERENCE.md`
3. traceability registries in `traceability/`
4. anything historical that is being moved out to external archive storage

## The Five Layers

```text
coordinate  ->  event  ->  guard  ->  pipeline  ->  store
  WHO+WHERE     WHAT      MAY I?    COMMIT       PERSIST
```

### Coordinate

- `Coordinate` is `(entity, scope)`
- `entity` is the stream key
- `scope` is the isolation boundary
- `Region` is the shared query/subscription/cursor predicate type

### Event

An `Event<P>` is typed payload plus metadata:

- `event_id`
- `correlation_id`
- `causation_id`
- `timestamp_us`
- `position`
- `event_kind`
- `flags`
- `content_hash`

`EventKind` is a sealed packed `u16` with category/type semantics.

`position` is not fully caller-controlled. Public append surfaces may hint only
the DAG branch coordinates:

- `AppendPositionHint { lane, depth }`
- writer still owns `wall_ms`, `counter`, and `sequence`
- default append behavior remains root position: `lane=0`, `depth=0`

### Guard and Pipeline

- `Gate<Ctx>` is a pure policy check
- `GateSet::evaluate()` yields `Receipt<T>`
- `Pipeline` turns approved proposals into committed operations
- `Bypass` exists for explicit override paths

### Store

The store owns:

- append path
- query path
- projection path
- subscription/cursor path
- lifecycle operations like `sync`, `snapshot`, `compact`, `close`

The store is sync by design. Async integration belongs around it, not inside it.

## Runtime Map

- `src/coordinate/mod.rs`: `Coordinate`, `Region`, `KindFilter`
- `src/event/`: event model and replay-lane types
- `src/store/config.rs`: `StoreConfig`, `IndexTopology`
- `src/store/contracts.rs`: `AppendOptions`, `AppendPositionHint`, batch contracts
- `src/store/control_plane.rs`: tickets, outbox, visibility fence
- `src/store/fanout.rs`: notification fanout and internal committed-event envelopes
- `src/store/writer.rs`: writer thread and commit flow
- `src/store/staging.rs`: shared committed-event staging packets
- `src/store/index.rs`: in-memory index and visibility gate
- `src/store/columnar.rs`: base AoS plus optional overlays
- `src/store/projection_flow.rs`: replay, incremental apply, cache path
- `src/store/watch.rs`: projection watcher
- `tools/integrity/src/architecture_lints.rs`: parser-backed truth-surface checks
- `tools/xtask/src/main.rs`: CLI entrypoint and dispatch only
- `tools/xtask/src/bench.rs`: benchmark surface and compile orchestration
- `tools/xtask/src/coverage.rs`: coverage execution, retained artifacts, and reporting
- `tools/xtask/src/docs.rs`: root-doc site and rustdoc generation
- `tools/xtask/src/devcontainer.rs`: canonical container execution and image reuse
- `tools/xtask/src/preflight.rs`: single-session canonical proof chain
- `tools/xtask/src/commands.rs`: repo workflow commands, hooks, smoke checks, release plumbing

## Topology Model

`IndexTopology` is the live public model. Base AoS maps are always present.
The topology only controls optional overlays.

- `IndexTopology::aos()`: base AoS only
- `IndexTopology::scan()`: base AoS + SoA
- `IndexTopology::entity_local()`: base AoS + SoAoS entity-group overlay
- `IndexTopology::tiled()`: base AoS + AoSoA64 tiled overlay
- `IndexTopology::all()`: base AoS + all overlays

`IndexTopology::default()` delegates to `aos()`, so overlay cost stays opt-in.

Query routing is capability-driven:

- kind/category queries prefer `SoA -> AoSoA64 -> SoAoS -> base AoS`
- scope queries prefer `SoAoS -> SoA -> AoSoA64 -> base AoS`
- only `entity_local()` and `all()` provide projection-local generation/cache acceleration

## Replay Lanes

`ReplayLane` is the live replay naming.

- `JsonValueInput`
  ergonomic default, decodes to `serde_json::Value`
- `RawMsgpackInput`
  keeps raw MessagePack bytes for throughput-sensitive projections

Raw replay is a real lane, not decorative sophistication, but it should win by
measurement before becoming the default mental model.

Current measurement witness:

- `benches/replay_lanes.rs` is the current witness surface and currently shows
  `RawMsgpackInput` ahead of `JsonValueInput` on the 1k-event counter-shaped
  replay workload in this tree
- `examples/event_sourced_counter.rs` is the canonical ergonomic lane example
- `examples/raw_projection_counter.rs` is the canonical performance-lane example

## Writer Data Flow

1. caller builds append intent
2. writer thread reserves sequence space
3. segment append writes MessagePack frames and integrity metadata
4. index population updates base maps and active overlays
5. publish happens only after population is complete
6. broadcast/subscription notifications happen after publish

Important characterization surfaces:

- `tests/atomic_batch.rs`
- `tests/multi_view_parity.rs`
- `tests/raw_projection_mode.rs`
- `tests/store_projection_wiring.rs`
- `tests/writer_command_flow.rs`

## Storage And Cold Start

Events live in append-only segment files.

Cold-start priority:

1. `index.fbati` mmap artifact
2. `index.ckpt` checkpoint restore
3. SIDX footer scan for sealed segments plus active-segment scan
4. full frame-by-frame rebuild

Batch append uses BEGIN/COMMIT markers and atomic visibility publication.

Current artifact versions:

- SIDX footer magic: `SDX2`
- checkpoint format: v4
- mmap index snapshot: v3

Compatibility rules:

- old SIDX footers are ignored and reopen falls back to scan
- checkpoint v3 restores missing `dag_lane` / `dag_depth` as `0`
- mmap v1/v2 restores missing `dag_lane` / `dag_depth` as `0`
- full frame scan remains the source of truth when an optimization artifact is missing, stale, or structurally incompatible

Position hints are persistence-affecting, not just API sugar: non-root
`lane`/`depth` must survive live append, mmap reopen, checkpoint reopen, SIDX
header reconstruction, and full rebuild.

## Public Surface Witnesses

Advanced store surface names worth keeping visible in docs and audits:

- `SyncMode`
- `AppendReceipt`
- `AppendOptions`
- `RetentionPredicate`
- `CompactionStrategy`
- `CompactionConfig`
- `StoreStats`
- `StoreDiagnostics`

Low-level storage surface names that remain intentionally public:

- `SEGMENT_MAGIC`
- `SEGMENT_EXTENSION`
- `SegmentHeader`
- `FramePayload`
- `FrameDecodeError`
- `frame_encode`
- `frame_decode`
- `segment_filename`
- `CompactionResult`

## Tuning Highlights

Important knobs on `StoreConfig`:

- `segment_max_bytes`
- `sync.every_n_events`
- `sync.mode`
- `fd_budget`
- `writer.channel_capacity`
- `writer.pressure_retry_threshold_pct`
- `writer.shutdown_drain_limit`
- `writer.stack_size`
- `batch.group_commit_max_batch`
- `batch.max_size`
- `batch.max_bytes`
- `index.topology`
- `index.incremental_projection`
- `index.enable_checkpoint`
- `index.enable_mmap_index`

Key tradeoffs:

- lower `sync.every_n_events` = more durability, less throughput
- higher `fd_budget` = faster reads, more open descriptors
- larger writer mailbox = fewer producer stalls, more peak memory
- richer topology = more query acceleration, more insert/memory cost

## Benchmark Surfaces

- `benches/projection_latency.rs`
- `benches/unified_bench.rs`
- `benches/writer_staging.rs`
- `benches/writer_batch_staging.rs`
- `benches/replay_lanes.rs`
- `benches/topology_matrix.rs`
- `benches/topology_write_cost.rs`

Canonical commands:

```bash
cargo xtask bench --surface neutral
cargo xtask bench --surface native
cargo xtask bench --surface neutral --compile
cargo xtask bench --surface native --compile
cargo xtask cover
cargo xtask cover --json
```

## Invariants

Build-time/runtime policy highlights:

1. no tokio in production dependencies
2. no async store API
3. no product-specific concepts in library declarations
4. no unsafe serialization shortcuts
5. Blake3 only for hash-chain integrity
6. public surface honesty checks must stay encoded in tooling

`build.rs` and `tools/integrity/src/architecture_lints.rs` are both part of
the enforcement story.

## Authoritative Paths

- front door: `README.md`
- usage/workflows: `GUIDE.md`
- technical reference: `REFERENCE.md`
- traceability registry: `traceability/artifacts.yaml`
- integrity entrypoint: `tools/integrity/src/main.rs`
- xtask command surface: `tools/xtask/src/commands.rs`
- architecture lints: `tools/integrity/src/architecture_lints.rs`
