# Reference

This is the compact technical reference for `batpak`. Use it for architecture,
topology, replay lanes, tuning, invariants, and authoritative paths. Use
[`README.md`](README.md) for orientation and [`GUIDE.md`](GUIDE.md) for
workflow-driven usage.

## Truth Hierarchy

When repo surfaces disagree, trust them in this order:

1. live code in `crates/core/src/`
2. root docs: `README.md`, `GUIDE.md`, `REFERENCE.md`
3. traceability registries in `traceability/`

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

`EventKind` is a sealed packed `u16` with category/type semantics. The
`#[derive(EventPayload)]` macro binds a Rust struct to its `EventKind` at compile
time via `#[batpak(category = N, type_id = N)]`. Category 0x0 and 0xD are
reserved; valid product categories are 0x1–0xC and 0xE–0xF. `type_id` is 12 bits
(0x000–0xFFF). The derive registers each payload in a binary-wide registry so
duplicate `(category, type_id)` pairs surface as generated test panics,
one-time `Store::open` warnings, explicit `validate_event_payload_registry()`
errors, or `EventPayloadValidation::FailFast` open errors instead of silent
shape drift on the wire.

For composed applications, treat kind allocation as a checked-in namespace:
reserve categories or `type_id` blocks per library boundary and keep the table
near the code that defines payloads. A minimal table is enough:
`category`, `type_id_start`, `type_id_end`, `owner`, `purpose`.

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

**Typed siblings.** For every raw surface that currently takes an `EventKind`
argument, the store exposes a typed sibling that infers the kind from
`T::KIND` where `T: EventPayload`:

- write: `append_typed`, `append_typed_with_options`, `submit_typed`,
  `try_submit_typed`, `append_reaction_typed`, `submit_reaction_typed`,
  `try_submit_reaction_typed`
- batch construction: `BatchAppendItem::typed`
- read: `by_fact_typed::<T>()`
- typestate: `Transition::from_payload::<P: EventPayload>`

The raw `EventKind` surfaces stay available for callers that compute
`EventKind` at runtime; the typed siblings are additive. See
`docs/ADR-0010-eventpayload-macro-surface.md` for scope and
schema-evolution rules. Typed reactor ergonomics
(`#[derive(EventSourced)]`, `#[derive(MultiEventReactor)]`,
`react_loop_typed`, `react_loop_multi`, `react_loop_multi_raw`) are
covered by ADR-0011.

## Runtime Map

- `crates/core/src/coordinate/mod.rs`: `Coordinate`, `Region`, `KindFilter`
- `crates/core/src/event/`: event model and replay-lane types
- `crates/core/src/store/config.rs`: `StoreConfig`, `IndexTopology`
- `crates/core/src/store/append.rs`: `AppendOptions`, `AppendPositionHint`, batch contracts
- `crates/core/src/store/write/control/`: tickets, outbox, visibility fence, submission bridge
- `crates/core/src/store/write/fanout.rs`: notification fanout and internal committed-event envelopes
- `crates/core/src/store/write/writer.rs`: writer orchestration spine, command router, segment rotation
- `crates/core/src/store/write/writer/append.rs`: single-append commit canal
- `crates/core/src/store/write/writer/batch.rs`: batch commit canal
- `crates/core/src/store/write/writer/fence_runtime.rs`: deferred replies and hidden-write ledger runtime
- `crates/core/src/store/write/writer/publish.rs`: committed-event materialization and fanout publish
- `crates/core/src/store/write/writer/runtime.rs`: restart loop, shutdown drain, segment bootstrap probe
- `crates/core/src/store/write/staging.rs`: shared committed-event staging packets
- `crates/core/src/store/platform/`: private target-sensitive machine-contact helpers for
  fs/sync/lock/clock/mmap operations
- `crates/core/src/store/index/mod.rs`: in-memory index and visibility gate
- `crates/core/src/store/index/columnar.rs`: base AoS plus optional overlays
- `crates/core/src/store/projection/flow.rs`: replay, incremental apply, cache path
- `crates/core/src/store/projection/watch.rs`: projection watcher
- `tools/integrity/src/architecture_lints.rs`: parser-backed truth-surface checks
- `tools/xtask/src/main.rs`: CLI entrypoint and dispatch only
- `tools/xtask/src/bench.rs`: benchmark surface and compile orchestration
- `tools/xtask/src/coverage.rs`: coverage execution, retained artifacts, and reporting
- `tools/xtask/src/docs.rs`: root-doc site and rustdoc generation
- `tools/xtask/src/devcontainer.rs`: canonical container execution and image reuse
- `tools/xtask/src/preflight.rs`: single-session canonical verification bundle
- `tools/xtask/src/commands.rs`: repo workflow commands, hooks, smoke checks, release plumbing

