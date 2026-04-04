# Architecture

This document walks through batpak's internals in the order you'd encounter them while building a product on top of it. Each layer depends only on the ones before it.

## The Five Layers

```
coordinate  →  event  →  guard  →  pipeline  →  store
  WHO+WHERE     WHAT      MAY I?    COMMIT       PERSIST
```

### 1. Coordinate — WHO is acting, WHERE

Every event lives at a **Coordinate**: an `(entity, scope)` pair.

- **entity** is the stream key. Think `"order:507"`, `"player:alice"`, `"sensor:room-4"`. Hash chains are per-entity. Projections fold per-entity.
- **scope** is the isolation boundary. Think `"workspace:acme"`, `"game:session-9"`. Scopes let you shard storage and scope subscriptions.

Coordinates use `Arc<str>` internally — cheap to clone, zero-copy in the hot path.

**Region** is the query predicate. Instead of separate filter types for queries, subscriptions, and cursors, everything uses `Region`:

```rust
Region::entity("order:507")                     // exact entity
Region::scope("workspace:acme")                 // all entities in a scope
Region::all().with_fact(KindFilter::Exact(k))   // all events of a specific kind
Region::entity("sensor:*").with_clock_range((10, 50))  // entity with clock window
```

One predicate type. Four access patterns (query, subscribe, cursor, walk).

### 2. Event — WHAT happened

An **Event\<P\>** is a typed payload `P` with an `EventHeader`:

- `event_id: u128` — UUIDv7, globally unique, time-sortable
- `correlation_id: u128` — groups related events (defaults to event_id for root causes)
- `causation_id: Option<u128>` — points to the event that caused this one (None = root)
- `timestamp_us: i64` — microseconds since epoch (injectable clock for testing)
- `position: DagPosition` — HLC wall clock + depth/lane/sequence for causal ordering
- `event_kind: EventKind` — category:type encoding (upper 4 bits = category, lower 12 = type)
- `flags: u8` — bitfield for REQUIRES_ACK, TRANSACTIONAL, REPLAY
- `content_hash: [u8; 32]` — for projection cache invalidation

**EventKind** is a sealed u16. Products use `EventKind::custom(category, type_id)`. System kinds (0x0xxx) and effect kinds (0xDxxx) are reserved by the library. This prevents product code from accidentally creating system events.

**HashChain** (feature-gated behind `blake3`) links each event to its predecessor via Blake3 hashes. When blake3 is off, all hashes are `[0u8; 32]` — the chain is structurally present but not verified.

### 3. Guard — MAY I do this?

A **Gate\<Ctx\>** is a pure predicate: given a context, it returns `Ok(())` or `Denial`. Gates are:
- Pure functions — no I/O, no mutation, no side effects
- Composable — stack them in a `GateSet` for fail-fast or evaluate-all
- Generic — `Ctx` is whatever your product defines (user session, feature flags, request context)

When all gates pass, `GateSet::evaluate()` returns a **Receipt\<T\>** — an unforgeable proof that the payload cleared all gates. The `Receipt` type is sealed: only `GateSet::evaluate()` can construct one (the token lives in a private module). This prevents TOCTOU bugs — you can't fabricate a receipt or use a stale one.

**Denial** carries structured error context: which gate failed, a code, a message, and arbitrary key-value context. Products decide whether to persist denials as events.

### 4. Pipeline — COMMIT the change

The **Pipeline** orchestrates the gate-then-commit workflow:

```
Proposal<T>  →  GateSet::evaluate()  →  Receipt<T>  →  commit_fn()  →  Committed<T>
```

- `Proposal::new(payload)` wraps a value for evaluation
- `pipeline.evaluate(&ctx, proposal)` runs all gates, returns a Receipt or Denial
- `pipeline.commit(receipt, |payload| { ... })` consumes the receipt and calls your commit function

The commit function is generic over error type — Pipeline doesn't know about `StoreError`. Products pass a closure that calls `store.append()`.

**Bypass** is the escape hatch. When you need to skip gates (migrations, admin overrides), `Pipeline::bypass(proposal, reason)` creates a `BypassReceipt` with an auditable justification. The audit trail shows exactly who bypassed what and why.

### 5. Store — PERSIST and query

The **Store** is the runtime. It manages:

- **Write path**: `append()` serializes to MessagePack, sends to the background writer thread via flume channel, writer appends to the active segment file, computes CRC32 + optional Blake3 hash, indexes the event, broadcasts to subscribers.
- **Read path**: `get()`, `query()`, `stream()`, `by_scope()`, `by_fact()` — all sync, all go through the in-memory index.
- **Projection path**: `project::<T>(entity, freshness)` replays events through `EventSourced::from_events()` with optional caching (NoCache, RedbCache, LmdbCache).
- **Subscription path**: `subscribe(region)` returns a push-based `Subscription` (lossy, bounded flume channel). `cursor(region)` returns a pull-based `Cursor` (guaranteed delivery).
- **Lifecycle**: `sync()`, `snapshot()`, `compact()`, `close()`.

### The Writer Thread

All writes go through a single background thread (`batpak-writer-{hash}`). This serializes all mutations, eliminating lock contention. The thread communicates via bounded flume channels:

