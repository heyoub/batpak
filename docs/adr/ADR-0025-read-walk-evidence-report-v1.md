# ADR-0025: Read Walk Evidence Report v1

## Status
Accepted.

## Context
Read-path facts are available in batpak (`Region` queries, visibility-gated index
selection, frontier snapshots, `IndexEntry` hash references), but callers had to
assemble read evidence manually across multiple APIs.

## Decision
batpak adds an opt-in, non-appending read evidence surface:

- `ReadWalkRequest`
- `Store::query_with_read_walk_evidence`
- `ReadWalkReportBody`
- `ReadWalkEvidenceReport`

This report remains structural and policy-neutral. It captures selected region
refs, read boundary posture, counts, limit drops when known, optional proof refs
from returned entries, and caller `freshness_intent`. v1 always samples current
visible index state; `freshness_intent` is not a stale-cache execution promise.

## v1 Contracts

- opt-in only; no automatic append
- deterministic report body and canonical `body_hash` through the active batpak
  hash backend
- deterministic sorted findings
- explicit `Known`/`NotApplicable` states where applicable
- visibility-bounded upgrade of query hits into backing entries, so proof refs
  describe the same visible snapshot the query selected
- proof refs are `Known` when requested and `NotApplicable` when not requested;
  v1 has no speculative unavailable proof-ref state
- `ReadWalkRequest` is not serializable in v1; serialize the report, not the
  syntactic request selector

## Non-goals

- protocol/application semantics
- authorization/ranking/context policy
- implicit persistence of read observations

## References

- `docs/adr/ADR-0019-canonical-encoding-contract.md`
- `docs/evidence-reports.md`
- `src/store/read_walk.rs`
