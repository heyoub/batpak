# ADR-0003: Projection Cache Safety and Capability Signaling

## Status
Accepted (motivating backend removed in 0.3.0; capability API still in force)

## Context
Projection cache backends have different capabilities and safety assumptions. The capability API (`CacheCapabilities`, `prefetch()`) avoids silent no-op behavior across backends. Originally motivated by LMDB environment-handling hazards (LMDB was removed in 0.3.0), the same principles guide the current `NativeCache` design — atomic filesystem operations via tempfile + rename, no unsafe blocks, no environment-wide global state.

## Decision
Cache behavior is surfaced through an explicit capability API, and backend-specific unsafe code is isolated behind small documented helpers.

## Consequences
- Silent no-op cache behavior is no longer implicit.
- Tests can assert prefetch support intentionally.
- Cache backends are explicit about what they support; agents and tools can query capabilities instead of assuming.
- The current `NativeCache` requires no unsafe code; the surface that would have needed unsafe review is gone.
- Future backends (if added) inherit the capability API and the no-implicit-features rule.
