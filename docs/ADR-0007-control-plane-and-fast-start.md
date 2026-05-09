# ADR-0007: Unified Control Plane And Fast-Start Restore

## Status
Accepted

## Context
The store needed a richer control surface around the existing synchronous API without violating the sync-only design. At the same time, cold start and query routing needed to stop pretending there was one globally-correct in-memory layout.

## Decision
The crate keeps the blocking `append` / `append_batch` API as the default surface, but implements it on top of additive control-plane primitives:

- `submit*` plus tickets for non-blocking enqueue with a separate blocking wait on the ticket
- `try_submit*` plus `Outcome` for soft-pressure and cancellation signaling
- producer-side `Outbox`
- public `VisibilityFence`
- `WriterPressure`
- `Store<ReadOnly>`

The index uses a multi-view overlay architecture instead of a single chosen scan layout. Base AoS maps remain mandatory, while SoA / SoAoS / AoSoA overlays can coexist and queries route by shape.

Cold start prefers a verified mmap snapshot (`index.fbati`), then checkpoint restore, then full replay. Cancelled visibility-fence ranges are persisted separately so "durable but hidden" survives reopen and snapshot export.

## Consequences

- The sync-only store contract remains intact for callers who do not need the richer control plane.
- Callers can pipeline work and react to pressure without introducing Tokio or futures into production.
- Visibility remains governed by one watermark; all active index views must be populated before publish.
- Fast start becomes an optimization, not a different truth source, because every path replays into the same canonical index and restores the same hidden-range metadata.
- Lossy `scan()` remains explicitly lossy; guaranteed folds belong on the cursor-worker path.
