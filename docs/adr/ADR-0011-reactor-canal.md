# ADR-0011: Reactor canal for typed reactors

## Status
Accepted (shipped in 0.6.0).

## Context

`EventPayload` (ADR-0010) closed the payload-binding seam. On top of that
binding, the shipped typed-reactor surface is `TypedReactive<T>` +
`react_loop_typed<T, R>()` and `#[derive(MultiEventReactor)]` +
`react_loop_multi(...)`.

Before designing the public typed-reactor surface, the question was:
**which canal feeds a typed reactor?**

The raw `react_loop` (`src/store/mod.rs:448-487`) rides an internal
fanout list (`reactor_subscribers: FanoutList<CommittedEventEnvelope>`) that
broadcasts committed events via a non-blocking `try_send` loop — the canal
is **lossy by construction**. A slow reactor's bounded channel fills; the
writer retains the sender and moves on. No error channel, no restart, no
checkpoint.

`cursor_guaranteed` exists alongside it: a pull-based, index-backed
canal that provides at-least-once ordered replay within a process, and
durable at-least-once replay across restart when `checkpoint_id` is set.
It is already wrapped by `cursor_worker`, which supplies restart policy,
panic recovery, checkpoint/resume, and clean stop.

This ADR compares the two canals and records the verdict consumed by the
shipped typed-reactor implementation.

## Current lossy fanout semantics

**Files cited**: `src/store/write/fanout.rs`, `src/store/mod.rs`, `src/store/write/writer/publish.rs`.

- **Delivery guarantee.** `FanoutList::broadcast` calls `sender.try_send(value.clone())`. Result handling: `Ok` or `Full` → retain sender; `Disconnected` → prune. Consequence: when a subscriber's bounded channel is full, the message is dropped at the writer side with no signal to the subscriber. Lossy.
- **Backpressure.** None. The writer never blocks on reactor capacity (by design — "NEVER use blocking send() — one slow subscriber must not block the writer"). Reactor latency is completely decoupled from writer throughput at the cost of drop-on-full.
- **Error surface.** None. `react_loop` calls `reactor.react(...)` which returns `Vec<(Coordinate, EventKind, P)>` — no `Result`. Any failure in `store.append_reaction(...)` emits `tracing::warn!` and moves on (`src/store/mod.rs:481`). The calling thread sees no error.
- **Restart / checkpoint.** None. A reactor-thread panic causes the thread to die. No supervisor, no retry, no checkpoint, no resume. Across store restart, the fanout subscription is gone entirely and any missed events are gone with it.
- **Writer-throughput coupling.** Zero (by `try_send`).
- **Decode cost locality.** `CommittedEventEnvelope` in `fanout.rs` carries both `Notification` (summary) and a pre-decoded `StoredEvent<serde_json::Value>`. The writer builds this envelope lazily only if `reactor_subscribers.has_subscribers()` in `writer/publish.rs`, so subscribed reactors pay a per-commit `serde_json::Value` allocation cost but save the re-read + decode on the reactor side.

## Cursor semantics

**Files cited**: `src/store/delivery/cursor.rs:1-229`, `src/store/mod.rs:858-862`.

- **Delivery guarantee.** Pull-based from the in-memory index. `Cursor::poll_batch(max)` at `cursor.rs:42-57` queries by `(region, position, started)` via `StoreIndex::query_hits_after` and returns up to `max` matching hits. The cursor advances only when events are consumed. "Guaranteed" here means at-least-once within process lifetime, and at-least-once across process restart when a `checkpoint_id` is set on `CursorWorkerConfig`.
- **Backpressure.** Natural — the reactor pulls. A slow reactor simply polls less frequently. The writer's commit-to-index visibility path is untouched.
- **Error surface.** `cursor_worker` at `cursor.rs:131-228` supplies a supervised thread with explicit outcomes: the handler returns `CursorWorkerAction::{Continue, Stop}`, panics are caught via `std::panic::catch_unwind`, thread join surfaces `WriterCrashed` error to `CursorWorkerHandle::join()`.
- **Restart / checkpoint.** Full: `RestartPolicy::{Once, Bounded { max_restarts, within_ms }}` at `cursor.rs:178-218`. On panic, the worker restores the last committed checkpoint via `Cursor::restore_checkpoint` and re-polls from there. When `CursorWorkerConfig.checkpoint_id: Option<String>` is set, checkpoints are persisted under `{data_dir}/cursors/{id}.ckpt` with parent-dir fsync so restart recovery spans process lifetime.
- **Writer-throughput coupling.** Zero — the writer writes, the cursor reads from index. No channel between them.
- **Decode cost locality.** The cursor returns `IndexEntry`; callers decode by calling `Store::get(event_id)` → `StoredEvent<serde_json::Value>`. That's one index lookup plus one disk read per event. Against the fanout's pre-decoded envelope, this is strictly more work on the reactor side — but the Dispatch Chapter's decode seam (ADR-0010 consumer, shipped in T1 of the Dispatch Chapter) will re-decode typed events from `Event<Value>` anyway, so the pre-decoded optimization is moot for a typed reactor.

## Comparison matrix

