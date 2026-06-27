# Terminals

A terminal is a named boundary where a battery accepts, denies, emits, or observes work.

Terminals are authority boundaries, not just function names. If work crosses from a host into a battery, or from one battery into another, the route must be visible.

## Terminal Rules

- Terminals name what may cross the boundary.
- Terminals apply policy before durable state changes.
- Terminals emit receipts or typed outcomes for durable operations.
- Terminals do not hide runtime ownership.
- Hidden wires are bugs.

## Today In batpak

Current terminal-shaped surfaces include:

- `Store` public methods
- append and batch append paths
- gate evaluation and denial paths
- cursor and subscription delivery surfaces
- projection and replay entry points
- netbat routes and operation handling surfaces

The reference NETBAT profile exposes ten operations: five core substrate
terminals plus one liveness terminal and four domain-neutral `evidence.*`
terminals.

- `system.heartbeat` is the liveness terminal.
- `bank.commit` is the write terminal.
- `event.get` is the point-read terminal.
- `event.query` is the commit-order paging terminal.
- `receipt.verify` is the proof terminal for ack-shaped append receipts.
- `event.walk` is the relation-walk terminal for bounded hash-chain ancestry.
- `evidence.chain_walk` is the chain-walk evidence report terminal.
- `evidence.store_resource` is the store-resource snapshot evidence terminal.
- `evidence.read_walk` is the read-walk evidence report terminal.
- `evidence.projection_run` is the projection-run evidence terminal. The
  reference `refbat` host registers no projections, so this op is advertised on
  the wire but answers with an unknown-projection handler error unless an
  embedder registers projections.

Push subscription is lossy awareness, not replay. Durable replay uses
commit-order query pages, durable cursor pull surfaces, or projection-owned
pull surfaces.

Run `cargo test -p netbat` to prove the ten reference NETBAT operations through
the Rust wire and stream runtime surfaces.

## Terminal Versus Function

A function is an implementation unit. A terminal is a boundary promise.

Many functions are not terminals. A terminal is where authority, input, evidence, or durable state crosses a meaningful boundary.

## Breakers

Gates and policy checks act like breakers. They do not make work disappear; they accept, deny, or classify it with evidence.
