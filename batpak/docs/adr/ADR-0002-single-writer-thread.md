# ADR-0002: Single Writer Thread Commit Path

## Status
Accepted

## Context
Append ordering, CAS enforcement, and idempotency all need a single coordination point to avoid local races and fake success.

## Decision
All writes pass through one background writer thread that owns segment mutation and notification fan-out.

## Consequences
- Ordering and guard checks are centralized.
- Restart policy and writer observability become critical proving surfaces.
- High-contention correctness is verified through integration, chaos, and deterministic scheduler tests.
