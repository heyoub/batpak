# ADR-0023: Projection Run Evidence Report (Design Precursor)

## Status
Accepted as design precursor; ADR-0024 records the v1 implementation.

## Context
batpak projection machinery is mature (`project`, `project_if_changed`,
`ProjectionWatcher`, `ProjectionCache`, replay lanes, freshness policy, frontier
waits), but the evidence is fragmented across multiple APIs and internal
execution branches.

The evidence-report family established by ADR-0019 through ADR-0022 requires a
single deterministic report-body shape when a consumer needs one proof object
for a projection run.

This slice defined the shape only. ADR-0024 subsequently implemented the v1
report surface.

## Decision
Define a smallest generic design for `ProjectionRunReportBody` as a structural
evidence report for one projection run.

### 1) Report subject

Report subject is a single projection run, identified by:

- `projection_id` (stable id string)
- `source_refs` (entity/region/range refs supported by the run)
- `replay_mode` (run boundary mode)

No protocol/application semantics are attached.

### 2) Input boundary and frontier terms

Input boundary uses existing batpak frontier vocabulary:

- `input_frontier` (sequence/HLC boundary observed by the run)
- optional `as_of_frontier` only when a run mode supports it

Names must reuse existing terms (`visible`, `durable`, `applied`) and avoid
new frontier vocabulary.

### 3) Freshness model

Report captures:

- `requested_freshness` (existing `Freshness`)
- `observed_freshness` (structural observation only)

No policy decision is made by this report.

### 4) Output identity

Output identity is structural:

- `output_hash` when output bytes are available for canonical hashing
- otherwise explicit unknown/unavailable via field state and findings

No semantic meaning is inferred from output hash.

### 5) Cache and checkpoint

Cache/checkpoint reporting is observational:

- `cache_status` in `Hit | Miss | Bypassed | Unavailable`
- `checkpoint_ref` is `NotApplicable` for v1; no checkpoint system is invented
  by the projection report

No checkpoint system is invented in this design.

### 6) Findings

Findings are deterministic and sorted. Initial structural finding set:

- `StaleUsed`
- `CacheStatusUnavailable`
- `OutputHashUnavailable`
- `ProjectionFailed`
- `PartialVisibilityNotApplicable`

### 7) Proposed body shape (design target)

```text
ProjectionRunReportBody {
  schema_version,
  projection_id,
  source_refs,
  replay_mode,
  requested_freshness,
  observed_freshness,
  input_frontier,
  output_hash,
  cache_status,
  checkpoint_ref,
  findings,
}
```

Proof object class remains `EvidenceReport`, not `Receipt`.

## Field population contract for v1

Each field must explicitly document whether v1 can populate, return
`NotApplicable`, return `Unavailable`, return `Unknown`, or omit.

| Field | v1 expectation |
| --- | --- |
| `schema_version` | Populate |
| `projection_id` | Populate (required caller-supplied or deterministic descriptor) |
| `source_refs` | Populate for the v1 entity + relevant-kind source shape |
| `replay_mode` | Populate (`Current` in v1 baseline) |
| `requested_freshness` | Populate |
| `observed_freshness` | Populate when derivable; otherwise `Unavailable` + finding |
| `input_frontier` | Populate best available run boundary from existing projection/frontier machinery |
| `output_hash` | Populate when output bytes are available; `NotApplicable` for known empty output; unavailable with reason on encoding failure |
| `cache_status` | Populate `Hit/Miss/Bypassed` where derivable; else `Unavailable` + finding |
| `checkpoint_ref` | `NotApplicable` in v1; no checkpoint system is invented by this report |
| `findings` | Populate deterministically; sorted |

## Explicit non-goals

This design excludes:

- protocol-specific projection meaning
- external artifact format semantics
- application policy/authority decisions
- migration semantics
- scheduler/workflow orchestration
- projection registry architecture work

## Consequences

- The projection evidence gap is now specified without overclaiming.
- Unknown states, where a later report version truly needs them, are explicit
  and reserved for genuinely indeterminate facts; v1 uses `Unavailable` when a
  deterministic acquisition failure is known.
- A subsequent implementation slice can stay small and evidence-only.

## References

- `docs/ADR-0019-canonical-encoding-contract.md`
- `docs/ADR-0020-schema-snapshot-drift-report.md`
- `docs/ADR-0021-chain-walk-evidence-report.md`
- `docs/ADR-0022-subscriber-frontier-observations.md`
- `docs/evidence-reports.md`