## Topology Model

`IndexTopology` is the live public model. Base AoS maps are always present.
The topology only controls optional overlays.

- `IndexTopology::aos()`: base AoS only
- `IndexTopology::scan()`: base AoS + SoA
- `IndexTopology::entity_local()`: base AoS + SoAoS entity-group overlay
- `IndexTopology::tiled()`: base AoS + AoSoA64 tiled overlay
- `IndexTopology::tiled_simd()`: base AoS + mixed-kind tiled overlay shaped
  for auto-vectorizable scans
- `IndexTopology::all()`: base AoS + all stable overlays

Diagnostics expose these presets as `aos`, `scan`, `entity-local`, `tiled`,
`tiled-simd`, and `all`. Non-preset overlay combinations are reported as
`hybrid`.

`IndexTopology::default()` delegates to `aos()`, so overlay cost stays opt-in.

Query routing is capability-driven:

- kind/category queries prefer `SoA -> AoSoA64 -> AoSoA64Simd -> SoAoS -> base AoS`
- scope queries prefer `SoAoS -> SoA -> AoSoA64 -> AoSoA64Simd -> base AoS`
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

- `crates/core/benches/replay_lanes.rs` is the current witness surface and currently shows
  `RawMsgpackInput` ahead of `JsonValueInput` on the 1k-event counter-shaped
  replay workload in this tree
- `crates/core/examples/event_sourced_counter.rs` is the canonical ergonomic lane example
- `crates/core/examples/raw_projection_counter.rs` is the canonical performance-lane example

## Diagnostics And Evidence Boundary

`StoreDiagnostics` is a live operational snapshot, not a deterministic evidence
report body. It intentionally exposes the current `StoreDiagnostics::frontier`,
writer pressure, topology label, `tile_count`, latest `open_report`, and
`platform_evidence` beside selected configuration values.

`StoreDiagnostics::platform_evidence` is recomputed when diagnostics are
collected. Platform evidence can therefore differ from the platform profile that
was verified at open if a later probe observes a different target posture or a
transient probe failure. Use platform profile verification for fail-closed
admission; use diagnostics for current operator visibility.

Not every `StoreConfig` knob appears in diagnostics. Sync cadence, batch limits,
single-append byte limits, signing-key registry contents, registry validation
mode, injected test clocks, open-report observers, and feature-gated test hooks
are configuration facts rather than deterministic evidence-report body fields.
Read-only stores also have no live writer thread, so `writer_pressure` reports
the absence of writer pressure rather than the mutable writer mailbox capacity.

The deterministic evidence-report family keeps this boundary explicit: report
bodies prove structural facts over visible store state, chain reads, projection
runs, read walks, schema snapshots, and subscriber observations. They do not
attest to fsync cadence, platform profile paths, signing-key configuration, or
which optional index layout produced equivalent answers.

## Writer Data Flow

1. caller builds append intent
2. writer thread reserves sequence space
3. segment append writes MessagePack frames and integrity metadata
4. index population updates base maps and active overlays
5. publish happens only after population is complete
6. broadcast/subscription notifications happen after publish

Important characterization surfaces:

- `crates/core/tests/atomic_batch.rs`
- `crates/core/tests/multi_view_parity.rs`
- `crates/core/tests/raw_projection_mode.rs`
- `crates/core/tests/store_projection_wiring.rs`
- `crates/core/tests/writer_command_flow.rs`

## Storage And Cold Start

Events live in append-only segment files.

Cold-start priority:

1. `index.fbati` mmap artifact
2. `index.ckpt` checkpoint restore
3. SIDX footer scan for sealed segments plus active-segment scan
4. full frame-by-frame rebuild

Batch append uses BEGIN/COMMIT markers and atomic visibility publication.

## Store Platform Backend

