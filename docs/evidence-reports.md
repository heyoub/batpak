# Evidence Reports Family

batpak evidence reports are deterministic structural proof objects over their
canonical body inputs (per report type and schema version). They share a
single identity pattern:

- deterministic `*ReportBody`
- canonical body bytes via `batpak::canonical`
- `body_hash` derived from canonical body bytes using batpak's active hash
  backend (`blake3` when enabled; deterministic CRC32-backed fallback in
  no-default-feature builds)
- operational metadata (`generated_at_unix_ms`, `batpak_version`,
  `diagnostics`) outside deterministic body identity

This follows `docs/ADR-0019-canonical-encoding-contract.md`.

## Family Members

- `SchemaSnapshotEvidenceReport` (`crates/core/src/schema.rs`)
- `ChainWalkEvidenceReport` (`crates/core/src/store/chain_walk.rs`)
- `SubscriberFrontierEvidenceReport` (`crates/core/src/store/subscriber_frontier.rs`)
- `ProjectionRunEvidenceReport` (`crates/core/src/store/projection_run.rs`)
- `ReadWalkEvidenceReport` (`crates/core/src/store/read_walk.rs`)
- `StoreResourceEvidenceReport` / `StoreResourceEnvelope` (`crates/core/src/store/store_resource_report.rs`)

## v1 Family Seal

The first batpak evidence-report family is complete for the current substrate
surface:

- canonical identity contract (`ADR-0019`)
- schema/fixture drift evidence (`ADR-0020`)
- chain continuity evidence (`ADR-0021`)
- subscriber frontier observations (`ADR-0022`)
- projection run evidence (`ADR-0024`)
- read walk evidence (`ADR-0025`)
- store resource evidence (diagnostics snapshot envelope; schema v1 in-tree)

This family is intentionally bounded. New report types require concrete
substrate pressure that cannot be satisfied by existing report bodies.

## Shared Contracts

- **Deterministic body identity:** equal stable logical inputs produce equal body
  bytes and `body_hash` within the same report schema version. Bodies that embed
  timing or frontier snapshots (notably `StoreResourceEvidenceReport` with
  optional `OpenIndexReport`) are **point-in-time operational evidence** over
  the captured diagnostics snapshot: `body_hash` is the identity of that
  observation, not a replay-stable proof that two opens share identical timing,
  cold-start path, or watermark coordinates.
- **Deterministic findings:** findings are emitted in sorted structural order.
- **Structural scope only:** these reports do not infer policy, protocol, or
  application semantics.
- **Receipt boundary:** reports are observations/proofs; they do not claim
  append/deny/commit authority like append or denial receipts.
- **No fake uncertainty:** `Unknown` is reserved for genuinely indeterminate
  facts. Use `Known` when batpak already exposes the value, `NotApplicable` when
  the field does not apply, `NotTracked` only when the substrate does not track
  the fact, and `Unavailable` for deterministic acquisition/encoding failures.
- **Runtime configuration scope:** canonical bodies do not embed full serialized
  `StoreConfig`, platform profile paths, registry validation mode, signing-key
  material, per-append durability gate wiring, or the active hash feature id.
  Correlate those facts from configuration, dispatch, platform profiles,
  receipts, and build metadata when they matter to an audit.
- **Store resource selected config facets:** `StoreResourceEvidenceReport`
  intentionally captures a small stable subset derived from open-time
  configuration and index state: `segment_max_bytes`, `fd_budget`, writer
  `restart_policy` shape, index topology label, tile counts, frontier
  coordinates, writer pressure, optional cold-start `OpenIndexReport`, and
  platform evidence summary (see `crates/core/src/store/store_resource_report.rs`).
  This is not a complete configuration dump.

## What Each Report Proves

- `SchemaSnapshotEvidenceReport`: deterministic structural drift over schema and
  fixture hashes; not migration or semantic compatibility policy.
- `ChainWalkEvidenceReport`: deterministic structural chain continuity findings;
  not causation/correlation meaning.
- `SubscriberFrontierEvidenceReport`: deterministic subscriber lag/loss/frontier
  observations. `LossObserved` is emitted only for non-`Unknown` loss precision;
  unknown loss precision remains explicit in the body without pretending loss
  was observed.
- `ProjectionRunEvidenceReport`: deterministic projection-run subject/boundary/
  freshness/cache/checkpoint/output identity observations from existing
  projection machinery; not workflow or protocol projection meaning.
- `ReadWalkEvidenceReport`: deterministic opt-in read selector/boundary/count/
  limit/proof-ref observations. Its `freshness_intent` records caller intent;
  v1 still samples current visible index state rather than applying a
  stale-cache read policy.
- `StoreResourceEvidenceReport`: canonical snapshot over stable
  `StoreDiagnostics` fields (counts, frontier coordinates, writer pressure,
  selected config-derived facets above, optional cold-start `OpenIndexReport`
  including reopen phase micros when present, platform evidence).
  Raw `data_dir` paths never appear in the body; identity uses a path-byte
  digest. Bodies are point-in-time: full byte equality across `close`/`open` is
  not a contract because cold-start path, timing, frontier coordinates, and
  replayed system events can differ while the store remains consistent.

## Forensic Query Composition

Generic forensic query export in batpak is `ReadWalkEvidenceReport` plus
`CanonicalArtifactEnvelope` when attestation or multi-artifact bundling is
required. A dedicated nominal forensic-query envelope is not part of the current
substrate surface.

## Public Surface

Evidence reports are exported from `batpak::store` and the prelude when the
types are practical caller-facing API, not internal fixture details. Hash
aliases and lower-level helper states stay in their owning modules unless
consumers need them to pattern-match report bodies without reaching through
private internals.

## Non-goals

- No policy engine.
- No downstream application vocabulary.
- No implied exactness where internals only support coarse or unknown precision.

## Owned Outside This Family

- Additional canonical artifact workflows beyond the generic envelope already in
  `crates/core/src/artifact.rs`.
- Additional registry attestation flows beyond the registry row envelope already
  in `crates/core/src/registry.rs`.
- Reservation ledger expansion.
- State transition report expansion.
- Dedicated forensic query envelope type.
- Deterministic phase cache for tooling, if repeated measured pain justifies it.
- Process/sandbox/supervisor evidence in a host/supervisor owner.
- Protocol registry semantics and semantic field classes in a protocol owner.

Tooling hygiene for evidence bodies: `cargo xtask evidence-audit`.