```
caller → [flume bounded channel] → writer thread → segment file
                                  → index update
                                  → subscriber broadcast
         [flume oneshot]        ← AppendReceipt
```

When the channel is full, producers block — this is intentional back-pressure. Tune `writer_channel_capacity` to control the threshold.

### Storage Format

Events are stored in **segment files** — append-only files with MessagePack-encoded frames and CRC32 checksums. When a segment exceeds `segment_max_bytes`, the writer seals it and creates a new one. Sealed segments are immutable and safe for concurrent reads.

**Cold start** uses a three-tier strategy: (1) if an `index.ckpt` checkpoint file exists and is valid, restore the index from it and replay only segments written after the checkpoint watermark; (2) if sealed segments carry SIDX footers (compact binary index), use those for fast per-segment index rebuild without full msgpack deserialization; (3) fall back to frame-by-frame segment scanning. Sealed segments are memory-mapped via `memmap2` for zero-copy reads; the active segment uses pread (Unix) or seek+read (Windows).

**Secondary scan index**: When `IndexLayout` is set to `SoA`, `AoSoA8/16/64`, or `SoAoS`, a columnar secondary index replaces the `by_fact` and `scope_entities` DashMaps for cache-friendly scan queries. AoSoA variants use const-generic `Tile<N>` structs with `#[repr(C, align(64))]` for cache-line alignment.

**Group commit**: The writer can batch multiple appends before a single fsync, controlled by `group_commit_max_batch`. When batch > 1, all appends must include idempotency keys for crash safety.

### Public API Witness Index

The advanced store surface intentionally includes `SyncMode`, `AppendReceipt`, `AppendOptions`, `RetentionPredicate`, `CompactionStrategy`, `CompactionConfig`, `StoreStats`, and `StoreDiagnostics`.

The low-level storage surface intentionally includes `SEGMENT_MAGIC`, `SEGMENT_EXTENSION`, `SegmentHeader`, `FramePayload`, `FrameDecodeError`, `frame_encode`, `frame_decode`, `segment_filename`, and `CompactionResult`.

## Cross-Cutting Patterns

### EventSourced — backward fold

```rust
trait EventSourced<P>: Sized {
    fn from_events(events: &[Event<P>]) -> Option<Self>;
    fn apply_event(&mut self, event: &Event<P>);
    fn relevant_event_kinds() -> &'static [EventKind];
}
```

Implement this on your projection types. `Store::project()` calls `from_events()` on the entity's event stream. The cache stores serialized projections keyed by entity, invalidated by watermark (global sequence).

### Reactive — forward fan-out

```rust
trait Reactive<P> {
    fn react(&self, event: &Event<P>) -> Vec<(Coordinate, EventKind, P)>;
}
```

See an event, maybe emit derived events. `Store::react_loop()` spawns a thread that subscribes, reads events, calls `react()`, and appends reactions — 7 lines of glue, automated.

### Outcome — algebraic result type

Six variants: `Ok(T)`, `Err(OutcomeError)`, `Retry { after_ms, attempt, max_attempts, reason }`, `Pending { condition, resume_token }`, `Cancelled { reason }`, `Batch(Vec<Outcome<T>>)`.

Satisfies the monad laws (verified by proptest). Combinators: `map`, `and_then`, `flatten`, `join_all`, `join_any`, `zip`.

### Typestate — compile-time state machines

```rust
define_state_machine!(DoorState { Open, Closed, Locked });
define_typestate!(Door<S: DoorState> { name: String });
```

`Transition<From, To, P>` encodes legal state changes. The compiler rejects invalid transitions — you literally can't write `Transition<Locked, Open, _>` unless your code allows it.

## Invariants

These are enforced at compile time (via `build.rs` and `compile_error!` guards):

1. **No tokio in production deps** — `build.rs` scans Cargo.toml
2. **No async in store** — `build.rs` rejects `async fn` in any file with "store" in its path
3. **No product concepts** — `build.rs` scans for banned nouns (trajectory, artifact, tenant)
4. **No unsafe serialization** — `build.rs` rejects transmute, mem::read, pointer_cast
5. **Blake3 only** — `compile_error!` prevents sha256 feature
6. **Sync store API** — `compile_error!` prevents async-store feature

## Build-Time Invariant Enforcement

`build.rs` runs five compile-time checks that enforce structural invariants. These are not optional — they fail the build immediately.

| Check | What it enforces |
|-------|-----------------|
| `check_pub_items_have_tests` | Every `pub` item in `src/` must appear by name in at least one test file. Items tested only indirectly are listed in the allowlist with a written justification. |
| `check_allow_justifications` | Every `#[allow(...)]` in `src/` must have a comment on the same line explaining why. Prevents silent lint suppression. |
| `check_no_stubs_in_src` | No placeholder strings (`todo!`, `unimplemented!`, stub sentinels) in `src/`. |
| `check_no_tokio_in_deps` | `tokio` must not appear in `Cargo.toml` dependencies. Enforces the sync-only design invariant. |
| `check_store_config_field_usage` | Every field of `StoreConfig` must be referenced in `src/store/mod.rs`. Prevents dead config fields. |

When adding a new `pub` item, either add a test that names it, or add it to the allowlist in `build.rs` with a justification explaining where it is exercised.
