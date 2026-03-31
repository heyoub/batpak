# ADR-0003: Projection Cache Safety and Capability Signaling

## Status
Accepted

## Context
Projection cache backends have different capabilities and safety assumptions, especially around LMDB environment handling.

## Decision
Cache behavior is surfaced through an explicit capability API, and backend-specific unsafe code is isolated behind small documented helpers.

## Consequences
- Silent no-op cache behavior is no longer implicit.
- Tests can assert prefetch support intentionally.
- Unsafe LMDB assumptions are reviewed at a smaller surface.
