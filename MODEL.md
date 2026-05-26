# Model

batpak is a sync-first embedded event substrate for recording, linking, replaying, verifying, and projecting local application truth.

The model:

```txt
Application intent
  -> enters through a terminal
  -> appends an event at a coordinate
  -> canonicalizes payload bytes
  -> links/hash-binds the event
  -> emits a receipt
  -> updates replayable source truth
  -> derives projections and gauges
```

## Objects

```txt
Factory
  produces Batteries

Battery
  owns a Boundary

Boundary
  exposes Terminals

Terminal
  admits Operations

Operation
  changes or reads State

Durable Operation
  emits Receipt

Source Truth
  is stored as Events

Events
  are grouped by Coordinate

Replay
  rebuilds State from Events

Projection
  is a Gauge over replayed State

Circuit
  connects Terminals without hiding ownership
```

One paragraph version:

> A battery owns a boundary. Terminals expose what may cross that boundary. Operations enter through terminals. Durable operations emit receipts. Events are the cells of source truth. Replay discharges those cells into projections. Projections are gauges, not truth.

## Beginner Store Path

The 0.8-facing Rust curriculum is one path through the substrate:

```txt
Store::open
  -> append_typed
  -> query_entries_after
  -> get
  -> walk_ancestors
  -> verify_append_receipt_detailed
  -> project / project_if_changed
  -> close
```

Advanced batteries such as delivery cursors, subscriptions, reactors, evidence
reports, schema snapshots, artifact envelopes, pipelines, outbox writes, and
visibility fences remain public where they are real. They are not the default
prelude story.

## Engineering Names

The Rust API keeps engineering names where precision matters:

- `Store`
- `Coordinate`
- `Event`
- `Receipt`
- `Projection`
- `Replay`
- `Gate`
- `Capability`

Factory language explains the shape. It does not rename precise contracts unless the type model earns that name.

## Source Of Law

This file is narrative ontology. Machine law lives in `bpk-lib/traceability/` and the integrity checks under `bpk-lib/tools/integrity/`.
