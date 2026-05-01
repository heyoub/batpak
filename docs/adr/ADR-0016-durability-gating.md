# ADR-0016: Durability Gating

## Status
Accepted

## Context
ADR-0014 exposed the durable frontier as an observation surface. Phase 2 turns
the same watermarks into a caller-facing control surface: code can wait until a
specific HLC is covered by durability without introducing an async runtime or a
new receipt contract.

The public store API is sync-only per ADR-0001. Waits therefore need ordinary
thread blocking, a mandatory timeout, and a clear failure mode when the writer
thread panics while callers are waiting.

## Decision
`Store::wait_for_durable(point, timeout)` blocks the calling thread until the
durable watermark observes `durable_hlc >= point`, the timeout expires, or the
writer crash poison flag is set.

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
  while the wait surface is small.

## Consequences
Callers must choose a timeout. Very long waits remain possible, but the API
makes that choice explicit at every call site.

The poison flag is one-way in this phase. A writer panic surfaces to existing
and future durable waiters as `StoreError::WriterCrashed`; Phase 2.1 does not
attempt to unpoison waiters after writer restart.

Spurious wakeups are part of the contract: every wake rechecks poison and the
target watermark before returning success.

## References
- [ADR-0001: Sync-Only Store API](ADR-0001-sync-only-store.md)
- [ADR-0014: Durable Frontier Observability](ADR-0014-durable-frontier.md)
- `traceability/invariants.yaml`: `INV-FRONTIER-DURABLE-COVERS-RECOVERED`,
  `INV-FRONTIER-OPEN-MONOTONIC`, `INV-FRONTIER-WAIT-MONOTONIC`
- `traceability/flows.yaml`: `FLOW-FRONTIER-WAIT-DURABLE`
- `tests/durable_frontier_waits.rs`
