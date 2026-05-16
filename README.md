[![crates.io](https://img.shields.io/crates/v/batpak.svg)](https://crates.io/crates/batpak)
[![docs.rs](https://docs.rs/batpak/badge.svg)](https://docs.rs/batpak)
[![CI](https://github.com/heyoub/batpak/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/heyoub/batpak/actions/workflows/ci.yml)
[![license](https://img.shields.io/crates/l/batpak.svg)](#license)

# batpak

Sync-first event sourcing for Rust: append-only segments, causal metadata,
caller-defined gates, and typed projections.

```bash
cargo add batpak
```

Choose `batpak` when you want an embedded event log with typed payloads,
causal metadata, caller-defined gates, and projections in one Rust process. It is a
library substrate, not a hosted database: callers own the process model, disk
placement, and integration boundaries.

For repository navigation, start with
[`000_REPO_MAP.md`](000_REPO_MAP.md).

## Batpak Family Layers

The root crate stays substrate-facing. Companion crates layer runtime, kit, and
network boundaries without changing what batpak core is.

| Prefix | Crate | Role | Doc |
| --- | --- | --- | --- |
| `bp` | `batpak` | records events and receipts | [`001_BATPAK_SUBSTRATE.md`](001_BATPAK_SUBSTRATE.md) |
| `sb` | `syncbat` | runs sync checkouts over registered operations | [`002_SYNCBAT_RUNTIME.md`](002_SYNCBAT_RUNTIME.md) |
| `cb` | `clawbat` | declares operation-kit vocabulary | [`003_CLAWBAT_KIT.md`](003_CLAWBAT_KIT.md) |
| `nb` | `netbat` | exposes syncbat runtimes at network/server boundaries | [`004_NETBAT_NETWORK.md`](004_NETBAT_NETWORK.md) |

Layer rule:

```text
cb declares.
sb runs.
nb exposes.
bp records.
```

## Mental Model

```
coordinate → event → guard → pipeline → store
```

**coordinate**: an (entity, scope) pair. Every event lives at a coordinate.

**event**: a typed payload sealed with a UUID v7 ID, HLC timestamp, per-entity clock, and
Blake3 hash chain link.

**guard**: a `Gate` evaluates a `Proposal` and issues a `Receipt` or `Denial`. A `GateSet`
composes gates. Evaluation is fail-fast by default.

**pipeline**: `Pipeline::evaluate` runs the gates; `Pipeline::commit` persists through a
caller-supplied closure. The `Receipt` records the gate result returned to the caller.

**store**: the persistence engine — append-only segments, in-memory index, background
writer thread, projections, subscriptions.

## One Event Through The System

`store.append_typed(&coord, &payload)` — where `payload` is any `#[derive(EventPayload)]`
struct — serializes the payload to MessagePack, wraps it in an `Event<Vec<u8>>`, and sends
it as a `WriterCommand::Append` through a one-shot flume channel. The calling thread parks
waiting for the writer's response. (The underlying raw surface, `append(&coord, kind, &payload)`,
still exists for callers that compute `EventKind` dynamically.)

The writer thread receives the command and calls `WriterState::handle_append()`, which
executes a ten-step commit protocol: reads the entity's latest `IndexEntry` from the
in-memory DashMap; runs CAS and idempotency checks; computes `prev_hash` from the latest
entry, or uses the genesis `[0u8; 32]` for first-ever events; advances the per-entity
clock; sets the HLC wall-clock position monotonically; computes the Blake3 event hash
chained to `prev_hash`; encodes the wire frame as `[len:u32 BE][crc32:u32 BE][MessagePack
FramePayload]`; rotates the active `.fbat` segment if the size threshold is crossed, sealing
it and writing the SIDX footer; writes the frame to the active segment; and inserts the
`IndexEntry` into all index structures, calls `index.publish(global_seq + 1)` to make it
visible to readers, then broadcasts a `Notification` to subscribers.

The event now lives in three index structures: a per-entity `BTreeMap<ClockKey,
Arc<IndexEntry>>` ordered by HLC then clock, an O(1) `by_id` DashMap, and the `latest`
chain head. Readers access it via `store.get(id)` (index lookup then disk read),
`store.query(&region)` (index scan, no disk I/O), or `store.stream(entity)` (BTreeMap
range scan, no disk I/O).

## Store Internals At A Glance

Eight subdirectories organize the store by concern. Flat files alongside them
(`append.rs`, `config.rs`, `error.rs`, `fault.rs`, `gate.rs`,
`hidden_ranges.rs`, `lifecycle.rs`, `reactor_typed.rs`, `stats.rs`) hold
types that belong to the store root and don't fit
neatly into one subdirectory.

```
store/
├── write/        control corridor + writer rooms (append, batch, fence runtime, publish, runtime)
├── segment/      on-disk .fbat frame format and SIDX footer
├── index/        in-memory query engine: streams, by_id, columnar overlays, interner
├── cold_start/   open/restore: mmap → checkpoint → SIDX rebuild → frame scan
├── platform/     target-sensitive fs/sync/lock/clock/mmap helpers
├── projection/   state reconstruction: replay, cache, watcher
├── ancestry/     causal graph walking: by hash chain or by HLC clock
└── delivery/     push subscriptions (lossy) and pull cursors (ordered)
```

## Public Surface

**Typed payload binding** — `#[derive(EventPayload)]` on a named-field struct binds the Rust
type to its `EventKind` at compile time. Every typed write/read sibling below infers the
kind from `T::KIND`, so callsites never write `EventKind::custom(...)` directly.

**Append** — `append_typed`, `append_typed_with_options`, `append_reaction_typed`,
`append_batch` (with `BatchAppendItem::typed`), `apply_transition` (with
`Transition::from_payload`). Each returns an `AppendReceipt`. Non-blocking variants via
`submit_typed` / `try_submit_typed` return an `AppendTicket` you `.wait()` later. The raw
`append`, `append_reaction`, `submit`, `try_submit`, `append_with_options` still exist for
callers computing `EventKind` at runtime.

**Query** — `stream(entity)`, `by_scope(scope)`, `by_fact_typed::<T>()`, `query(&region)`,
`get(event_id)`, `walk_ancestors(id, limit)`. All return from the in-memory index; only
`get` and `walk_ancestors` read from disk. `by_fact(kind)` remains for dynamic-kind lookups.

**Projection** — `project::<T>(entity, &freshness)` folds events into any type
implementing `EventSourced`. `project_if_changed` skips work when nothing changed.
`watch_projection` returns a `ProjectionWatcher` that re-projects on subscription
events from a lossy/prunable watcher canal.
Two replay lanes: `JsonValueInput` (default, ergonomic) and `RawMsgpackInput` (perf).

**Delivery** — `subscribe_lossy(&region)` for push-based broadcast (may drop under load).
`cursor_guaranteed(&region)` for process-local pull-based ordered replay from the
in-memory index. Durable at-least-once across restarts is exposed by
`cursor_worker(..., CursorWorkerConfig { checkpoint_id: Some(CheckpointId::new(..)), .. })`
and typed reactors via `ReactorConfig::checkpoint_id: Option<CheckpointId>`.
Checkpoint-backed handlers receive `Some(&AtLeastOnce)` for exactly-once
composition with a caller-supplied `IdempotencyKey`; process-local handlers
receive `None`.
`Cursor::with_gap_config(...)` plus `Cursor::take_gaps()` expose in-memory
write-to-deliver gap observations without introducing a persisted system event.
`react_loop` is the legacy subscribe-based loop.

**Store control surface** — `submit`/`try_submit` for non-blocking fire-and-ticket. `outbox()` for
staged batch assembly. `begin_visibility_fence()` for atomic write groups. `open`, `close`,
`sync`, `snapshot`, `compact` for lifecycle. `stats()` and `diagnostics()` for
observability. `diagnostics().open_report` exposes the structured cold-start
receipt, `StoreConfig::with_open_report_observer(...)` lets callers export it,
and mutable opens append one durable `SYSTEM_OPEN_COMPLETED` lifecycle event at
`batpak:store` / `batpak:lifecycle`. The `batpak:` coordinate prefix is
reserved for library-owned lifecycle streams; application code should avoid it.
Receipt signing is opt-in via `StoreConfig::with_signing_key(...)`; signed
`AppendReceipt` and `DenialReceipt` values carry `key_id` and `signature`, and
`verify_append_receipt` / `verify_denial_receipt` re-check them against the
store's configured key registry. Callers can attach opaque receipt extension
bytes through `AppendOptions::with_extension(...)`; batpak signs those bytes
without interpreting profile meaning and persists them in `.fbat` frames for
cold-start reconstruction. Unknown extensions are substrate cargo: `pcp.*`
bytes and application-owned bytes are preserved and signed by the same generic
mechanism. Gate denials can be persisted as first-class
entity-chain events through `Store::append_denial(...)` using
`EventKind::SYSTEM_DENIAL`.

## Commands

| Command | What it does |
|---|---|
| `cargo xtask doctor` | Check tools and env |
| `cargo xtask ci` | Full test + lint + structural + bench-compile checks |
| `cargo xtask cover` | Coverage with retained artifacts |
| `cargo xtask mutants policy` | Print the repo-owned mutation policy |
| `cargo xtask mutants smoke` | Critical seam hard gates + repo-wide ratchet smoke |
| `cargo xtask platform ...` | Doctor/probe/verify/bless/audit platform profile workflows |
| `cargo xtask bench --surface neutral` | Criterion benchmark suite |
| `cargo xtask perf-gates` | Catastrophic-regression guards (stable hardware only) |
| `cargo xtask preflight` | Canonical verification bundle: CI + coverage + docs in one devcontainer session |
| `cargo xtask docs` | Build and check documentation |
| `cargo xtask release --dry-run` | Release preflight |

## Testing Doctrine

[040_TESTING_DOCTRINE.md](040_TESTING_DOCTRINE.md) defines the five harness
patterns used to classify doctrine-bearing test suites and the module-header
rule for new harnesses. `cargo xtask structural` now enforces the ledger
schema, module-header rule, and 500-line split discipline for ledger-listed
harnesses, with explicit capped legacy debt entries.
[041_TESTING_LEDGER.md](041_TESTING_LEDGER.md) records the current canonical witnesses,
including derive compile-fail/parity,
deterministic concurrency, chaos, fuzz-chaos feedback, perf gates, and
cold-start/replay consistency.
`cargo xtask mutants policy` prints the repo-owned mutation thresholds and
critical seams without running cargo-mutants.

## Operational Boundaries

**Sync store API.** `Store` methods are synchronous and production builds do not depend on
tokio, async-std, or smol. Async callers integrate at the edges via `spawn_blocking` or
flume's `recv_async`.

**Domain-free substrate.** Library concepts are coordinates, events, gates, pipelines, and
the store. Application nouns stay in caller payloads or opaque extension bytes.

**Native append-log storage.** Segments are coordinate-addressed `.fbat` append logs.

**Single live owner.** One live `Store` handle owns the directory lock at a time.
The directory lock is exclusive-only: a second mutable open or a concurrent
read-only open fails with `StoreLocked` instead of racing the same store
directory.

**Frame-level integrity.** Each frame carries a CRC32. Cold-start artifacts carry a
full-file CRC.

**Single-version operation.** Stop all writers before upgrading. Different binary versions
must not share an open store simultaneously.

See [010_USER_GUIDE.md](010_USER_GUIDE.md) for human-first workflows and usage patterns. See
[020_TECHNICAL_REFERENCE.md](020_TECHNICAL_REFERENCE.md) for the full technical reference and invariant catalog.

## License

MIT OR Apache-2.0