| Axis | Lossy fanout | `cursor_guaranteed` |
|---|---|---|
| Delivery guarantee | Lossy under backpressure | At-least-once within process lifetime; durable at-least-once across restart when `checkpoint_id` is set |
| Backpressure model | None (drop-on-full) | Natural (pull-based) |
| Error surface | None — `tracing::warn!` only | `JoinHandle<Result<(), StoreError>>` + supervised panic recovery |
| Restart / checkpoint | None | `RestartPolicy` + `Cursor::{checkpoint, restore_checkpoint}` |
| Writer-throughput coupling | Zero (via `try_send`) | Zero (via index decoupling) |
| Decode cost locality | Pre-decoded `StoredEvent<Value>` | Decode-in-reactor via `Store::get` + `DecodeTyped::route_typed` |
| Substrate work required | None for raw; upgrading it to guaranteed requires either per-subscriber overflow-to-disk OR blocking writer (both substantial) | None — `cursor_worker` already supplies every primitive a typed reactor needs |

## Verdict

**(a) `cursor_guaranteed` is the canal for typed reactors.**

Derivation from the matrix:

1. A typed reactor's public contract says "react to every event of kind
   `T`." Lossy delivery silently violates that contract. Guaranteed
   delivery is the minimum viable semantics for a typed public surface.
2. Every capability the typed reactor needs — error propagation,
   supervised restart, checkpoint/resume, clean stop — already exists as
   `cursor_worker`. Building on it is zero new substrate.
3. The only advantage of the lossy fanout (pre-decoded
   `StoredEvent<Value>`) is irrelevant because the Dispatch Chapter's
   `DecodeTyped` seam (T1) decodes from `Event<Value>` regardless.
4. Upgrading the lossy fanout to guaranteed delivery would require either
   coupling the writer's throughput to reactor latency (blocking send —
   explicitly rejected by the writer's comment at `fanout.rs:61`) or
   building per-subscriber overflow-to-disk. Both are substantial new
   substrate. Neither is justified when the existing substrate already
   meets the need.

**Raw surface preserved.** `react_loop` + `Reactive<P>` stay intact as the
lossy push variant (ADR-0010 / Dispatch Chapter invariant 6). Callers who
want the lossy, decoupled, pre-decoded-envelope path keep it. Typed
reactors are the additive at-least-once delivery variant built on
`cursor_worker`.

## Consequences

- **`TypedReactive<T>` + `react_loop_typed<T, R>()`** is implemented as a
  thin wrapper over `cursor_worker`. Its public `ReactorConfig` carries
  `RestartPolicy` and cursor-worker-compatible fields (`batch_size`,
  `idle_sleep`). The loop polls via `Cursor`, decodes via the
  `DecodeTyped` seam, builds a `ReactionBatch`, and flushes atomically on
  `Ok(())` from the reactor. Error path routes user errors and store
  errors through `TypedReactorHandle::join()` → `ReactorError<E>`.
- **`#[derive(MultiEventReactor)]` + `react_loop_multi(...)`** reuses the
  same canal unchanged. The multi-event loop runs a single cursor over the
  caller-supplied region and dispatches per event through the
  derive-generated `match` using the same decode seam. Wrong-kind events
  inside that region are filtered by the generated dispatch body. No
  independent canal decision.

No substrate work was required inside this ADR's scope — `cursor_worker`
already provided everything the typed reactor surface needs.

### Cross-reference to the Integrity Closeout

The typed reactor's behavior is pinned down by the Integrity Closeout
chapters:

- **Chapter A8** — cursor durable checkpoint under
  `{data_dir}/cursors/{id}.ckpt`. "Guaranteed" here means at-least-once
  across process restart when a `checkpoint_id` is set on
  `CursorWorkerConfig`, and at-least-once within process lifetime
  otherwise. Checkpoint write uses `persist_temp_with_parent_sync`, so the
  `cursors/` directory entry is durable before the worker observes the
  new checkpoint as recoverable.
- **Chapter D1** — reactor `join()` never returns `Ok(())` after crash;
  `ReactorError::RestartBudgetExhausted` surfaces when the
  `RestartPolicy` budget is spent.
- **Chapter D2** — `TypedReactorHandle::join` is passive; callers that
  want the prior implicit-stop behavior use `stop_and_join`.
- **Chapter D3** — restart policy governs handler panics, not explicit
  `Err` returns. An `Err` return surfaces immediately via `ReactorError`
  without consuming restart budget.
- **Chapter D6** — the lossy fanout drops slow subscribers on `Full`
  (no retain-on-Full). Typed reactors are unaffected because they run on
  the cursor canal.

## Scope boundary

- The lossy fanout's semantics and capacity are unchanged. It remains the
  raw reactor canal for callers using `react_loop` + `Reactive<P>`
  directly.
- Multi-cursor reactors (one cursor per event type) are not part of this
  ADR; `react_loop_multi` intentionally runs a single cursor so that
  `&mut self` spans all event types and dispatch remains strictly serial.
- Any guaranteed-delivery upgrade to the existing `reactor_subscribers`
  fanout is out of scope for this ADR.

## Cross-reference

- Raw surface: `src/store/mod.rs:448-487` (`react_loop`), `src/event/sourcing.rs:129-132` (`Reactive<P>`)
- Typed surface target: `src/store/reactor_typed.rs` (shipped in Dispatch Chapter T4b)
- Cursor primitives: `src/store/delivery/cursor.rs:8-229`
- Decode seam: ADR-0010 + Dispatch Chapter T1 (`src/event/decode.rs`)
