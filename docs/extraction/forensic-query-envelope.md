# Forensic query envelope (disposition: already covered)

**Disposition:** reject / not needed as a separate batpak nominal type for the
pre-Downstream arc.

## What “forensic query” means generically in batpak

Bounded read observability is already shipped as
`ReadWalkEvidenceReport` / `ReadWalkReportBody` (`crates/core/src/store/read_walk.rs`):

- explicit region / source refs
- input frontier and freshness intent
- returned vs matched vs dropped counts and proof refs
- sorted structural findings
- canonical `body_hash` over the report body (metadata outside the hash)

Export and attestation wrapping use `CanonicalArtifactEnvelope` / artifact
verification (`crates/core/src/artifact.rs`, `batpak::prelude`).

## When a dedicated `ForensicQueryEnvelope` would return

Only if a named invariant cannot be expressed as:

1. a `ReadWalkRequest` + `ReadWalkEvidenceReport`, and/or
2. composition with `CanonicalArtifactEnvelope`, and/or
3. additional existing evidence reports (projection run, chain walk, schema snapshot),

without importing Downstream query grammar or multi-tenant protocol semantics
into batpak core.

Until such a gap is written down with a concrete invariant, treat forensic
query export as **composition**, not a new core primitive.
