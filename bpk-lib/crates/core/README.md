# batpak

The Free Battery Factory makes batteries for software boundaries. **batpak** is
the core battery: an embedded, sync-first append-only journal with typed
payloads, Blake3 hash-chained ancestry, verifiable receipts, deterministic
replay, and derived projections.

The family around it — `syncbat`, `netbat`, and `hostbat` — wires
the journal into larger Rust hosts through explicit terminals and circuits. See the
[repository README](https://github.com/freebatteryfactory/batpak/blob/main/README.md) for
the full family map, scale-out model, and host path.

Use it when you need a tamper-evident, replayable record of what happened:
agent action audit trails, local-first app logs, compliance evidence,
event-sourced application state.

```rust
use batpak::prelude::*;

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 1)]
struct ThingHappened {
    value: i64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;

    // The Store owns this directory and nothing else.
    let store = Store::open(StoreConfig::new(dir.path()))?;

    // A Coordinate names where events belong: an entity within a scope.
    let coord = Coordinate::new("entity:a", "scope:1")?;

    // Append source truth. The receipt is verifiable evidence of what
    // was accepted.
    let receipt = store.append_typed(&coord, &ThingHappened { value: 42 })?;

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

## Verifiability defaults

The safe behavior is the default; the weaker behavior is an explicit opt-out.

- **Open fails closed on an undecodable registry.** `EventPayloadValidation`
  defaults to `FailFast`: a duplicate-kind collision or an incomplete upcast
  chain refuses `Store::open` (relax to `Warn`/`Silent` deliberately).
- **Signing policy.** `SigningPolicy::Optional` (default) permits a keyless
  store; `SigningPolicy::Required` refuses to open without a signing key so an
  unsigned receipt can never be accepted. A configured signer that cannot build
  its cover fails the append closed unless
  `StoreConfig::with_signing_downgrade_allowed(true)`.
- **Tamper checks on demand.** `Store::verify_chain()` recomputes the full
  blake3 chain and returns a `ChainVerificationReport`; opt into
  `ChainVerification::Recompute` to run it at open and fail closed on tamper.
- **Observable truncation.** `Store::walk_ancestors_outcome()` returns an
  `AncestorWalk` whose `AncestryBoundary` distinguishes a complete walk to
  genesis from a lineage truncated at a missing parent (e.g. a retention drop).

## Trust

Judge the evidence, not the 0.x version number: a deep test surface
(integration, property, crash-recovery, cold-start), deterministic
concurrency proofs with `loom`, property-based tests over hash-chain
integrity and canonical encoding, crash-recovery and fault-injection
suites, mutation testing on critical seams, and a catalog of named
invariants enforced by an executable integrity gate. See the root
README and `03_INVARIANTS.md` for the current counts.

## Docs

Full documentation lives in the
[repository](https://github.com/freebatteryfactory/batpak): the
[README](https://github.com/freebatteryfactory/batpak/blob/main/README.md) for the
evaluator path, the
[cookbook](https://github.com/freebatteryfactory/batpak/blob/main/cookbook/README.md) for
task-shaped recipes, and
[MODEL](https://github.com/freebatteryfactory/batpak/blob/main/02_MODEL.md) /
[INVARIANTS](https://github.com/freebatteryfactory/batpak/blob/main/03_INVARIANTS.md) /
[CONFORMANCE](https://github.com/freebatteryfactory/batpak/blob/main/12_CONFORMANCE.md)
for the mental model and the guarantees.

```text
bp records.
```
