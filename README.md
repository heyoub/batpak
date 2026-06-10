[![crates.io](https://img.shields.io/crates/v/batpak.svg)](https://crates.io/crates/batpak)
[![docs.rs](https://docs.rs/batpak/badge.svg)](https://docs.rs/batpak)
[![CI](https://github.com/heyoub/batpak/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/heyoub/batpak/actions/workflows/ci.yml)
[![license](https://img.shields.io/crates/l/batpak.svg)](#license)

# batpak

batpak is an embedded, sync-first event store for Rust: an append-only log
with typed payloads, Blake3 hash-chained ancestry, verifiable (optionally
Ed25519-signed) receipts, deterministic replay, and derived projections — in
one process, with no server and no async runtime.

Use it when you need a tamper-evident, replayable record of what happened:
agent action audit trails, local-first app logs, compliance evidence,
event-sourced application state.

batpak is not a database server, queue, ORM, workflow engine, async runtime,
network framework, or agent framework. Callers own process model, disk
placement, runtime integration, network boundaries, and application authority.

## Install

```sh
cargo add batpak
```

TypeScript clients for a networked batpak host install one npm package:
`@batpak/sdk`. See [bpk-ts/README.md](bpk-ts/README.md).

## First Shape

```rust
use batpak::prelude::*;

// One struct binds a Rust type to its event kind at compile time.
#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 1)]
struct PlayerMoved {
    x: i32,
    y: i32,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;

    // Open the Store: the battery. It owns this directory and nothing else.
    let store = Store::open(StoreConfig::new(dir.path()))?;

    // A Coordinate names where events belong: an entity within a scope.
    let coord = Coordinate::new("player:alice", "room:dungeon")?;

    // Append a cell of source truth. The receipt is verifiable evidence
    // of exactly what was accepted.
    let receipt = store.append_typed(&coord, &PlayerMoved { x: 10, y: 20 })?;

    // Read it back. Accepted events are immutable.
    let fetched = store.get(receipt.event_id)?;
    println!(
        "stored {} at sequence {} in scope {}",
        fetched.event.header.event_id,
        receipt.sequence,
        fetched.coordinate.scope()
    );

    store.close()?;
    Ok(())
}
```

Run it for real: `cargo run --example quickstart` under `bpk-lib/crates/core`.

The full beginner path is eight jobs — open, append, page commit order with
`query_entries_after`, point-read with `get`, walk hash-chain ancestry with
`walk_ancestors`, verify receipts, derive projections, close. See
`bpk-lib/crates/core/examples/eight_jobs.rs` for the contract example and
[COOKBOOK.md](COOKBOOK.md) for task-shaped recipes.

## Why Not SQLite With An Events Table?

SQLite gives you durable rows. batpak gives you durable rows plus proof:

- Every event is hash-bound to its per-entity ancestor with Blake3, so
  tampering and reordering are detectable, not silent.
- Every accepted write returns a receipt you can verify later — against the
  committed store, and against an Ed25519 signature when keys are configured.
- Projections are derived views rebuilt from the log by construction, so
  read models can never silently drift from source truth.
- Canonical bytes are stable across languages: the TypeScript codec is
  byte-for-byte parity-tested against the Rust encoder in CI.

When batpak is the wrong tool:

- You need ad-hoc SQL queries over relational data → SQLite or Postgres.
- You need one store shared by many writer processes or machines → a
  database server.
- You need maximum write throughput over verifiable history → batpak
  serializes appends through a single writer on purpose.
- You need distributed consensus or replication → batpak is a local truth
  boundary, by design.

## Can You Trust A 0.x Store?

Judge the evidence, not the version number:

- ~40k lines of tests against ~43k lines of source, including crash-recovery
  and cold-start suites.
- Deterministic concurrency proofs with `loom`, not just stress tests.
- Property-based tests over hash-chain integrity and canonical encoding.
- Chaos testing with fault injection, including disk-fault integration.
- Mutation testing on critical seams, so the tests are themselves tested.
- 71 named invariants traced to 116 concrete artifacts, enforced by an
  integrity gate that fails CI on orphaned or stale claims —
  see [INVARIANTS.md](INVARIANTS.md) and [CONFORMANCE.md](CONFORMANCE.md).

All of it runs from one command surface: `just verify`.

## The Mental Model

batpak ships with an opinionated mental model. You can use the API without
ever adopting it — the code above is the whole beginner story. But composition
gets much easier once you think in it, because every boundary question
("who owns this state? where may it cross?") already has a name.

The deeper project identity is Free Battery Factory:

> The Free Battery Factory makes batteries for software boundaries.
> A battery does not own the machine. It powers one boundary.

The Rosetta table — factory words on the left, the precise engineering
surface on the right:

| Factory word | Rust surface          | Plain engineering meaning                                          |
| ------------ | --------------------- | ------------------------------------------------------------------ |
| Battery      | `Store`               | An embedded append log that owns one directory, one boundary.      |
| Cell         | `Event`               | An immutable typed record; source truth.                           |
| —            | `Coordinate`          | Names where an event belongs: an entity within a scope.            |
| Terminal     | named API entry points| The only places where state or evidence crosses the boundary.      |
| Receipt      | `Receipt`             | Verifiable evidence of what was accepted, denied, or replayed.     |
| Discharge    | `Replay`              | Rebuild state from the cells.                                      |
| Gauge        | `Projection`          | A derived view: disposable, rebuildable, never source truth.       |
| Gate         | `Gate`                | Caller-defined policy evaluated before commit.                     |
| Circuit      | host wiring           | Connects terminals across batteries without hiding ownership.      |
| —            | `Capability`          | Explicit authority to perform an operation.                        |

Factory words explain the shape. Engineering names stay precise in the API,
and factory language never renames a Rust contract unless the type model
earns that name.

## Reading Paths

Pick the door that matches your intent — the docs are one model, but nobody
is required to read all of them:

- **Evaluating?** You have already read enough. Run the quickstart, skim
  [COOKBOOK.md](COOKBOOK.md), decide.
- **Building on the store?** [MODEL.md](MODEL.md) →
  [EVENTS.md](EVENTS.md) → [RECEIPTS.md](RECEIPTS.md) →
  [REPLAY.md](REPLAY.md) → [PROJECTIONS.md](PROJECTIONS.md) →
  [COOKBOOK.md](COOKBOOK.md).
- **Composing batteries or operating a host?** [FACTORY.md](FACTORY.md) →
  [BATTERIES.md](BATTERIES.md) → [TERMINALS.md](TERMINALS.md) →
  [CIRCUITS.md](CIRCUITS.md) → [INTEGRATION.md](INTEGRATION.md).
- **Auditing the guarantees?** [INVARIANTS.md](INVARIANTS.md) →
  [CONFORMANCE.md](CONFORMANCE.md).

Machine law lives in `bpk-lib/traceability/` and `bpk-lib/tools/integrity/`.
These root docs describe the current system; they are not a decision archive.

## Command Authority

Use the root `justfile`.

```sh
just list
just inspect
just verify
just perf-gates
just loom
just seal
just ship dry
```

`just` is the command counter. `xtask` is the factory machinery. `ast-grep`
inspects structural doctrine. Tests inspect behavior. Receipts preserve
evidence. `perf-gates` and `loom` are manual release-confidence proofs, not
part of `just verify`.

Raw `cargo`, `npm`, and `pnpm` are implementation details unless routed
through an explicit escape hatch:

```sh
just cargo -- <args>
just pnpm -- <args>
just npm -- <args>
```

## Current Rust Surface

Engineering names remain precise:

- `Store`
- `Coordinate`
- `Event`
- `Receipt`
- `Projection`
- `Replay`
- `Gate`
- `Capability`

Factory words explain the model. They do not rename the Rust API unless the
type model earns that name.

## License

Licensed under either of:

- Apache License, Version 2.0
- MIT license

at your option.
