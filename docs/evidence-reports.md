# Evidence Reports Family

batpak evidence reports are deterministic structural proof objects. They share a
single identity pattern:

- deterministic `*ReportBody`
- canonical body bytes via `batpak::canonical`
- `body_hash` derived from canonical body bytes using batpak's active hash
  backend (`blake3` when enabled; deterministic CRC32-backed fallback in
  no-default-feature builds)
- operational metadata (`generated_at_unix_ms`, `batpak_version`,
  `diagnostics`) outside deterministic body identity

This follows `docs/adr/ADR-0019-canonical-encoding-contract.md`.

## Family Members

- `SchemaSnapshotEvidenceReport` (`src/schema.rs`)
- `ChainWalkEvidenceReport` (`src/store/chain_walk.rs`)
- `SubscriberFrontierEvidenceReport` (`src/store/subscriber_frontier.rs`)
- `ProjectionRunEvidenceReport` (`src/store/projection_run.rs`)
- `ReadWalkEvidenceReport` (`src/store/read_walk.rs`)

## Arc Seal (v1 Family)

The first batpak evidence-report family is considered complete for this
extraction arc:

- canonical identity contract (`ADR-0019`)
- schema/fixture drift evidence (`ADR-0020`)
- chain continuity evidence (`ADR-0021`)
- subscriber frontier observations (`ADR-0022`)
- projection run evidence (`ADR-0024`)
- read walk evidence (`ADR-0025`)

This family is intentionally bounded. New report types require concrete pressure
that cannot be satisfied by existing report bodies.

## Shared Contracts

- **Deterministic body identity:** equal logical inputs produce equal body bytes
  and `body_hash` within the same report schema version.
- **Deterministic findings:** findings are emitted in sorted structural order.
- **Structural scope only:** these reports do not infer policy, protocol, or
  application semantics.
- **Receipt boundary:** reports are observations/proofs; they do not claim
  append/deny/commit authority like append or denial receipts.
- **No fake uncertainty:** `Unknown` is reserved for genuinely indeterminate
  facts. Use `Known` when batpak already exposes the value, `NotApplicable` when
  the field does not apply, `NotTracked` only when the substrate does not track
  the fact, and `Unavailable` for deterministic acquisition/encoding failures.
- **Non-capture of runtime configuration:** canonical report bodies intentionally
  do not attest to `StoreConfig`, platform profile paths, registry validation
  mode, signing-key configuration, durability gates, or the active hash feature.
  Those facts must be correlated through configuration, diagnostics, platform
  profiles, and build metadata when they matter to an audit.

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
  v1 still samples current visible index state rather than applying a stale-cache
  read policy.

## Public Surface

Evidence reports are exported from `batpak::store` and the prelude when the
types are practical caller-facing API, not internal fixture details. Hash aliases
and lower-level helper states stay in their owning modules unless consumers need
them to pattern-match report bodies without reaching through private internals.

## Non-goals

- No policy engine.
- No downstream application vocabulary.
- No implied exactness where internals only support coarse or unknown precision.

## Intentionally Parked

- Store resource envelope beyond `WriterPressure`.
- Canonical artifact envelope.
- Attested registry.
- Reservation ledger.
- State transition report.
- Audit assertion runner.
- Deterministic phase cache.
- Process/sandbox/supervisor evidence.
- Protocol registry semantics and semantic field classes.
