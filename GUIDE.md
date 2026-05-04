# Guide

This is the human-first usage guide for `batpak`. Use it for workflows and
examples. Use [`README.md`](README.md) for orientation and
[`REFERENCE.md`](REFERENCE.md) for architecture, topology, tuning, and
invariants.

## Quickstart

```bash
cargo xtask setup --install-tools
cargo run --example quickstart
```

If you just want the crate:

```bash
cargo add batpak
```

## Define Your Payload

Every event is a typed payload. Use `#[derive(EventPayload)]` to bind a
Rust struct to its `EventKind` at compile time. The derive is the only
place a category/type_id pair should appear in your code; callsites
never touch `EventKind::custom(...)` again.

```rust
use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 1)]
struct PlayerMoved {
    x: i32,
    y: i32,
}
```

- `#[batpak(category = N, type_id = N)]` is required exactly once.
- `serde::Serialize + serde::Deserialize` are required; the derive does
  not generate them for you.
- The derive works on named-field structs only. Enums, unions, tuple
  structs, and unit structs are rejected with a compile-time error.
- Adding fields is wire-safe only if they are `Option<T>` or carry
  `#[serde(default)]`. Renaming, removing, or retyping a field requires
  bumping `type_id`.

## Append And Query

### Single event

```rust
use batpak::prelude::*;

let store = Store::open(StoreConfig::new("./data"))?;
let coord = Coordinate::new("user:alice", "chat:general")?;

let receipt = store.append_typed(&coord, &PlayerMoved { x: 10, y: 20 })?;
let event = store.get(receipt.event_id)?;
println!("entity={}, payload={}", event.coordinate.entity(), event.event.payload);
```

### Batch append

For atomic bulk insertion, use `Store::append_batch` with
`BatchAppendItem::typed`:

```rust
use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 1, type_id = 1)]
struct MessagePosted { text: String }

let items = vec![
    BatchAppendItem::typed(
        Coordinate::new("user:alice", "chat:general")?,
        &MessagePosted { text: "Hello".into() },
        AppendOptions::default(),
        CausationRef::None,
    )?,
    BatchAppendItem::typed(
        Coordinate::new("user:bob", "chat:general")?,
        &MessagePosted { text: "Hi!".into() },
        AppendOptions::default(),
        CausationRef::PriorItem(0),
    )?,
];

let receipts = store.append_batch(items)?;
assert_eq!(receipts.len(), 2);
```

Batch properties:

- one durable commit boundary
- atomic visibility
- restart recovery discards incomplete batches
- `CausationRef` can link events inside the batch

### Query patterns

```rust
let stream = store.stream("user:alice");
let scope = store.by_scope("chat:general");
let by_kind = store.by_fact_typed::<PlayerMoved>();
let region = store.query(
    &Region::scope("chat:general")
        .with_fact(KindFilter::Exact(PlayerMoved::KIND)),
);
```

Queries return `Vec<IndexEntry>` from the in-memory index. Use
`store.get(entry.event_id)` for full payload reads.

### Waiting for durability

When a caller needs a specific event to cross the durable frontier, wait on the
event's HLC with a mandatory timeout:

```rust
let target = batpak::store::HlcPoint {
    wall_ms: entry.wall_ms,
    global_sequence: entry.global_sequence,
};
store.wait_for_durable(target, std::time::Duration::from_secs(1))?;
```

The wait is synchronous. It returns `StoreError::WaitTimeout` if the deadline
expires and `StoreError::WriterCrashed` if the writer panics while the caller is
waiting.

#### Inline gating at append time

Use `AppendOptions::gate` when the append call itself should wait for a
frontier watermark:

```rust
let opts = AppendOptions::new().with_gate(DurabilityGate {
    kind: WatermarkKind::Durable,
    timeout: std::time::Duration::from_secs(1),
});
store.append_typed_with_options(&coord, &payload, opts)?;
```

The gate is opt-in. `StoreError::WaitTimeout` means the event committed, but the
requested watermark did not cross the event's HLC before the timeout.

### Append options

Compare-and-swap:

