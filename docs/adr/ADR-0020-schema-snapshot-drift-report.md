# ADR-0020: Schema Snapshot Drift Evidence Report

## Status
Accepted.

## Context
batpak now defines canonical encoding compatibility boundaries in ADR-0019.
The next low-risk evidence slice is schema/fixture drift reporting: consumers
need deterministic proof that expected shape and fixture bytes still match what
they observe at integration time.

This pressure is generic across event-sourced systems and should stay
structural. batpak must avoid turning schema snapshot evidence into protocol
semantics, migration policy, or downstream application law.

## Decision
batpak adds a small schema snapshot evidence surface in `src/schema.rs`.

### 1) Snapshot input and report body

The comparison surface uses:

- `stable_id`
- `schema_version`
- `expected_schema_hash`
- `observed_schema_hash`
- `expected_fixture_hash`
- `observed_fixture_hash`
- `change_class`
- deterministic `findings`

`SchemaSnapshotReportBody` is the deterministic payload.  
`SchemaSnapshotEvidenceReport` wraps that body with non-authority metadata
(`generated_at_unix_ms`, `batpak_version`, `diagnostics`) outside deterministic
body identity.

### 2) Drift classification model

First pass classification is intentionally conservative:

- exact match => `Unchanged`
- any drift => `Unknown`

The `Changed` variant exists as reserved room for future structurally-proven
classification work. This decision does not infer additive/breaking semantics.

### 3) Findings and deterministic ordering

Comparison emits structural findings only:

- stable id mismatch
- schema hash mismatch
- fixture hash mismatch

Findings are deterministically ordered in the report body.

### 4) Report body hash contract

Report body bytes use `batpak::canonical` and hash from those canonical bytes.
This follows ADR-0019: patch releases preserving the report schema version must
yield identical body bytes and body hash for equal logical report inputs.

## Non-goals

This slice does not add:

- protocol registry semantics
- artifact envelopes
- attested registries
- authority/privacy/context field classes
- migration oracle behavior

## Consequences

- batpak gets a generic schema/fixture drift proof object with minimal surface
  area.
- consumers can gate builds/tests on deterministic drift evidence without
  importing downstream vocabulary.
- deeper semantic compatibility logic remains a higher-layer concern.

## References

- `docs/adr/ADR-0019-canonical-encoding-contract.md`
- `src/schema.rs`
- `src/encoding.rs`
