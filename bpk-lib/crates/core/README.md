# batpak

The Free Battery Factory makes batteries for software boundaries. **batpak** is
the core battery: an embedded, sync-first append-only journal with typed
payloads, Blake3 hash-chained ancestry, verifiable receipts, deterministic
replay, and derived projections.

The family around it — `syncbat`, `netbat`, and `hostbat` — wires
the journal into larger Rust hosts through explicit terminals and circuits. See the
[repository README](https://github.com/heyoub/batpak/blob/main/README.md) for
the full family map, scale-out model, and host path.

Use it when you need a tamper-evident, replayable record of what happened:
agent action audit trails, local-first app logs, compliance evidence,
event-sourced application state.

```rust
use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 1)]
struct PlayerMoved {
    x: i32,
    y: i32,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;

    // The Store owns this directory and nothing else.
    let store = Store::open(StoreConfig::new(dir.path()))?;

    // A Coordinate names where events belong: an entity within a scope.
    let coord = Coordinate::new("player:alice", "room:dungeon")?;

    // Append source truth. The receipt is verifiable evidence of what
    // was accepted.
    let receipt = store.append_typed(&coord, &PlayerMoved { x: 10, y: 20 })?;

    // Accepted events are immutable.
    let fetched = store.get(receipt.event_id)?;
    println!("stored {} at sequence {}", fetched.event.header.event_id, receipt.sequence);

    store.close()?;
    Ok(())
}
```

## Why not SQLite with an events table?

SQLite gives you durable rows. batpak gives you durable rows plus proof:
every event is hash-bound to its per-entity ancestor with Blake3, every
accepted write returns a receipt you can verify later, and projections are
derived views rebuilt from the log by construction — read models cannot
silently drift from source truth.

When batpak is the wrong tool: ad-hoc SQL over relational data; many writers
on one mutable `data_dir`; raw write throughput over verifiable history; or
automatic Raft replication inside the core crate. Scale with multiple
journals and explicit host circuits instead — one writer per `data_dir`, by
design.

## Trust

Judge the evidence, not the 0.x version number: roughly one line of tests
per line of source, deterministic concurrency proofs with `loom`,
property-based tests over hash-chain integrity and canonical encoding,
crash-recovery and fault-injection suites, mutation testing on critical
seams, and 73 named invariants enforced by an executable integrity gate.

## Docs

Full documentation lives in the
[repository](https://github.com/heyoub/batpak): the
[README](https://github.com/heyoub/batpak/blob/main/README.md) for the
evaluator path, the
[cookbook](https://github.com/heyoub/batpak/blob/main/cookbook/README.md) for
task-shaped recipes, and
[MODEL](https://github.com/heyoub/batpak/blob/main/02_MODEL.md) /
[INVARIANTS](https://github.com/heyoub/batpak/blob/main/03_INVARIANTS.md) /
[CONFORMANCE](https://github.com/heyoub/batpak/blob/main/12_CONFORMANCE.md)
for the mental model and the guarantees.

```text
bp records.
```
