# Append and query

Use `Store::append`, `Store::get`, `Store::query`, `Store::stream`, `Store::by_scope`, and `Store::by_fact` for the basic event-log workflow. The storage boundary returns `StoredEvent<serde_json::Value>`, which carries both the `Coordinate` and the decoded event payload/header.

## Batch append

For atomic bulk insertion, use `Store::append_batch`:

```rust
use batpak::prelude::*;
use batpak::store::{BatchAppendItem, CausationRef};

let items = vec![
    BatchAppendItem::new(
        Coordinate::new("user:alice", "chat:general")?,
        EventKind::custom(1, 1),
        &json!({"text": "Hello"}),
        AppendOptions::default(),
        CausationRef::None,
    )?,
    BatchAppendItem::new(
        Coordinate::new("user:bob", "chat:general")?,
        EventKind::custom(1, 1),
        &json!({"text": "Hi!"}),
        AppendOptions::default(),
        CausationRef::None,
    )?,
];

let receipts = store.append_batch(items)?;
// All events committed atomically or none visible
```

**Key properties:**
- All events in a batch share a single fsync — significantly higher throughput than individual appends
- Atomic visibility: subscribers see all events or none (no partial batches)
- Crash recovery: incomplete batches (missing COMMIT marker) are discarded on restart
- `CausationRef` links events within a batch: `PriorItem(index)`, `PriorItemInEntity(index, entity)`, or `ExplicitEventId(id)`
