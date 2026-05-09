# ADR-0024: Projection Run Evidence Report v1

## Status
Accepted.

## Context
ADR-0023 defined a design-only shape for projection-run evidence and identified
a small proof-object gap: projection run facts were available across existing
projection/frontier/cache surfaces, but not bound into one deterministic report.

## Decision
batpak adds `Store::project_run_evidence` and `crates/core/src/store/projection_run.rs`.
v1 reuses existing projection machinery (`project` flow, freshness policy,
frontier view, projection id derivation) and reports known/not-applicable/
unavailable states explicitly.

### 1) v1 report body

`ProjectionRunReportBody` includes:

- `schema_version`
- `projection_id`
- `source_refs`
- `replay_mode`
- `requested_freshness`
- `observed_freshness`
- `input_frontier`
- `output_hash`
- `cache_status`
- `checkpoint_ref`
- deterministic sorted `findings`

### 2) honesty contract

v1 reports strongest truthful state from existing substrate facts:

- `Known` when available
- `NotApplicable` when semantics do not apply to this run path
- `Unavailable` with deterministic reason when acquisition failed
- `Unknown` only when genuinely indeterminate

### 3) deterministic identity

`ProjectionRunEvidenceReport` follows ADR-0019 family identity:

- deterministic body
- canonical `body_hash` through the active batpak hash backend
- metadata outside deterministic identity

## Non-goals

This slice does not add:

- projection registry architecture changes
- protocol/application projection semantics
- scheduler/workflow/migration machinery
- artifact envelope semantics

## Consequences

- projection evidence is no longer scattered across run outputs, frontier calls,
  and internal cache branches
- consumers can persist one deterministic projection-run proof object
- uncertainty is explicit rather than implied
- empty/no-input projections report freshness as `NotApplicable`, not
  `Unknown`
- `input_frontier` records the replay/cache watermark selected from the visible
  index; durable and process-wide applied watermarks remain `FrontierView`
  diagnostics, not projection input-boundary facts
- v1 marks checkpoint references as `NotApplicable`; it does not synthesize a
  checkpoint fact where the projection path does not own one

## References

- `docs/adr/ADR-0019-canonical-encoding-contract.md`
- `docs/adr/ADR-0023-projection-run-evidence-report-design.md`
- `docs/evidence-reports.md`
- `crates/core/src/store/projection_run.rs`
