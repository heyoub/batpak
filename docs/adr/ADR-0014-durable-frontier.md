# ADR-0014: Durable Frontier Observability

## Status
Accepted

## Context
Phase 0 needed an honest way to describe where the store is in the commit
pipeline without changing read semantics, default sync configuration, receipt
types, segment format, or fanout guarantees.

One scalar was not enough. A write becomes ordered, written to the active
segment, synced, query-visible, projection-applied, and broadcast-attempted at
different moments. Collapsing those moments into a single "stable" value would
either hide today's visible-before-durable gap or publish placeholder names with
semantics the store does not yet provide.

The frontier also needed to survive restart observation. Mutable open already
emits a `SYSTEM_OPEN_COMPLETED` lifecycle event, and read-only open must remain
side-effect free. Both paths need a bootstrap HLC that is monotonic relative to
recovered data and any close lifecycle point.

## Decision
The store records six internal frontier watermarks:

- `accepted_hlc`: the highest HLC whose ordering coordinate has been assigned.
- `written_hlc`: the highest HLC whose frame write returned successfully.
- `durable_hlc`: the highest HLC covered by a successful sync.
- `visible_hlc`: the highest HLC visible to query readers.
- `emitted_hlc`: the highest HLC for which broadcast artifacts were attempted.
- `applied_hlc`: the minimum consumed HLC across registered projections.

Each watermark advances monotonically. Advance helpers take `max(current,
proposed)` and never permit backward motion. Frontier ordering is asserted at
torn-free observation time:

- `accepted_hlc >= written_hlc >= durable_hlc`
- `accepted_hlc >= visible_hlc >= applied_hlc`
- `emitted_hlc >= visible_hlc`

`HlcPoint` orders lexicographically by `(wall_ms, global_sequence)`. Tests and
frontier logic compare full HLC points rather than raw sequence numbers.

`Store::frontier()` returns an operator-facing `FrontierView`. It is composed
under one `parking_lot::Mutex<WatermarkState>` acquisition. `diagnostics()`
uses the same composition path. The visible/emitted commit advance uses one
composite helper so external observers cannot see `emitted_hlc` below
`visible_hlc`.

Mutable `Store::open` computes a bootstrap candidate from recovered index state,
the last close point when available, and a wall-time floor. It emits
`SYSTEM_OPEN_COMPLETED`, validates the emitted HLC against the recovered
candidate, and resets all six watermarks to the open HLC. Read-only open emits
no lifecycle event, but bootstraps from recovered index state using the same
monotonicity rule. Violations surface as `StoreError::InvariantViolation`.

Projection application is tracked through a projection registry. Each
registered projection records its latest consumed HLC; `applied_hlc` is the
minimum across registered projections. With zero registered projections,
`applied_hlc` remains at the bootstrap open HLC and does not drift.

The watermark state uses `parking_lot::Mutex`. Panic-path chaos tests need to
observe and reopen after writer-thread panics without poisoning the observation
mutex itself.

Fault-injection ordinals are part of the Phase 0 contract:

- `SingleAppendStart` fires before any watermark advance.
- `SingleAppendWritten` fires after written advances and before durability.
- `SingleAppendPublished` fires after visible/emitted advances and before the
  receipt returns.

Panic behavior is tested with test-local `FaultInjector` implementations. The
production fault API does not gain a `Panic` action.

## Consequences
The store now has a public, narrow, honest frontier surface for operators and
for later phases. Phase 1 can build real durability gates on top of
`durable_hlc` without renaming placeholder fields. Phase 2 can tighten default
read semantics with a measured baseline for today's visible/durable gap.

The frontier tests pin both behavior and limits. In-process panic tests can
prove writer-crash recovery and lifecycle monotonicity, but they cannot prove
true power-loss durability because the host page cache may preserve unsynced
bytes after a process panic. A VM-level or block-device torn-tail harness is
deferred.

Test-local panic injectors must carry a module-level `clippy::panic` opt-out
with a traceable `justifies:` comment. This keeps panic-as-assertion discipline
visible while avoiding a production panic injection API.

## Related ADRs
- [ADR-0002: Single Writer Thread Commit Path](ADR-0002-single-writer-thread.md)
- [ADR-0006: Writer Restart Policy](ADR-0006-restart-policy.md)

## References
- `traceability/invariants.yaml`: `INV-FRONTIER-MONOTONIC`,
  `INV-FRONTIER-ORDERING`, `INV-FRONTIER-TORN-FREE`,
  `INV-FRONTIER-OPEN-MONOTONIC`, `INV-FRONTIER-APPLIED-MIN`,
  `INV-FRONTIER-FAULT-ORDINALS`, `INV-TEST-PANIC-AS-ASSERTION`
- `traceability/flows.yaml`: `FLOW-FRONTIER-OBSERVE`,
  `FLOW-FRONTIER-BOOTSTRAP`, `FLOW-FRONTIER-CHAOS-RECOVERY`
- `traceability/observations.yaml`:
  `OBS-CADENCE-GT-1-VISIBLE-EXCEEDS-DURABLE`
- `tests/durable_frontier_semantics.rs`
- `tests/durable_frontier_chaos.rs`
