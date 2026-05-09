# ADR-0021: Chain Walk Evidence Report

## Status
Accepted.

## Context
`Store::walk_ancestors` already provides bounded chain traversal, but it returns
a prefix vector and can stop on corruption/cycle conditions without a
deterministic, machine-readable finding set.

After ADR-0019 and ADR-0020 established deterministic report-body identity for
evidence surfaces, the next low-risk gap is structural chain continuity
evidence over stored event material.

## Decision
batpak adds `Store::chain_walk_evidence` plus `store::chain_walk` report types
for deterministic structural chain-walk findings.

### 1) Scope and mode

v1 exposes one mode:

- `ChainWalkMode::Linear`

The report validates stored chain continuity only. It does not infer
application-level causation/correlation meaning, policy semantics, or protocol
semantics.

### 2) Deterministic body shape

`ChainWalkReportBody` carries:

- `schema_version`
- `mode`
- `checked_count`
- `first_ref`
- `last_ref`
- `walk_digest`
- deterministic `findings`

`walk_digest` is a deterministic digest over checked `(event_id, event_hash)`
pairs. It is a walk summary, not a transparency/Merkle root claim.

`ChainWalkEvidenceReport` wraps the body with metadata outside deterministic body
identity (`generated_at_unix_ms`, `batpak_version`, `diagnostics`) following the
ADR-0019 pattern.

### 3) Findings posture

v1 findings are structural only:

- missing start
- start hash mismatch (receipt hash vs stored hash)
- entry hash mismatch (stored content vs chain hash)
- missing parent link
- ordering regression
- cycle detected
- stopped early read failure
- truncated by limit
- end not reached

Findings are deterministic and sorted in structural order for a given
input/store state.

### 4) Non-goals in this slice

This decision does not:

- fuse signed receipt verification into chain walking
- add artifact envelopes or registries
- add semantic compatibility classes
- claim fork/cycle graph analytics beyond what linear traversal naturally emits

## Consequences

- Chain-walk truncation/failure becomes explainable evidence instead of silent
  prefix success.
- Consumers can gate checks on deterministic structural findings without
  importing downstream vocabulary.
- Signed receipt verification remains a composable, separate check.

## References

- `crates/core/src/store/mod.rs` (`Store::walk_ancestors`, `Store::chain_walk_evidence`)
- `crates/core/src/store/chain_walk.rs`
- `docs/adr/ADR-0019-canonical-encoding-contract.md`
- `docs/adr/ADR-0020-schema-snapshot-drift-report.md`
