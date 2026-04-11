# batpak

Sync-first event sourcing for Rust: append-only log, causal metadata, policy gates, projections, subscriptions, and typestate-friendly workflows without an async runtime.

## Start In 5 Minutes

```bash
cargo xtask setup
cargo run --example quickstart
```

If you just want the crate:

```bash
cargo add batpak
```

```rust
use batpak::prelude::*;

let config = StoreConfig::new("./batpak-data")
    .with_sync_every_n_events(100)
    .with_sync_mode(SyncMode::SyncData);
let store = Store::open(config)?;

let coord = Coordinate::new("player:alice", "room:dungeon")?;
let kind = EventKind::custom(0xF, 1);
let receipt = store.append(&coord, kind, &serde_json::json!({"x": 10, "y": 20}))?;
println!("stored {} at {}", receipt.event_id, receipt.sequence);
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Pick Your Lane

- User lane: `cargo build`, `cargo test`, `cargo run --example quickstart`
- Maintainer lane: `cargo xtask doctor`, `cargo xtask ci`
- Perf lane: `cargo xtask bench --surface neutral|native [--save|--compare]`

## What You Get

- Append-only segment store with CRC32 integrity
- Optional Blake3 hash chains
- Causal metadata and region queries
- **Atomic batch append**: multi-event commit with two-phase markers, crash recovery, and intra-batch causation
- Fault injection framework (`test-support` feature) for chaos testing batch and write paths
- Gate / receipt workflow for policy enforcement
- Event-sourced projections with optional native file-backed cache
- Push subscriptions, pull cursors, typestate helpers
- Query/read operations yield `StoredEvent<serde_json::Value>` at the storage boundary

## Docs

- [Guide](guide/src/SUMMARY.md)
- [Architecture reference](docs/reference/ARCHITECTURE.md)
- [Tuning reference](docs/reference/TUNING.md)
- [Contributing](CONTRIBUTING.md)
- [Agent guide](AGENTS.md)
- [Specification](docs/spec/SPEC.md)

## Features

- `blake3` (default): hash-chain verification
- `test-support`: explicit test-only runtime hooks

## Canonical Commands

```bash
cargo xtask doctor
cargo xtask ci
cargo xtask docs
```

`just` remains available as shorthand, but `cargo xtask` is the canonical command surface.

## License

MIT OR Apache-2.0
