# batpak

Sync-first event sourcing for Rust: append-only log, causal metadata, policy gates, projections, subscriptions, and typestate-friendly workflows without an async runtime.

This README is the primary entrypoint. If you only read one document, read
this one. The rest of the docs should exist to go deeper, not to replace the
front door.

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

store.close()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Pick Your Lane

- User lane: `cargo build`, `cargo test`, `cargo run --example quickstart`
- Maintainer lane: `cargo xtask doctor`, `cargo xtask install-hooks`, `cargo xtask ci`, `cargo xtask preflight` (gold standard before pushing; one canonical devcontainer proof session)
- Perf lane: `cargo xtask bench --surface neutral|native [--save|--compare|--compile]`
- Coverage lane: `cargo xtask cover [--ci|--json|--threshold N]`

## Mental Model

Think about batpak in five layers:

- `Coordinate`: who and where
- `Event`: what happened
- `Gate` / `Receipt`: may this happen
- `Pipeline`: approve then commit
- `Store`: persist, query, replay, subscribe

The runtime stays sync on purpose. Async integration happens around it, not
inside it.

## What You Get

- Append-only segment store with CRC32 integrity
- Optional Blake3 hash chains
- Causal metadata and region queries
- **Atomic batch append**: multi-event commit with two-phase markers, crash recovery, and intra-batch causation
- Fault injection framework (`dangerous-test-hooks` feature) for chaos testing batch and write paths
- Gate / receipt workflow for policy enforcement
- Event-sourced projections with optional native file-backed cache
- `subscribe_lossy` / `cursor_guaranteed` delivery names that say what they do
- `close(self) -> Closed` for explicit durable shutdown; `Drop` is best-effort only
- Query/read operations yield `StoredEvent<serde_json::Value>` at the storage boundary

## Projection Lanes

`JsonValueInput` is the ergonomic default replay lane. Start there when you want
the clearest projection code and easiest onboarding.

`RawMsgpackInput` is the performance lane. Use it when replay cost matters and
you want to deserialize directly from MessagePack bytes inside the projection.

- `cargo run --example event_sourced_counter` shows the default JSON lane
- `cargo run --example raw_projection_counter` shows the raw replay lane

The current `benches/replay_lanes.rs` quick surface consistently shows raw
replay ahead of `JsonValueInput` on the 1k-event counter-shaped workload, so
raw replay should be treated as real engineering value, not as an obscure
trick.

## Docs

Keep the live docs small and root-first:

- [GUIDE.md](GUIDE.md) for workflows and usage patterns
- [REFERENCE.md](REFERENCE.md) for architecture, tuning, topology, replay lanes, and invariants
- [CONTRIBUTING.md](CONTRIBUTING.md) for repo workflow
- [CHANGELOG.md](CHANGELOG.md) for release history
- [AGENTS.md](AGENTS.md) for agent-facing repo guidance

The goal is one real front door plus a small handful of actual docs, not a
maze of half-authoritative pages.

## Features

- `blake3` (default): hash-chain verification
- `dangerous-test-hooks`: explicit test-only runtime hooks

## Canonical Commands

```bash
cargo xtask doctor
cargo xtask ci
cargo xtask docs
cargo xtask preflight    # CI + coverage + docs in one canonical devcontainer session
cargo xtask perf-gates   # catastrophic-regression perf guards; interpret on stable hardware
cargo xtask cover        # coverage feedback with retained artifacts under target/xtask-cover/last-run
```

`just` remains available as shorthand, but `cargo xtask` is the canonical command surface.

## Docs Policy

- Keep the README as the main human and agent entrypoint.
- Keep `GUIDE.md` for workflows and usage.
- Keep `REFERENCE.md` for technical truth that would bloat the README.

That gives us one smart front door plus a few deliberate root-level docs,
instead of a sprawl or a single unreadable wall of text.

## License

MIT OR Apache-2.0
