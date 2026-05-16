# ADR-0009: Position Hints and Artifact Upgrade Contract

## Status
Accepted

## Context

`DagPosition` already carried branch coordinates (`lane`, `depth`) alongside the
writer-owned HLC and per-entity sequence fields, but the public append surface
did not let callers express non-root branch placement. Cold-start artifacts
also needed to preserve those branch coordinates consistently across live
commit, mmap restore, checkpoint restore, SIDX-backed reconstruction, and
full frame-scan rebuild.

That left two questions that needed to be answered explicitly:

1. which parts of `DagPosition` are caller-owned versus writer-owned
2. what upgrade compatibility guarantees apply when the cold-start artifacts
   evolve to preserve that information

## Decision

### Public Append Contract

The public append surface accepts only an `AppendPositionHint { lane, depth }`.
Callers may choose the DAG branch coordinates, but they do **not** supply:

- `wall_ms`
- `counter`
- `sequence`

Those fields remain writer-owned so the commit path keeps one authoritative
clock, one per-entity sequence allocator, and one visibility/publication model.

### Persistence Contract

Position hints are persistence-affecting, not decorative metadata.
Non-root `lane` and `depth` must survive:

- live append
- mmap reopen
- checkpoint reopen
- SIDX-backed reconstruction
- full rebuild from segment scan

### Artifact Upgrade Contract

Cold-start artifacts may evolve additively when needed to preserve runtime
truth, but the compatibility posture is explicit:

- current readers support forward upgrade of known older artifacts
- old optimization artifacts may be ignored and rebuilt or fallback-scanned
- mixed-version operation does **not** mean two binaries may write the same
  mutable store directory concurrently
- downgrade is not assumed safe just because forward-read compatibility exists;
  rollback means reverting binaries and purging/rebuilding cold-start artifacts
  unless a specific downgrade path is proven

## Consequences

- append callers gain a narrow, honest branch-placement hook without taking
  ownership of commit-time clocks or sequence allocation
- reopen paths remain consistent: the same committed event position survives
  regardless of which cold-start artifact wins
- operator procedure must document artifact purge/rebuild expectations during
  rollback or mixed-fleet transitions
