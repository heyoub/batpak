# ADR-0006: Writer Restart Policy

## Status
Accepted

## Context
The writer needs bounded recovery from panics without turning failure into silent fake success.

## Decision
The public restart policy remains limited to `Once` and `Bounded`, with restart budgeting enforced centrally by the writer thread.

## Consequences
- Failure remains explicit after the configured budget is exhausted.
- The policy can be verified with fault-injection tests and observability traces.
- More elaborate backoff belongs in callers or supervisors, not in the core store.
