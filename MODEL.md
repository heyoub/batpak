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

## Family Stack

| Layer | Surface | Role |
| --- | --- | --- |
| Journal | `batpak::Store` | Source truth, HLC frontier, receipts |
| Runtime | `syncbat` | Handler dispatch, runtime receipts |
| Network | `netbat` | NETBAT/1 framing |
| Reference host | `hbat` | Ten-op manifest |
| TS clients | `@batpak/sdk` | Wire client, canonical codec, generated types |

The in-process path opens `Store` directly. The networked path crosses
terminals documented in [TERMINALS.md](TERMINALS.md). Journal and multi-journal
composition rules live in [README.md](README.md) and [CIRCUITS.md](CIRCUITS.md).

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

## Lane Frontier Model

The store has one physical commit order (`global_sequence`) and one segment-file
durability axis. DAG lanes are logical branch labels inside that order, not
separate logs. Therefore frontier tracking is split:

- the physical durable point is global, because one fsync covers interleaved
  bytes across lanes;
- accepted, written, durable, visible, applied, and emitted are also exposed per
  lane as logical watermarks;
- a lane's logical durable watermark is capped by the global physical durable
  point on the shared `global_sequence` axis, not by wall-clock HLC ordering;
- lane visibility is a lane-scoped publish cursor over the same global sequence
  axis, not a stream session or branch manager.
- cancelled visibility ranges are persisted as global ranges plus per-lane
  ranges over that same global sequence axis, so bootstrap can keep hidden
  durable entries out of visible/applied lane progress.

The legacy global frontier remains a max view for compatibility. Lane-aware
callers use lane-scoped reads and waits when they need branch-local progress.

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