```rust
let opts = AppendOptions::new().with_cas(expected_sequence);
store.append_typed_with_options(&coord, &payload, opts)?;
```

Idempotency:

```rust
let opts = AppendOptions::new().with_idempotency(0xDEADBEEF_u128);
store.append_typed_with_options(&coord, &payload, opts)?;
```

Idempotency keys are required when `group_commit_max_batch > 1`.

Position hints:

```rust
let opts = AppendOptions::new().with_position_hint(AppendPositionHint::new(3, 1));
store.append_typed_with_options(&coord, &payload, opts)?;
```

Position-hint contract:

- callers control only `lane` and `depth`
- the writer still assigns `wall_ms`, `counter`, and `sequence`
- old checkpoints and mmap artifacts load missing lane/depth as root defaults
- old SIDX footers fall back cleanly to scan when the new footer format is absent

## Projections

A projection replays events for an entity and folds them into typed state.

```rust
use batpak::prelude::*;

#[derive(Default, Debug, serde::Serialize, serde::Deserialize)]
struct HitCounter {
    count: u64,
}

impl EventSourced for HitCounter {
    type Input = JsonValueInput;

    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        if events.is_empty() {
            return None;
        }
        let mut state = Self::default();
        for event in events {
            state.apply_event(event);
        }
        Some(state)
    }

    fn apply_event(&mut self, _event: &Event<serde_json::Value>) {
        self.count += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        static KINDS: [EventKind; 1] = [PlayerMoved::KIND];
        &KINDS
    }
}

let state: Option<HitCounter> = store.project("counter:hits", &Freshness::Consistent)?;
```

Projection APIs:

- `store.project(entity, &freshness)` for one-shot reconstruction
- `store.project_if_changed(entity, last_generation, &freshness)` to skip unchanged entities
- `store.entity_generation(entity)` for a cheap generation check
- `store.watch_projection(entity, freshness)` for live projection updates over a lossy/prunable watcher canal

Replay lanes:

- `JsonValueInput`: ergonomic default, payloads become `serde_json::Value`
- `RawMsgpackInput`: raw MessagePack bytes for throughput-sensitive projections

Use `JsonValueInput` first when projection clarity matters most. Promote a
projection to `RawMsgpackInput` when replay is on the hot path and measurement
shows the JSON decode lane is costing real time.

Current bench signal:

- `benches/replay_lanes.rs` is the current witness surface for the replay-lane
  tradeoff and currently shows `RawMsgpackInput` ahead on the 1k-event
  counter-shaped workload in this tree
- `examples/event_sourced_counter.rs` is the ergonomic default template
- `examples/raw_projection_counter.rs` is the performance-lane template

Use `supports_incremental_apply() -> true` on your `EventSourced` type plus
`StoreConfig::with_incremental_projection(true)` when the projection is a pure
fold over `apply_event`.

## Subscriptions And Cursors

Two consumption models exist: push and pull.

### `subscribe_lossy`

```rust
let sub = store.subscribe_lossy(&Region::entity("user:alice"));
while let Some(notif) = sub.recv() {
    println!("event {} kind {:?}", notif.event_id, notif.kind);
}
```

Use this for dashboards, live UI, and approximate live state. The writer's
fanout calls `try_send` into a bounded channel and drops the subscriber on
`Full` — slow subscribers are dropped, not retained.

### `scan()`

```rust
let mut live = store
    .subscribe_lossy(&Region::entity("counter:hits"))
    .ops()
    .scan(0u64, |count, _| {
        *count += 1;
        Some(*count)
    });
```

This remains lossy. It does not upgrade delivery semantics.

### `cursor_guaranteed`

```rust
let mut cursor = store.cursor_guaranteed(&Region::all());
while let Some(entry) = cursor.poll() {
    let event = store.get(entry.event_id)?;
    println!("{}", event.coordinate.entity());
}
```

Use cursor paths when you need ordered pull replay from the index.
Guarantee scope: at-least-once within the current process without a
checkpoint, or durable at-least-once across restart if
`CursorWorkerConfig.checkpoint_id` is set.

