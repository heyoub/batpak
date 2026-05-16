# ADR-0004: Compaction and Concurrent Appends

## Status
Accepted

## Context
Compaction rewrites historical segments while appends continue through the active writer path.

## Decision
Compaction is allowed while the store is live, but it must synchronize before rebuild phases, rebuild index state from disk, and prove observational equivalence through replay tests.

## Consequences
- Compaction correctness is validated by replay and snapshot tests.
- The implementation must expose enough telemetry to audit rebuild phases.
- Comment-only concurrency claims are not sufficient; tests and detectors must back them.