`crates/core/src/store/platform/` is a private store-internal room for target-sensitive
machine contact. It owns narrow mechanics such as symlink leaf checks,
same-directory tempfile persistence, parent-directory sync, store-lock open
policy, segment file creation, active-segment positional reads, segment sync,
canonical clocks, direct mmap calls, descriptive platform evidence, admission
tokens, profile records, and opt-in reverify.

The room stays private to `crates/core/src/store/` and reports target mechanics without
deciding store semantics. The rule is: platform observes; store admits; batpak
guarantees.
Durability, replay, visibility, and admission meaning stay with store,
cold-start, segment, and frontier code. `StoreDiagnostics::platform_evidence`
exposes the reported mechanics/posture, while internal admission tokens
(store lock, parent-dir sync, mmap index, and sealed-segment mmap) keep raw
evidence from becoming meaning. `StoreConfig::with_platform_profile_path`
enables profile-verified open; a mismatch returns a platform profile error
before mutable writer spawn or successful-open observability. Profile reverify
may still create the data directory and lock file before failing.

Operator profile workflows live under `cargo xtask platform ...`:

- `doctor` reports whether the current store path can produce a profile.
- `probe` writes a versioned JSON profile with a non-cryptographic CRC32
  fingerprint for accidental drift detection. Profile signing is not
  implemented.
- `verify` compares a profile with current evidence.
- `bless` intentionally refreshes a profile fixture.
- `audit` runs the platform boundary structural check.

Current artifact versions:

- SIDX footer magic: `SDX2`
- checkpoint format: v5
- mmap index snapshot: v4

Compatibility rules:

- old SIDX footers are ignored and reopen falls back to scan
- checkpoint v4 restores missing cumulative reserved-kind fallback stats as empty
- checkpoint v3 restores missing `dag_lane` / `dag_depth` as `0`
- mmap v3 restores missing cumulative reserved-kind fallback stats as empty
- mmap v1/v2 restores missing `dag_lane` / `dag_depth` as `0`
- full frame scan remains the source of truth when an optimization artifact is missing, stale, or structurally incompatible
- SIDX-accelerated cold start reconstructs `timestamp_us` as `wall_ms * 1000`, so it is best-effort to the nearest millisecond (±999 µs), not a sub-millisecond replay guarantee

Reopen observability contract:

- `diagnostics().open_report` carries per-reopen reserved-kind fallback totals and histograms plus cumulative totals and histograms persisted through the current store's cold-start artifacts
- `StoreConfig::with_open_report_observer(...)` fires once after each successful open with that same structured receipt; observer panics are warned and ignored
- mutable opens append one durable `SYSTEM_OPEN_COMPLETED` event at `batpak:store` / `batpak:lifecycle`; read-only opens stay side-effect free
- `SYSTEM_OPEN_COMPLETED` is an ordinary persisted event after append: it participates in the same query, snapshot, compaction, retention, and tombstone rules as any other stored event, so it does not have a special auto-prune path
- the `batpak:` coordinate prefix is reserved for library-owned lifecycle streams; application code should avoid emitting events at that prefix

### Delivery Witnesses

Checkpoint-backed cursor workers and typed reactors surface an at-least-once
witness to their handlers:

```rust
move |batch, store, witness: Option<&AtLeastOnce>| {
    if let Some(witness) = witness {
        let key = IdempotencyKey::from_bytes([0; 32]);
        let observed = ObservedOnce::new(witness.clone(), key);
        let (_at_least_once, _idempotency_key) = observed.into_parts();
    }
    CursorWorkerAction::Continue
}
```

`Some(&AtLeastOnce)` is emitted only when the worker configuration declares a
durable `checkpoint_id`. Ephemeral workers receive `None`, which means the
handler has process-local at-least-once delivery but no durable checkpoint
witness. The substrate is the only constructor for `AtLeastOnce`; handlers may
inspect its identity with `checkpoint_id()` and clone the witness when they
need to compose an `ObservedOnce`.

Typed reactor surfaces follow the same rule. `TypedReactive::react`,
`MultiReactive::dispatch`, and derive-generated multi-reactor handlers receive
the witness after their `ReactionBatch` parameter.

### Durable Frontier

The store exposes a six-watermark frontier that tracks how far events have
advanced through the commit, visibility, fanout, and projection pipeline:

- `accepted`: highest HLC the writer has accepted into the commit pipeline
- `written`: highest HLC whose frame has been written to the active segment
- `durable`: highest HLC whose frame has been fsynced
- `visible`: highest HLC visible to query readers
- `emitted`: highest HLC for which broadcast artifacts were attempted
- `applied`: highest HLC consumed by all registered projections

At every torn-free observation, the frontier preserves these ordering
relations:

```text
accepted >= written >= durable
accepted >= visible >= applied
emitted  >= visible
```

Use `Store::frontier()` to get a coherent `FrontierView` snapshot. The same
composition path feeds `Store::diagnostics().frontier`, so
`store.diagnostics().frontier == store.frontier()` for a single observation.
The view is composed while holding the watermark mutex, and visible/emitted
advance through a composite helper so external observers cannot see
commit-time torn states such as `emitted < visible`.

`FrontierView` exposes the current six-watermark observation surface plus two
derived lag fields:

- `accepted_hlc`
- `written_hlc`
- `durable_hlc`
- `current_visible_hlc`
- `applied_hlc`
- `emitted_hlc`
- `visible_minus_durable_seq`
- `oldest_pending_write_age_ms`

`visible_minus_durable_seq` is signed. A positive value means visible events
are ahead of durable sync, while a negative value can appear in internal
commit windows where durability advances before visibility. Pending-write age
is measured with `Instant`; it is not derived from HLC wall time.

On open, the store computes:

```text
open_hlc = max(max_recovered_hlc, last_close_hlc, wall_time_floor)
```

Mutable opens emit a durable `SYSTEM_OPEN_COMPLETED` lifecycle event and then
bootstrap the frontier to the emitted open HLC. Read-only opens do not emit a
lifecycle event, but still bootstrap from the recovered index high-water mark
and wall-time floor. The frontier starts with all six watermarks at `open_hlc`,
including `applied`, so a store with zero registered projections does not
report projection lag below the lifecycle open frontier.

Projection progress is tracked through the projection registry. `applied_hlc`
is the minimum HLC across registered projections. Registering a projection
seeds it at the current applied frontier so late registration cannot move the
global frontier backward. Unregistering a projection recomputes the minimum
from the remaining projections: removing the fastest projection can freeze
`applied_hlc`, while removing the slowest can allow it to advance.

#### Waiting for Durability

`Store::wait_for_durable(point, timeout)` blocks the calling thread until the
durable frontier crosses `point`. `Store::wait_for_applied` and
`Store::wait_for_visible` use the same synchronous wait contract for the
applied and visible watermarks. The timeout is mandatory:

```rust
store.wait_for_durable(target_hlc, std::time::Duration::from_secs(1))?;
```

The wait returns `Ok(())` only after observing `durable_hlc >= point` under the
watermark mutex. If the timeout expires first it returns
`StoreError::WaitTimeout { watermark: WatermarkKind::Durable, target, waited_ms
}`. If the writer thread panics while callers are waiting, waiters wake and
return `StoreError::WriterCrashed`.

The implementation is sync-only and uses a `parking_lot::Condvar` with wake-all
notification. Spurious wakeups are expected and harmless: every wake rechecks
the writer-crash poison flag and the target watermark before returning.
The release benchmark surface includes `frontier_waiters`, which measures both
waiter wake completion and writer-side wake cost at 1, 8, 32, 128, and 512
concurrent waiters. Each count runs same-target waits, where every waiter waits
for one HLC, and spread-target waits, where waiters cover distinct future HLCs.
Precise waiter lists stay deferred unless that benchmark shows writer-side wake
cost dominating append/sync latency or an order-of-magnitude wake-completion
jump between adjacent waiter-count tiers on stable hardware.

Append-time gating is opt-in through `AppendOptions::gate`:

```rust
pub struct DurabilityGate {
    pub kind: WatermarkKind,
    pub timeout: std::time::Duration,
}

pub struct AppendOptions {
    pub gate: Option<DurabilityGate>,
    // other append controls omitted
}
```

When set, `Store::append_with_options` and `Store::append_batch_with_options`
commit first, then wait for the chosen watermark to cross the committed event's
HLC. Batch gates apply to the last event in the batch; per-item gates embedded
in `BatchAppendItem` are ignored. `StoreError::WaitTimeout` means the commit
succeeded but the requested gate was not observed before the timeout.

