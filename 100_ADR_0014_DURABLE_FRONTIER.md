# ADR-0014: Durable Frontier Observability

## Status
Accepted (shipped in 0.7.0).

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
The store has a public, narrow, honest frontier surface for operators and for
the durability-gating API accepted in ADR-0016. Those gates build on
`durable_hlc` without renaming placeholder fields. Default read semantics remain
an explicit policy choice: visible state may still exceed durable state when the
configured sync cadence is greater than one.

The frontier tests pin both behavior and limits. In-process panic tests can
prove writer-crash recovery and lifecycle monotonicity, but they cannot prove
true power-loss durability because the host page cache may preserve unsynced
bytes after a process panic. ADR-0015 covers the VM/block-device torn-tail proof
lane; additional chaos scenarios are separate proof work, not a missing frontier
API.

Test-local panic injectors must carry a module-level `clippy::panic` opt-out
with a traceable `justifies:` comment. This keeps panic-as-assertion discipline
visible while avoiding a production panic injection API.

## Errata
Phase 1A closed the lifecycle gap described above by adding
`SYSTEM_CLOSE_COMPLETED` on explicit `Store::close()`. Drop still performs only
best-effort shutdown and does not emit a close lifecycle event. Reopen scans
recovered close lifecycle events, verifies they advance monotonically in log
order, and uses the highest close HLC as `last_close_hlc` for bootstrap.

This is a closure of the Phase 0 durable-frontier decision, not a new
architecture decision. The visible-vs-durable cadence observation remains
unchanged by close-event emission.

## Related ADRs
- [ADR-0002: Single Writer Thread Commit Path](100_ADR_0002_SINGLE_WRITER_THREAD.md)
- [ADR-0006: Writer Restart Policy](100_ADR_0006_RESTART_POLICY.md)
- [ADR-0015: dm-flakey Chaos Harness](100_ADR_0015_CHAOS_HARNESS_DM_FLAKEY.md) -
  proves torn-tail frontier behavior below the process/page-cache boundary.
- [ADR-0016: Durability Gating](100_ADR_0016_DURABILITY_GATING.md) - builds the
  wait and append-gate API on the frontier watermarks.
- [ADR-0017: At-Least-Once Witness Surface](100_ADR_0017_AT_LEAST_ONCE_WITNESS_SURFACE.md) -
  carries delivery witnesses through handler paths that depend on applied
  frontier progress.

## References
- `bpk-lib/traceability/invariants.yaml`: `INV-FRONTIER-MONOTONIC`,
  `INV-FRONTIER-ORDERING`, `INV-FRONTIER-TORN-FREE`,
  `INV-FRONTIER-OPEN-MONOTONIC`, `INV-FRONTIER-APPLIED-MIN`,
  `INV-FRONTIER-FAULT-ORDINALS`, `INV-TEST-PANIC-AS-ASSERTION`
- `bpk-lib/traceability/flows.yaml`: `FLOW-FRONTIER-OBSERVE`,
  `FLOW-FRONTIER-BOOTSTRAP`, `FLOW-FRONTIER-CHAOS-RECOVERY`
- `bpk-lib/traceability/observations.yaml`:
  `OBS-CADENCE-GT-1-VISIBLE-EXCEEDS-DURABLE`
- `bpk-lib/crates/core/tests/durable_frontier_semantics.rs`
- `bpk-lib/crates/core/tests/durable_frontier_chaos.rs`
