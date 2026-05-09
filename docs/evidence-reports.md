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

- `SchemaSnapshotEvidenceReport` (`crates/core/src/schema.rs`)
- `ChainWalkEvidenceReport` (`crates/core/src/store/chain_walk.rs`)
- `SubscriberFrontierEvidenceReport` (`crates/core/src/store/subscriber_frontier.rs`)
- `ProjectionRunEvidenceReport` (`crates/core/src/store/projection_run.rs`)
- `ReadWalkEvidenceReport` (`crates/core/src/store/read_walk.rs`)
- `StoreResourceEvidenceReport` / `StoreResourceEnvelope` (`crates/core/src/store/store_resource_report.rs`)

## Arc Seal (v1 Family)

The first batpak evidence-report family is considered complete for this
extraction arc:

- canonical identity contract (`ADR-0019`)
- schema/fixture drift evidence (`ADR-0020`)
- chain continuity evidence (`ADR-0021`)
- subscriber frontier observations (`ADR-0022`)
- projection run evidence (`ADR-0024`)
- read walk evidence (`ADR-0025`)
- store resource evidence (diagnostics snapshot envelope; schema v1 in-tree)

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
- `StoreResourceEvidenceReport`: deterministic snapshot over stable
  `StoreDiagnostics` fields (counts, frontier coordinates, writer pressure,
  topology label, optional cold-start `OpenIndexReport`, platform evidence).
  Raw `data_dir` paths never appear in the body; identity uses a path-byte
  digest. Bodies are point-in-time: full byte equality across `close`/`open` is
  not a contract because cold-start path and replayed system events can differ.

## Forensic query composition (no extra nominal type)

Generic “forensic query export” in batpak is **`ReadWalkEvidenceReport` plus
`CanonicalArtifactEnvelope`** when attestation or multi-artifact bundling is
required. See `docs/extraction/forensic-query-envelope.md` for the closure
disposition.

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

- Canonical artifact envelope.
- Attested registry.
- Reservation ledger.
- State transition report.
- Dedicated forensic query envelope type (composition path documented above).
- Deterministic phase cache (tooling design only until repeated measured pain).
- Process/sandbox/supervisor evidence (Moonwalker Host planning).
- Protocol registry semantics and semantic field classes (protocol-registry planning).

Tooling hygiene for evidence bodies: `cargo xtask evidence-audit`.