### `cursor_worker`

Use `cursor_worker(...)` for restartable background consumers with
`RestartPolicy` and explicit batch processing. Set
`CursorWorkerConfig.checkpoint_id: Option<CheckpointId>` to persist
resume position under `{data_dir}/cursors/{id}.ckpt` (written with
parent-dir fsync). Without a `checkpoint_id`, resume is in-memory only.
The handler receives a third argument, `Option<&AtLeastOnce>`: durable
checkpoint-backed workers receive `Some`, while process-local workers receive
`None`. Code that needs exactly-once composition can require `Some(witness)`
and combine it with an `IdempotencyKey` through `ObservedOnce::new`; ordinary
workers can name the parameter `_witness` and ignore it.
Use `Cursor::with_gap_config(...)` plus `take_gaps()` when you want
cursor-local write-to-deliver gap observations without emitting durable
system events. Startup
checkpoint load and validation failures are reported asynchronously from
`join()` / `stop_and_join()`, not from `cursor_worker(...)` itself.

## Control Plane

The simple path stays simple:

```rust
let receipt = store.append_typed(&coord, &payload)?;
```

But the control plane gives you more explicit execution shapes when needed.

### Submit and tickets

```rust
#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 1, type_id = 3)]
struct Tick { n: u64 }

let t1 = store.submit_typed(&coord, &Tick { n: 1 })?;
let t2 = store.submit_typed(&coord, &Tick { n: 2 })?;

let r1 = t1.wait()?;
let r2 = t2.wait()?;
```

Ticket surfaces:

- `wait(self)`
- `try_check(&self)`
- `receiver(&self)` for explicit `flume` integration

### Soft pressure with `Outcome`

```rust
match store.try_submit_typed(&coord, &payload)? {
    Outcome::Ok(ticket) => {
        let receipt = ticket.wait()?;
        println!("{}", receipt.sequence);
    }
    Outcome::Retry { after_ms, reason, .. } => {
        println!("retry after {after_ms}ms: {reason}");
    }
    other => unreachable!("unexpected control-plane outcome: {other:?}"),
}
```

Use `store.writer_pressure()` for direct mailbox telemetry.
Use `store.diagnostics().open_report` for the last structured cold-start
receipt. To export that receipt during startup without polling, configure
`StoreConfig::with_open_report_observer(...)`. Mutable opens also append a
durable `SYSTEM_OPEN_COMPLETED` event at `batpak:store` / `batpak:lifecycle`.
That `batpak:` coordinate prefix is reserved for library lifecycle streams and
should not be reused by application coordinates.

### Outbox batching

`Outbox::stage` collects events for one batch-shaped commit. Pass the
payload type's `KIND` constant so callsites stay free of literal
category/type_id pairs:

```rust
let mut outbox = store.outbox();
outbox.stage(coord.clone(), Tick::KIND, &Tick { n: 1 })?;
let receipts = outbox.flush()?;
assert_eq!(receipts.len(), 1);
```

### Visibility fences

`VisibilityFence` gives you durable-now, visible-on-commit semantics:
writes go to disk through the normal commit path but stay invisible to
readers until `fence.commit()` runs. Use the payload type's `KIND`
constant so callsites stay free of literal category/type_id pairs:

```rust
let fence = store.begin_visibility_fence()?;
let ticket = fence.submit(&coord, Tick::KIND, &Tick { n: 1 })?;
fence.commit()?;
let receipt = ticket.wait()?;
```

### Read-only mode

```rust
let ro = batpak::store::Store::<batpak::store::ReadOnly>::open_read_only(config)?;
let events = ro.by_fact_typed::<Tick>();
```

## Policy Gates

Use `Gate`, `GateSet`, `Proposal`, and `Pipeline` when you want a
gate-evaluate-commit workflow with receipts and explicit bypasses.

The rough shape is:

```rust
Proposal<T> -> GateSet::evaluate() -> Receipt<T> -> Pipeline::commit(...)
```

This is useful when the domain wants “approval before append” to be explicit
instead of scattered across ad hoc precondition checks.
