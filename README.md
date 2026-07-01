[![crates.io](https://img.shields.io/crates/v/batpak.svg)](https://crates.io/crates/v/batpak)
[![docs.rs](https://docs.rs/batpak/badge.svg)](https://docs.rs/batpak)
[![CI](https://github.com/freebatteryfactory/batpak/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/freebatteryfactory/batpak/actions/workflows/ci.yml)
[![license](https://img.shields.io/crates/l/batpak.svg)](#license)

# batpak

The Free Battery Factory makes batteries for software boundaries.

> A battery does not own the machine. It powers one boundary.

**batpak** is the core battery: an embedded, sync-first append-only journal with
typed payloads, Blake3 hash-chained ancestry, verifiable receipts, deterministic
replay, and derived projections. The **family** around it wires that journal into
larger hosts — `syncbat` for runtime dispatch, `netbat` for NETBAT/1 network
terminals, and `hostbat` for manifest-owned host contracts. Circuits connect
batteries without one owning another's state.

Use it when you need a tamper-evident, replayable record of what happened:
agent action audit trails, local-first app logs, compliance evidence,
event-sourced application state.

batpak is not a database server, queue, ORM, workflow engine, async runtime,
network framework, or agent framework. Callers own process model, disk
placement, runtime integration, network boundaries, and application authority.

## What Ships

| Battery / surface | Crate / package | Role |
| --- | --- | --- |
| Core journal | `batpak` | Append-only store, HLC frontier, receipts, replay, projections |
| Runtime dispatch | `syncbat` | Operation descriptors, handler registration, runtime receipts |
| Network terminal | `netbat` | NETBAT/1 frames, bounded request/response |
| Host contract | `hostbat` | Client manifest, H-interface fingerprints, subscription descriptors |

See [04_BATTERIES.md](04_BATTERIES.md) for the full battery map and
[05_TERMINALS.md](05_TERMINALS.md) for the ten-op NETBAT profile.

## Two Doors

**Door A — Rust embedded.** Add the core crate and open a `Store` on a
directory you own:

```sh
cargo add batpak
```

**Door B — Networked host.** Add the runtime and wire crates, then prove the
NETBAT surface with integration tests:

```sh
cargo add syncbat netbat
cargo test -p netbat
```

The ten reference NETBAT terminals — `bank.commit`, `event.query`, `event.get`,
`receipt.verify`, `event.walk`, `system.heartbeat`, and the four `evidence.*` ops — are documented
in [05_TERMINALS.md](05_TERMINALS.md). The Rust `hostbat` crate projects the live
host contract through `ClientManifest`.

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

Run it for real: `cargo run -p batpak-examples --bin quickstart` under `bpk-lib/crates/batpak-examples`.

The full beginner path is eight jobs — open, append, page commit order with
`query_entries_after`, point-read with `get`, walk hash-chain ancestry with
`walk_ancestors`, verify receipts, derive projections, close. See
`bpk-lib/crates/batpak-examples/src/bin/eight_jobs.rs` for the contract example and the
[cookbook](cookbook/README.md) for task-shaped recipes. The cookbook is the
task-shaped field guide: each recipe maps intent to API to proof surface, and
its index lives at [cookbook/README.md](cookbook/README.md).

## Scale And Composition

A **journal** is one `Store` on one `data_dir` with one exclusive writer. That
scope is the **local truth boundary** — truth is bounded to that journal, not
denied to distributed systems.

**Scale out** with multiple journals and explicit circuits: `netbat` routes,
cross-store observations, and host wiring documented in
[08_CIRCUITS.md](08_CIRCUITS.md) and [11_INTEGRATION.md](11_INTEGRATION.md). There is no
single `global_sequence` across separate store roots, and no in-core Raft over
one mutable directory.

**HLC** (hybrid logical clock) is the per-journal frontier inside one writer:
accepted → written → durable → visible → applied watermarks. `wait_for_durable`,
batch gates, and projection progress use those watermarks. HLC coordinates
durability and visibility inside a journal; it is not cross-machine consensus.

## Why Not SQLite With An Events Table?

SQLite gives you durable rows. batpak gives you durable rows plus proof:

- Every event is hash-bound to its per-entity ancestor with Blake3, so
  tampering and reordering are detectable, not silent.
- Every accepted write returns a receipt you can verify later — against the
  committed store, and against an Ed25519 signature when keys are configured.
- Projections are derived views rebuilt from the log by construction, so
  read models can never silently drift from source truth.
- Canonical bytes are stable inside the Rust substrate and sealed by Rust
  golden vectors; non-Rust clients are intentionally deferred until after the
  Rust host/schema contracts settle.

When batpak is the wrong tool:

| Need | Reach for |
| --- | --- |
| Ad-hoc SQL over relational data | SQLite or Postgres |
| Many writers on one mutable directory with leader election | A database server or etcd — batpak is one writer per `data_dir` |
| Maximum write throughput over verifiable history | batpak serializes appends through a single writer on purpose |
| Automatic Raft replication inside the core crate | Compose multiple journals and explicit host circuits instead |

## Can You Trust A 0.x Store?

Judge the evidence, not the version number:

- A deep test surface — integration, property, crash-recovery, and
  cold-start suites, not a thin unit-test layer.
- Deterministic concurrency proofs with `loom`, not just stress tests.
- Property-based tests over hash-chain integrity and canonical encoding.
- Chaos testing with fault injection, including disk-fault integration.
- Mutation testing on critical seams, so the tests are themselves tested.
- 102 named invariants traced to 148 concrete artifacts, enforced by an
  integrity gate that fails CI on orphaned or stale claims —
  see [03_INVARIANTS.md](03_INVARIANTS.md) and [12_CONFORMANCE.md](12_CONFORMANCE.md).

All of it runs from one command surface: `just verify`.

## The Mental Model

batpak ships with an opinionated mental model. You can use the API without
ever adopting it — the code above is the whole beginner story. But composition
gets much easier once you think in it, because every boundary question
("who owns this state? where may it cross?") already has a name.

The Rosetta table — factory words on the left, the precise engineering
surface on the right:

| Factory word | Rust surface | Plain engineering meaning |
| --- | --- | --- |
| Battery | `Store` | An embedded append log that owns one directory, one boundary. |
| Journal | one `data_dir` | One append-only store root; one exclusive writer. |
| Cell | `Event` | An immutable typed record; source truth. |
| — | `Coordinate` | Names where an event belongs: an entity within a scope. |
| Terminal | named API entry points | The only places where state or evidence crosses the boundary. |
| Receipt | `Receipt` | Verifiable evidence of what was accepted, denied, or replayed. |
| Discharge | `Replay` | Rebuild state from the cells. |
| Gauge | `Projection` | A derived view: disposable, rebuildable, never source truth. |
| Frontier | HLC watermarks | Per-journal accepted → durable → visible → applied progress inside one writer. |
| Gate | `Gate` | Caller-defined policy evaluated before commit. |
| Circuit | host wiring | Connects terminals across batteries without hiding ownership. |
| — | `Capability` | Explicit authority to perform an operation. |

Factory words explain the shape. Engineering names stay precise in the API,
and factory language never renames a Rust contract unless the type model
earns that name. Deeper factory identity lives in [01_FACTORY.md](01_FACTORY.md).

## Reading Paths

Pick the door that matches your intent — the docs are one model, but nobody
is required to read all of them:

- **Evaluating?** You have already read enough. Run the quickstart, skim
  the [cookbook](cookbook/README.md), decide.
- **Building on the store?** [02_MODEL.md](02_MODEL.md) →
  [06_EVENTS.md](06_EVENTS.md) → [07_RECEIPTS.md](07_RECEIPTS.md) →
  [09_REPLAY.md](09_REPLAY.md) → [10_PROJECTIONS.md](10_PROJECTIONS.md) →
  [cookbook](cookbook/README.md).
- **Composing batteries or operating a host?** [01_FACTORY.md](01_FACTORY.md) →
  [04_BATTERIES.md](04_BATTERIES.md) → [05_TERMINALS.md](05_TERMINALS.md) →
  [08_CIRCUITS.md](08_CIRCUITS.md) → [11_INTEGRATION.md](11_INTEGRATION.md).
- **Auditing the guarantees?** [03_INVARIANTS.md](03_INVARIANTS.md) →
  [12_CONFORMANCE.md](12_CONFORMANCE.md).

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

Raw `cargo` is an implementation detail unless routed through the explicit
escape hatch:

```sh
just cargo -- <args>
```

## License

Licensed under either of:

- Apache License, Version 2.0
- MIT license

at your option.
