[![crates.io](https://img.shields.io/crates/v/batpak.svg)](https://crates.io/crates/batpak)
[![docs.rs](https://docs.rs/batpak/badge.svg)](https://docs.rs/batpak)
[![CI](https://github.com/heyoub/batpak/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/heyoub/batpak/actions/workflows/ci.yml)
[![license](https://img.shields.io/crates/l/batpak.svg)](#license)

# batpak

batpak is a sync-first, embedded event substrate for recording, linking, replaying, verifying, and projecting local application truth.

It is built around four ideas:

1. Coordinates name where events belong.
2. Events record what happened.
3. Receipts prove what was accepted, denied, replayed, verified, or projected.
4. Projections are derived views, not source truth.

batpak is not a database server, queue, ORM, workflow engine, async runtime, network framework, or agent framework.

The deeper project identity is Free Battery Factory:

> The Free Battery Factory makes batteries for software boundaries.
> A battery does not own the machine. It powers one boundary.

## Install

```sh
cargo add batpak
```

## First Shape

Use `batpak` when you want an embedded append log with typed payloads, causal metadata, caller-defined gates, receipts, replay, and projections in one Rust process.

Callers own process model, disk placement, runtime integration, network boundaries, and application authority.

The beginner Rust path is intentionally small:

1. open a `Store`
2. append typed events
3. page commit order with `query_entries_after`
4. fetch payloads with `get`
5. walk hash-chain ancestry with `walk_ancestors`
6. verify receipts with typed outcomes
7. derive projections
8. close the store

See `bpk-lib/crates/core/examples/eight_jobs.rs` for the contract example.

## Reading Order

Read the current substrate contract in this order:

1. [FACTORY.md](FACTORY.md)
2. [MODEL.md](MODEL.md)
3. [INVARIANTS.md](INVARIANTS.md)
4. [BATTERIES.md](BATTERIES.md)
5. [TERMINALS.md](TERMINALS.md)
6. [EVENTS.md](EVENTS.md)
7. [RECEIPTS.md](RECEIPTS.md)
8. [CIRCUITS.md](CIRCUITS.md)
9. [REPLAY.md](REPLAY.md)
10. [PROJECTIONS.md](PROJECTIONS.md)
11. [INTEGRATION.md](INTEGRATION.md)
12. [CONFORMANCE.md](CONFORMANCE.md)
13. [COOKBOOK.md](COOKBOOK.md)

Machine law lives in `bpk-lib/traceability/` and `bpk-lib/tools/integrity/`. These root docs describe the current system; they are not a decision archive.

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

`just` is the command counter. `xtask` is the factory machinery. `ast-grep` inspects structural doctrine. Tests inspect behavior. Receipts preserve evidence. `perf-gates` and `loom` are manual release-confidence proofs, not part of `just verify`.

Raw `cargo`, `npm`, and `pnpm` are implementation details unless routed through an explicit escape hatch:

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

Factory words explain the model. They do not rename the Rust API unless the type model earns that name.

## License

Licensed under either of:

- Apache License, Version 2.0
- MIT license

at your option.
