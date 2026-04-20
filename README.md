# batpak

Sync-first event sourcing for Rust: append-only segments, causal metadata, policy gates,
and typed projections — no async runtime.

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
caller-supplied closure. The `Receipt` is the unforgeable proof gates passed.

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

Seven subdirectories organize the store by concern. Flat files alongside them
(`append.rs`, `config.rs`, `error.rs`, `lifecycle.rs`, `stats.rs`,
`hidden_ranges.rs`) hold types that belong to the store root and don't fit
neatly into one subdirectory.

```
store/
├── write/        writer thread, staging, fanout, submit/tickets/outbox/fences
├── segment/      on-disk .fbat frame format and SIDX footer
├── index/        in-memory query engine: streams, by_id, columnar overlays, interner
├── cold_start/   open/restore: mmap → checkpoint → SIDX rebuild → frame scan
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
`watch_projection` returns a `ProjectionWatcher` that re-projects on subscription events.
Two replay lanes: `JsonValueInput` (default, ergonomic) and `RawMsgpackInput` (perf).

**Delivery** — `subscribe_lossy(&region)` for push-based broadcast (may drop under load).
`cursor_guaranteed(&region)` for process-local pull-based ordered delivery from the in-memory
index. Durable at-least-once across restarts is exposed by
`cursor_worker(..., CursorWorkerConfig { checkpoint_id: Some(..), .. })` and typed reactors via
`ReactorConfig::checkpoint_id`. `react_loop` is the legacy subscribe-based loop.

**Control plane** — `submit`/`try_submit` for non-blocking fire-and-ticket. `outbox()` for
staged batch assembly. `begin_visibility_fence()` for atomic write groups. `open`, `close`,
`sync`, `snapshot`, `compact` for lifecycle. `stats()` and `diagnostics()` for
observability.

## Commands

| Command | What it does |
|---|---|
| `cargo xtask doctor` | Check tools and env |
| `cargo xtask ci` | Full test + lint + structural + bench-compile checks |
| `cargo xtask cover` | Coverage with retained artifacts |
| `cargo xtask mutants policy` | Print the repo-owned mutation policy |
| `cargo xtask mutants smoke` | Critical seam hard gates + repo-wide ratchet smoke |
| `cargo xtask bench --surface neutral` | Criterion benchmark suite |
| `cargo xtask perf-gates` | Catastrophic-regression guards (stable hardware only) |
| `cargo xtask preflight` | CI + coverage + docs in one session (gold standard before push) |
| `cargo xtask docs` | Build and check documentation |
| `cargo xtask release --dry-run` | Release preflight |

## Testing Doctrine

[HARNESS_DIRECTIVE.md](HARNESS_DIRECTIVE.md) defines the five harness
patterns used to classify doctrine-bearing test suites and the module-header
rule for new harnesses.
[HARNESS_LEDGER.md](HARNESS_LEDGER.md) records the current canonical witnesses,
including derive compile-fail/parity,
deterministic concurrency, chaos, fuzz-chaos feedback, perf gates, and
cold-start/replay consistency.
`cargo xtask mutants policy` prints the repo-owned mutation thresholds and
critical seams without running cargo-mutants.

## What This Is Not

**No async runtime in production.** No tokio, no async-std, no futures in `[dependencies]`.
Async callers integrate at the edges via `spawn_blocking` or flume's `recv_async`.

**No product or domain concepts.** No users, orders, accounts, or payments in the library.
Only coordinates, events, gates, pipelines, and the store.

**No external database substrate.** Segments are native coordinate-addressed append logs.
No LMDB, no redb, no SQLite.

**No concurrent writers.** One writer thread owns commit order. Multiple readers are fine.
Two processes sharing a store directory will corrupt it.

**No per-entry integrity.** Each frame carries a CRC32. Cold-start artifacts carry a
full-file CRC. There is no per-byte or per-field checksum beyond that.

**No mixed-version concurrent operation.** Stop all writers before upgrading. Different
binary versions must not share an open store simultaneously.

See [GUIDE.md](GUIDE.md) for human-first workflows and usage patterns. See
[REFERENCE.md](REFERENCE.md) for the full technical reference and invariant catalog.

## License

MIT OR Apache-2.0