Single-append fault injection defines three ordinals for frontier tests:

- `SingleAppendStart`: before any watermark advance
- `SingleAppendWritten`: after `written` advances and before durable sync
- `SingleAppendPublished`: after `visible` and `emitted` advance and before
  the receipt is returned

See `docs/ADR-0014-durable-frontier.md` for design rationale, and
`INV-FRONTIER-*` in `traceability/invariants.yaml` for the formal invariant
records. The current visible-before-durable cadence gap is intentionally
registered as `OBS-CADENCE-GT-1-VISIBLE-EXCEEDS-DURABLE` in
`traceability/observations.yaml` rather than as an invariant.

Position hints are persistence-affecting, not just API sugar: non-root
`lane`/`depth` must survive live append, mmap reopen, checkpoint reopen, SIDX
header reconstruction, and full rebuild.

Cold-start timing contract:

- `wall_ms` is the persisted ordering field used by reopen and HLC reconstruction
- exact `timestamp_us` still lives in the event frame and is available on full frame reads
- `EventHeader::age_us()` remains safe on SIDX-reconstructed headers, but callers must not compare live-path and cold-start `timestamp_us` values for sub-millisecond precision

Store ownership contract:

- opens acquire a lifetime-held lock file rooted at `{data_dir}/.batpak.lock`
- the directory lock is exclusive-only, so mutable and read-only opens both fail
  with `StoreLocked` while another live owner exists
- Unix lock-file opens use `O_NOFOLLOW`; non-Unix targets currently do a
  best-effort symlink-leaf rejection before opening because `std` exposes no
  equivalent atomic no-follow flag there

### Upgrade and Rollback Procedure

Operator posture for artifact-format changes is explicit:

- forward upgrade support is part of the contract for current readers
- old optimization artifacts may be ignored and rebuilt or fallback-scanned
- mixed-version operation does **not** mean two binaries may write the same mutable store directory at once
- downgrade is **not** assumed safe just because forward-read compatibility exists

Practical procedure:

1. stop all writers for the target store directory
2. deploy the newer binary and allow it to reopen normally
3. if reopen ignores an old artifact, let the fallback scan or rebuild complete instead of trying to preserve the stale optimization file
4. if you must roll back binaries, purge cold-start optimization artifacts (`index.fbati`, `index.ckpt`, old SIDX expectations) before reopening with the older binary unless that downgrade path is explicitly proven
5. never run two binary versions against the same mutable store directory concurrently

## Public Surface Witnesses

Advanced public surface names worth keeping visible in docs and audits. This
section is an audit witness list for public API shape; delivery-specific
witness types are called out separately below.

- `SyncMode`
- `AppendReceipt`
- `DenialReceipt`
- `AppendOptions`
- `CursorGapConfig`
- `GapObservation`
- `SigningKey`
- `RetentionPredicate`
- `CompactionStrategy`
- `CompactionConfig`
- `StoreStats`
- `StoreDiagnostics`
- `EventPayloadValidation`
- `EventPayloadRegistryError`

Delivery witness types:

- `CheckpointId`
- `AtLeastOnce`
- `IdempotencyKey`
- `ObservedOnce`

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

