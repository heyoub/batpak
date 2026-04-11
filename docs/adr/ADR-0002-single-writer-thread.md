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

## Amendment (2026-04): Group Commit
Group commit (`batch.group_commit_max_batch > 1`) batches multiple appends before a single fsync within the single writer thread. This is a loop optimization, not a concurrency relaxation — all events in the batch are still serialized through the writer. When batch > 1, all appends must include an idempotency key for crash safety (`StoreError::IdempotencyRequired` enforced at append time). Batch = 0 means unbounded drain (drain all pending before syncing).
