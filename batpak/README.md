# batpak

Event-sourced state machines over coordinate spaces. Sync API. No tokio. No async.

## Features

| Feature | Flag | Description |
|---------|------|-------------|
| Segment store | always | Append-only event log with msgpack encoding and CRC32 integrity |
| Hash chain | `blake3` (default) | Blake3-based hash chain for tamper detection |
| Gate/Pipeline | always | Policy gates with TOCTOU-safe receipts and auditable bypass |
| Outcome monad | always | 6-variant result type (Ok, Err, Retry, Pending, Cancelled, Batch) with monad laws |
| Coordinate/Region | always | Entity + scope addressing with Region predicates for queries |
| Projections | always | Incremental projections over event streams with optional caching |
| Subscriptions | always | Reactive event subscriptions with flume channels |
| Typestate | always | Compile-time state machine transitions via phantom types |
| redb backend | `redb` | Embedded B-tree storage via redb |
| LMDB backend | `lmdb` | Memory-mapped storage via heed/LMDB |

## Architecture

Reading order (each layer builds on the previous):

```
coordinate  →  event  →  guard  →  pipeline  →  store
   WHO+WHERE     WHAT      MAY I?    COMMIT       PERSIST
```

- **Coordinate**: `(entity, scope)` — addresses WHO is acting and WHERE
- **Event**: `Event<P>`, `StoredEvent<P>` — typed payload with header, hash chain, causation DAG
- **Guard**: `Gate<Ctx>` + `GateSet` — policy evaluation returning `Receipt` (unforgeable proof) or `Denial`
- **Pipeline**: `Proposal<T>` → `Receipt` → `Committed<T>` — gate-then-commit workflow
- **Store**: Segment-based append-only log with sync API, projections, and subscriptions

## Quick Start

```rust
use batpak::prelude::*;

let config = StoreConfig::new("./batpak-data");
let store = Store::open(config)?;

let coord = Coordinate::new("player:alice", "room:dungeon")?;
let kind = EventKind::custom(0xF, 1);

let receipt = store.append(&coord, kind, &serde_json::json!({"x": 10, "y": 20}))?;
println!("Stored event {} at seq {}", receipt.event_id, receipt.sequence);
```

## Integrity Workflow

Use the checked-in Dev Container for the canonical environment, or run the same gates natively after a strict doctor check:

```bash
cargo run --manifest-path tools/integrity/Cargo.toml -- doctor --strict
just ci
```

Traceability and architectural proof live in `../traceability/` and `docs/adr/`.

## Design Invariants

1. **No async** — the Store API is synchronous. Async belongs in the product layer.
2. **No tokio** — zero async runtime dependency.
3. **No product concepts** — no User, no Account, no domain-specific types.
4. **No transmute** — safe Rust only, except LMDB FFI behind `#[cfg(feature = "lmdb")]`.
5. **Blake3 only** — single hash algorithm, feature-gated.

## Tuning

See [TUNING.md](TUNING.md) for configuration reference and tradeoff guidance.

## License

MIT OR Apache-2.0
