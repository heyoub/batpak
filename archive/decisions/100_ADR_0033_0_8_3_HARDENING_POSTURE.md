# ADR-0033: 0.8.3 Hardening Posture

## Status

Accepted; 0.8.3 audit-remediation cut.

## Context

The 0.8.3 cut is an integrity-hardening pass over the durable substrate, not a
feature release. Two of its changes constrain the public surface and the build
profile in ways a future maintainer might be tempted to relax for convenience
or throughput. This ADR records the rationale so neither is loosened without
seeing why it exists.

The companion footer-format change is recorded separately in ADR-0032.

## Decision

### Reserved-Kind Public-Surface Rejection

The public raw-`kind` append surface rejects reserved `EventKind`s. Every
public entry point that takes a caller-supplied `kind` -- `append`,
`append_with_options`, `submit`, `submit_reaction`, the `try_submit*` family,
and the batch paths (`append_batch`, `append_batch_with_options`,
`append_reaction_batch`, `submit_batch`) -- returns `StoreError::ReservedKind`
when `kind` is in the reserved system category (`0x0`) or effect category
(`0xD`), as defined by `EventKind::is_reserved`. On the batch paths the error is
returned directly with `index: Some(i)` for the offending item rather than
wrapped in `BatchFailed`.

Reserved kinds (for example `SYSTEM_BATCH_BEGIN`/`SYSTEM_BATCH_COMMIT`,
`TOMBSTONE`, and the effect family) are emitted only by the substrate. The one
legitimate reserved-kind emitter reachable from the public surface,
`append_denial` (`SYSTEM_DENIAL`), is a substrate-owned audit path and is
unaffected.

Rationale: a caller who could smuggle a reserved marker through the append
surface could forge batch envelope markers and corrupt crash recovery and the
SIDX fast-path rebuild. The reserved-kind namespace is substrate-owned; the
public surface must not be a side door into it.

### overflow-checks: Panic on Wrap

The release profile builds with `overflow-checks = true`. A silent integer wrap
in durable-state arithmetic is treated as a correctness fault: it panics rather
than admitting a wrapped value that could become corrupt durable state. The
bench profile keeps overflow checks off so throughput numbers reflect prior
release behavior. The cold-start `wall_ms` multiply saturates rather than
wrapping.

Rationale: in a durability substrate a silent wrap is worse than a crash. A
panic is recoverable and observable; corrupt durable state is neither.

## Non-Negotiables

- Do not add a public append path that accepts an arbitrary reserved `kind`.
  Substrate-internal emission stays internal.
- Do not turn `overflow-checks` off on the release profile to chase throughput.
  Throughput claims belong to the bench profile.

## Consequences

- Callers that previously appended reserved kinds must move to product kinds
  (category `>= 0x1`); this is a deliberate behavior change.
- Release builds may panic on an integer wrap that earlier builds silently
  accepted; this surfaces latent arithmetic bugs instead of persisting them.

## References

- `100_ADR_0032_SIDX_SDX3_INTEGRITY_FOOTER.md`
- `100_ADR_0026_PRE_1_0_PUBLIC_SURFACE_STRATEGY.md`
- `CHANGELOG.md`
- `06_EVENTS.md`
