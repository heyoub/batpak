# free-batteries

Event-sourced state machines over coordinate spaces. Sync API. No tokio. No async.

## Install

```toml
[dependencies]
free-batteries = "0.1"
serde_json = "1"
```

### Feature flags

| Flag    | Default | Description                              |
|---------|---------|------------------------------------------|
| `blake3`| yes     | Hash chain integrity (disable for WASM)  |
| `redb`  | no      | Projection cache backed by redb          |
| `lmdb`  | no      | Projection cache backed by LMDB (heed)   |

## Quick start

```rust,no_run
use free_batteries::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = Store::open_default()?;
    let coord = Coordinate::new("player:alice", "room:dungeon")?;
    let kind = EventKind::custom(0xF, 1);

    let receipt = store.append(&coord, kind, &serde_json::json!({"x": 10, "y": 20}))?;
    println!("Stored event {} at seq {}", receipt.event_id, receipt.sequence);

    for entry in store.stream("player:alice") {
        let stored = store.get(entry.event_id)?;
        println!("{}: {:?}", stored.event.event_kind(), stored.event.payload);
    }
    Ok(())
}
```

## Module guide (reading order)

1. **`coordinate`** — Identify entities and scopes (`Coordinate`, `Region`, `DagPosition`)
2. **`event`** — Structure your events (`Event`, `EventHeader`, `EventKind`, `EventSourced`)
3. **`guard`** — Build policy gates (`Gate`, `GateSet`, `Denial`, `Receipt`)
4. **`pipeline`** — Propose and commit (`Proposal`, `Pipeline`, `Committed`)
5. **`store`** — Persist and query (`Store`, `StoreConfig`, `AppendOptions`)

## Pipeline → Store wiring

```rust,no_run
use free_batteries::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = Store::open_default()?;
    let gates: GateSet<()> = GateSet::new();
    let pipeline = Pipeline::new(gates);

    let coord = Coordinate::new("order:123", "scope:checkout")?;
    let kind = EventKind::custom(0xA, 1);
    let payload = serde_json::json!({"item": "widget", "qty": 3});

    let proposal = Proposal::new(payload.clone());
    let receipt = pipeline.evaluate(&(), proposal)?;
    let committed = pipeline.commit(receipt, |p| -> Result<_, StoreError> {
        let r = store.append(&coord, kind, &p)?;
        Ok(Committed { payload: p, event_id: r.event_id, sequence: r.sequence, hash: [0u8; 32] })
    })?;
    println!("Committed: {}", committed.event_id);
    Ok(())
}
```

## Projection (state from events)

```rust,no_run
use free_batteries::prelude::*;
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize)]
struct OrderTotal { amount: f64 }

impl EventSourced<serde_json::Value> for OrderTotal {
    fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
        let mut total = 0.0;
        for e in events {
            if let Some(amt) = e.payload.get("amount").and_then(|v| v.as_f64()) {
                total += amt;
            }
        }
        Some(OrderTotal { amount: total })
    }
    fn apply_event(&mut self, event: &Event<serde_json::Value>) {
        if let Some(amt) = event.payload.get("amount").and_then(|v| v.as_f64()) {
            self.amount += amt;
        }
    }
    fn relevant_event_kinds() -> &'static [EventKind] { &[] }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = Store::open_default()?;
    let total: Option<OrderTotal> = store.project("order:123", &Freshness::Consistent)?;
    if let Some(t) = total {
        println!("Order total: {}", t.amount);
    }
    Ok(())
}
```

## Subscriptions (push + pull)

```rust,no_run
use free_batteries::prelude::*;
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(Store::open_default()?);
    let region = Region::entity("order:*");

    // Push-based (lossy — bounded channel, slow subscribers drop events)
    let sub = store.subscribe(&region);
    std::thread::spawn(move || {
        while let Some(notif) = sub.recv() {
            println!("Push: event {} kind {:?}", notif.event_id, notif.kind);
        }
    });

    // Pull-based (guaranteed delivery — cursor tracks position)
    let mut cursor = store.cursor(&region);
    let batch = cursor.poll_batch(100);
    for entry in batch {
        println!("Pull: event {} seq {}", entry.event_id, entry.global_sequence);
    }

    Ok(())
}
```

## Async patterns

free-batteries is intentionally sync. For async callers:

```rust,ignore
// Store methods (append, get, query, project): use spawn_blocking
let result = tokio::task::spawn_blocking(move || store.append(&coord, kind, &payload)).await?;

// Subscriptions: use flume's built-in async receiver
let sub = store.subscribe(&region);
let notif = sub.receiver().recv_async().await?;
```

## License

MIT OR Apache-2.0
