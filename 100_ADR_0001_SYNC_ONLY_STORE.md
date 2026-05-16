# ADR-0001: Sync-Only Store API

## Status
Accepted

## Context
`batpak` is designed as a runtime-agnostic event store. The public API must not force a specific async runtime.

## Decision
The `Store` API remains synchronous. Async callers adapt at the edge through threads, channels, or runtime-specific blocking wrappers.

## Consequences
- Production dependencies stay free of Tokio.
- Test and CI tooling can verify the invariant mechanically.
- Concurrency proofs focus on the writer thread and channel boundaries instead of async schedulers in the public surface.
