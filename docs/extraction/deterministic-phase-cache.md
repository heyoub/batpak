# Deterministic phase cache (above-batpak / tooling design)

**Disposition:** reject / not needed in batpak until repeated deterministic-phase
glue shows up in `xtask`, integrity, schema, fixture, or bench flows with
measurable pain.

## Intended shape (when promoted)

Tooling-owned helper only (not store runtime):

- `phase_id` (stable string or hash)
- sorted `input_hashes`
- `output_hash`
- `evidence_hash` (optional correlation to an evidence report body hash)
- explicit `miss_reason` when lookup does not apply

**Forbidden in the canonical key:** timestamps, raw environment strings, raw
filesystem paths, and env-dependent invalidation. Reuse is justified only when
the phase output is genuinely deterministic from the keyed inputs.

## Prove-or-build gate

Survey showed no second hot path that re-encodes the same deterministic phase
with only hash-keyed inputs; `cargo xtask structural` and integrity checks
already centralize expensive static gates. Revisit this document when a third
copy-paste of the same phase wiring appears.