| Knob | Default | When to change |
| --- | --- | --- |
| `segment_max_bytes` | 256 MiB | Lower for faster rotation and smaller repair units; raise for fewer segment files. |
| `sync.every_n_events` | `1000` | Lower for tighter durability; raise for throughput when callers use explicit gates where needed, after measuring on deployment hardware. |
| `sync.mode` | `SyncAll` | Use `SyncData` only after deciding metadata sync cost is not needed for the deployment. |
| `fd_budget` | `64` | Raise when many segments stay hot and read latency matters; lower for constrained processes. |
| `writer.channel_capacity` | `4096` | Raise for bursty producers; lower to cap peak queued memory. |
| `writer.pressure_retry_threshold_pct` | `75` | Lower when `try_submit*` callers should back off earlier under load. |
| `writer.shutdown_drain_limit` | `1024` | Raise when graceful shutdown should drain larger queued append bursts. |
| `writer.stack_size` | OS default | Set only when platform thread defaults are too small for a measured workload. |
| `batch.max_size` | `256` items | Lower to bound latency; raise for larger atomic import batches. |
| `batch.max_bytes` | 1 MiB | Lower to bound single-batch memory; raise only within the configured 16 MiB ceiling. |
| `batch.group_commit_max_batch` | `1` | Raise for fsync amortization; appends then require idempotency keys. |
| `index.topology` | `IndexTopology::aos()` | Add overlays when query benchmarks show broad scans or entity-local scans dominate. |
| `index.incremental_projection` | `false` | Enable when projections support pure incremental apply and replay cost is visible. |
| `index.enable_checkpoint` | `true` | Disable only for tiny stores or tests where cold-start artifacts are unwanted. |
| `index.enable_mmap_index` | `true` | Disable when the platform or deployment policy rejects mmap artifacts. |
| `platform_profile_path` via `with_platform_profile_path(...)` | `None` | Set when open must fail closed unless current platform evidence matches a recorded profile. |
| `signing_keys` via `with_signing_key(...)` | empty | Add when append and denial receipts need tamper-evident verification. |

## Receipt And Denial Notes

- `batpak::encoding::to_bytes` is the stable named-field MessagePack helper.
- `batpak::canonical` currently aliases the same encoding surface while the
  stronger canonical-bytes contract is phased in.
- `AppendReceipt` now carries `content_hash`, `key_id`, `signature`, and
  `extensions`.
- `DenialReceipt` mirrors the same receipt envelope for `SYSTEM_DENIAL`.
- `Store::append_denial(...)` persists denials on the ordinary per-entity
  chain; denial events are not a separate chain class.
- `verify_append_receipt(...)` and `verify_denial_receipt(...)` validate
  receipt signatures against the store's configured signing-key registry.

Key tradeoffs:

- lower `sync.every_n_events` = more durability, less throughput
- higher `fd_budget` = faster reads, more open descriptors
- larger writer mailbox = fewer producer stalls, more peak memory
- richer topology = more query acceleration, more insert/memory cost

## Benchmark Surfaces

- `crates/core/benches/projection_latency.rs`
- `crates/core/benches/unified_bench.rs`
- `crates/core/benches/writer_staging.rs`
- `crates/core/benches/writer_batch_staging.rs`
- `crates/core/benches/replay_lanes.rs`
- `crates/core/benches/topology_matrix.rs`
- `crates/core/benches/topology_write_cost.rs`

Canonical commands:

```bash
cargo xtask bench --surface neutral
cargo xtask bench --surface native
cargo xtask bench --surface neutral --compile
cargo xtask bench --surface native --compile
cargo xtask cover
cargo xtask cover --json
```

## Testing Doctrine

The doctrine surfaces live in:

- [HARNESS_DIRECTIVE.md](HARNESS_DIRECTIVE.md) for the five harness patterns
  and invariant/failure-mode/seed header rule enforced by
  `cargo xtask structural` for ledger-listed harnesses
- [HARNESS_LEDGER.md](HARNESS_LEDGER.md) for the current doctrine-bearing
  suites and their primary pattern
- `cargo xtask mutants policy` for the repo-owned mutation thresholds,
  critical seams, and repo-wide ratchet phase

Use these to classify strong suites by evidence shape. Do not treat perf gates,
chaos probes, loom proofs, and compile-fail parity tests as interchangeable
"more testing"; they answer different questions.

## Invariants

Build-time/runtime policy highlights:

1. no tokio in production dependencies
2. no async store API
3. no product-specific concepts in library declarations
4. no unsafe serialization shortcuts
5. Blake3 only for hash-chain integrity
6. public surface honesty checks must stay encoded in tooling

`crates/core/build.rs` and `tools/integrity/src/architecture_lints.rs` are both part of
the enforcement story.

## Authoritative Paths

- front door: `README.md`
- usage/workflows: `GUIDE.md`
- technical reference: `REFERENCE.md`
- decision index: `docs/README.md`
- harness doctrine: `HARNESS_DIRECTIVE.md` and `HARNESS_LEDGER.md`
- traceability registry: `traceability/artifacts.yaml`
- integrity entrypoint: `tools/integrity/src/main.rs`
- xtask command surface: `tools/xtask/src/commands.rs`
- architecture lints: `tools/integrity/src/architecture_lints.rs`
