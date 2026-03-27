# batpak

**Battery pack for event sourcing.** A complete, sync-first runtime for building event-sourced systems in Rust.

No async. No tokio. No product concepts. Just the engine.

## What is this?

batpak gives you an append-only event log with hash chain integrity, a DAG-based causation tracker, compile-time state machines, and a policy gate system — all behind a synchronous API. You bring the domain; batpak brings the infrastructure.

```rust
use batpak::prelude::*;

let store = Store::open(StoreConfig::new("/tmp/my-store"))?;
let coord = Coordinate::new("player:alice", "room:dungeon")?;
let kind = EventKind::custom(0xF, 1);

let receipt = store.append(&coord, kind, &serde_json::json!({"x": 10, "y": 20}))?;
```

## Project Layout

```
batpak/          Rust crate — the library itself
  src/           Source code (reading order: coordinate → event → guard → pipeline → store)
  tests/         Integration, property-based, chaos, and UI tests
  benches/       Criterion benchmarks (write throughput, cold start, projections, etc.)
  examples/      Runnable examples demonstrating core patterns
SPEC.md          Technical specification
SPEC_REGISTRY.md Detailed build registry
CONTRIBUTING.md  How to build, test, and contribute
```

## Quick Links

| Document | What it covers |
|----------|---------------|
| [batpak/README.md](batpak/README.md) | Features, architecture, quick start, design invariants |
| [batpak/ARCHITECTURE.md](batpak/ARCHITECTURE.md) | Deep narrative walkthrough of the module graph |
| [batpak/TUNING.md](batpak/TUNING.md) | StoreConfig reference, tradeoff matrix, deployment examples |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Build, test, lint, pre-submit checklist |
| [SPEC.md](SPEC.md) | Full technical specification |

## License

MIT OR Apache-2.0
