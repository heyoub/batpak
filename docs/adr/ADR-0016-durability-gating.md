# ADR-0016: Durability Gating

## Status
Accepted (shipped in 0.7.0).

## Context
ADR-0014 exposed the durable frontier as an observation surface. Phase 2 turns
the same watermarks into a caller-facing control surface: code can wait until a
specific HLC is covered by durability without introducing an async runtime or a
new receipt contract.

The public store API is sync-only per ADR-0001. Waits therefore need ordinary
thread blocking, a mandatory timeout, and a clear failure mode when the writer
thread panics while callers are waiting.

## Decision
`Store::wait_for_durable(point, timeout)`,
`Store::wait_for_applied(point, timeout)`, and
`Store::wait_for_visible(point, timeout)` block the calling thread until the
chosen watermark observes its current HLC `>= point`, the timeout expires, or
the writer crash poison flag is set.

The implementation uses:

- one `parking_lot::Condvar` paired with the existing watermark mutex;
- mandatory `Duration` timeouts, never optional infinite waits;
- wake-all notification on watermark guard release;
- an `AtomicBool` writer-crash poison flag that wakes all waiters and returns
  `StoreError::WriterCrashed`;
- `StoreError::WaitTimeout { watermark, target, waited_ms }` for expired waits.

Wake-all is the first implementation. Precise wait lists are deferred until
benchmarks or production use show the extra bookkeeping is worth it.

## Alternatives Considered
- `tokio::sync::Notify`: rejected because the store remains sync-only and does
  not take an async runtime dependency.
- `std::sync::Condvar`: rejected because poisoned mutex semantics would add
  unwrap-heavy boilerplate around an observation path that already uses
  `parking_lot`.
- Precise per-watermark waiter lists: deferred. Wake-all is simpler and correct
  while the wait surface is small. The `frontier_waiters` benchmark records
  waiter wake completion and writer-side wake cost at 1, 8, 32, 128, and 512
  concurrent waiters. Precise lists promote only if stable-hardware results
  show writer-side wake cost dominating append/sync latency or an
  order-of-magnitude wake-completion jump between adjacent waiter-count tiers.

## Consequences
Callers must choose a timeout. Very long waits remain possible, but the API
makes that choice explicit at every call site.

The poison flag is one-way in this phase. A writer panic surfaces to existing
and future durable waiters as `StoreError::WriterCrashed`; Phase 2.1 does not
attempt to unpoison waiters after writer restart.

Spurious wakeups are part of the contract: every wake rechecks poison and the
target watermark before returning success.

### Append-time inline gating

`AppendOptions::gate` lets callers opt into the same wait contract as part of a
blocking append call. `DurabilityGate { kind, timeout }` is optional and
defaults to `None`, so existing append behavior remains no-gate.

For single-event appends, the writer commits and publishes the event first, then
the append path waits for the selected watermark to cross the committed event's
HLC. For batch appends, `Store::append_batch_with_options(items, opts)` applies
the batch-level gate to the last event in the batch; monotonic watermarks and
atomic batch commit make that cover all earlier batch items. Per-item gates
embedded in `BatchAppendItem` are ignored so batches do not serialize into
per-item waits.

`StoreError::WaitTimeout` from an append gate is not a rollback signal. It means
the append committed, but the requested watermark guarantee was not observed
within the timeout. The event remains queryable; callers that still need the
guarantee can call the corresponding `wait_for_*` method with a longer timeout.

## References
- [ADR-0001: Sync-Only Store API](ADR-0001-sync-only-store.md)
- [ADR-0014: Durable Frontier Observability](ADR-0014-durable-frontier.md)
- `traceability/invariants.yaml`: `INV-FRONTIER-DURABLE-COVERS-RECOVERED`,
  `INV-FRONTIER-OPEN-MONOTONIC`, `INV-FRONTIER-WAIT-MONOTONIC`
- `traceability/flows.yaml`: `FLOW-FRONTIER-WAIT-DURABLE`,
  `FLOW-FRONTIER-WAIT-APPLIED`, `FLOW-FRONTIER-WAIT-VISIBLE`
- `crates/core/tests/durable_frontier_waits.rs`
