# ADR-0008: Restore Planner and Projection Trait Evolution

## Status
Accepted

## Context
Cold-start restore at scale showed superlinear scaling due to per-entry
ordered-map insertion. Projection replay paid for full serde_json::Value
tree construction even when projections only needed raw payload bytes.
These were independent problems sharing one root cause: subsystems
rediscovering partition boundaries and decode shapes independently
instead of sharing one substrate.

## Decision

### Restore Planner
All cold-start sources (mmap v2, checkpoint v3, parallel SIDX rebuild,
frame-scan fallback) normalize through one internal `RestorePlanner`
that produces entity-partitioned `RoutingSummary` runs. Runtime views
(streams, SoA, SoAoS, AoSoA, by-id, latest) are materialized from
those runs in bulk rather than from per-entry insertion. Artifact
formats carry additive routing summaries; older formats fall back to
on-load summary synthesis.

### Projection Trait
`EventSourced<P>` replaced with `EventSourced` using an associated
`Input: ProjectionInput` type. Two built-in input modes:
- `JsonValueInput`: payload decoded to `serde_json::Value` (default)
- `RawMsgpackInput`: payload stays as raw `Vec<u8>` bytes

The `ProjectionInput` trait is sealed — no external implementations.
Replay lane selection is automatic based on the associated type.

### Routing Summary
`RoutingSummary` is the shared internal contract: chunk boundaries,
entity run boundaries, counts, and sequence ranges. Consumed by
restore, projection replay planning, and view materialization.
Designed as `#[derive(Clone, Debug, Serialize, Deserialize)]` to keep the
option open for process-boundary portability.

## Consequences
- Single-entity 1M-event restore no longer hits O(n log n) BTreeMap
  insertion pathology
- Projections that opt into `RawMsgpackInput` skip the JSON value tree
  allocation on cold replay
- Artifact format versions are additive: v1 mmap / v2 checkpoint remain
  readable via fallback decoders
- All views consume from the same entity-partitioned substrate; no
  subsystem rediscovers partition boundaries independently
