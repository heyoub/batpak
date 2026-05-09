# Evidence Substrate Audit

This pass audits the shipped **evidence v1** family as batpak-native substrate.
**Evidence v1 is sealed** (no new report families in this doc pass). The
**evidence-debt-zero** hygiene arc is **closed**. The active arc is **Lane A
core substrate primitives** (`CanonicalArtifactEnvelope`, compaction structural
report, idempotency prove-or-build, region-bound query prove-or-build) without
reopening family design.

## Current State Check

- `tests/evidence_report_family.rs` now exists and is extended rather than forked.
- `benches/evidence_reports.rs` now exists and is extended rather than forked.
- `docs/extraction/evidence-substrate-audit.md` now exists and is extended
  rather than forked.
- Evidence report exports intentionally flow through:
  - `src/lib.rs` (`pub mod schema`, `pub mod store`, `pub mod prelude`)
  - `src/store/mod.rs` (report-type re-exports)
  - `src/prelude.rs` (caller-facing evidence report bodies/envelopes and
    request/status enums that applications are expected to pattern-match)

## Evidence QA Rule

Evidence reports must not fake certainty or fake uncertainty:

- `Known` is used when existing batpak machinery already exposes the fact.
- `NotApplicable` is used when a field does not apply to that report path.
- `NotTracked` is reserved for facts the substrate genuinely does not track.
- `Unavailable` carries deterministic acquisition/encoding failure reasons.
- `Unknown` is reserved for facts that are genuinely indeterminate, not values
  the implementation forgot to wire.

## Macro / Build / Xtask Classification

| Surface | Current shape | Disposition | Next arc | Blocker |
| --- | --- | --- | --- | --- |
| `EventPayload` proc macro | `crates/macros/src/lib.rs` + compile-fail tests | already covered | none | none — keep ui/trybuild parity as rustc evolves |
| `EventSourced` proc macro | `crates/macros/src/lib.rs` + derive tests | already covered | none | expand compile-fail coverage when macro contract changes |
| `EvidenceReportBody` derive | not implemented | implement in batpak tooling/helper | lane-tooling-evidence-derive | measure repeated boilerplate before implementing |
| `SchemaSnapshotSource` derive/helper | not implemented | implement in batpak tooling/helper | lane-tooling-schema-derive | lock stable input contract before deriving helpers |
| Declarative report helper macros | no dedicated family macro | reject / not needed | none | revisit only if measured duplication forces a tiny helper surface |
| `build.rs` report logic | minimal for evidence family; broader invariants in build script | already covered | none | keep tiny/deterministic; no env/network-driven report behavior |
| `cargo xtask` family audit | `cargo xtask evidence-audit` → `batpak-integrity evidence-audit` (schema anchors + export vocabulary) | already covered | none | extend anchors when new `*ReportBody` types ship |

Classification only: macro/report helper rows above are **not** Lane B
delivery commitments unless a separate arc promotes them.

## Platform Test Style Note

The prior broad panic/unwrap suppression debt in `tests/platform_backend.rs` is
closed in the evidence-debt-zero arc. Current platform backend tests use
`Result`-returning style and explicit error assertions without file-level
suppression. Keep this posture for new platform and evidence tests.

## ECS / Coordinate / Tile Fit

The term "tile" here refers to batpak's index-layout implementation details
(`AoSoA64`/mixed-kind tiled overlays), not a public evidence subject. Layout is an
optimization boundary: coordinate, event, and frontier semantics are authority;
AoS/SoA/AoSoA/tiled/SIMD choices may change cost, not answers or canonical
evidence.

### Per-report substrate fit

| Report | Subject model | Frontier/watermark | Appendable as typed event without body redesign | Projection/query friendliness | Metadata-independent `body_hash` |
| --- | --- | --- | --- | --- | --- |
| `SchemaSnapshotEvidenceReport` | `stable_id` | not required | yes (`stable_id` scoped event payload) | projectable/queryable by stable id coordinate strategy | yes |
| `ChainWalkEvidenceReport` | start event / receipt ref | implicit via walked chain + checked refs | yes | projectable/queryable by event id/chain subject refs | yes |
| `SubscriberFrontierEvidenceReport` | subscriber observation request | explicit available/consumed frontier sequence | yes | projectable/queryable by source + subscriber identity chosen by caller | yes |
| `ProjectionRunEvidenceReport` | `projection_id` + `source_refs` | explicit `input_frontier` | yes | projectable/queryable by projection id/source refs | yes |
| `ReadWalkEvidenceReport` | region-derived `source_refs` | explicit read input frontier | yes | projectable/queryable by source refs/region family | yes |
| `StoreResourceEvidenceReport` | path-byte digest + stable diagnostics slice | explicit `StoreResourceFrontierBody` watermarks | yes | operator/diagnostics plane; not a query selector | yes |

## Layout / Topology / SIMD / Tile Audit

batpak already exposes index topology as runtime substrate:

- `IndexTopology::aos()` — base maps only.
- `IndexTopology::scan()` — base maps plus SoA broad-scan overlay.
- `IndexTopology::entity_local()` — base maps plus entity-local SoAoS overlay.
- `IndexTopology::tiled()` — base maps plus kind-homogeneous AoSoA64 tiles.
- `IndexTopology::tiled_simd()` — base maps plus mixed-kind tiled overlay shaped
  for auto-vectorizable scans.
- `IndexTopology::all()` — currently all stable overlays except the experimental
  mixed-kind SIMD-shaped overlay.

`StoreDiagnostics` reports `index_topology` and `tile_count`; tiled topologies
must report live tile usage after population, while non-tiled topologies report
zero tile count.

Evidence-report posture:

- `ReadWalkEvidenceReport` is valid regardless of index topology.
- `ProjectionRunEvidenceReport` is valid regardless of index topology.
- `SchemaSnapshotEvidenceReport`, `ChainWalkEvidenceReport`, and
  `SubscriberFrontierEvidenceReport` are layout-neutral except where they use
  ordinary store/index query primitives.
- Deterministic report body ordering must be defined by canonical source refs,
  sequence order, and sorted findings, never by storage-layout iteration.
- SIMD-shaped tiled paths are optimization-only. They do not own semantic
  authority; scalar/topology parity tests must keep visible query truth
  equivalent.

Current coverage:

- `tests/index_topology.rs` covers topology constructors, diagnostics labels,
  and tile-count reporting.
- `tests/multi_view_parity.rs` checks visible query results and ordering across
  `aos`, `scan`, `entity-local`, `tiled`, and `all`, including reopen.
- `tests/unified_topology_red.rs` covers direct topology query correctness and
  `tiled-simd` parity against `aos`.
- `tests/evidence_report_family.rs` includes topology-independence checks for
  read-walk and projection-run evidence bodies and `body_hash`.

Topology bench status:

- `benches/evidence_reports.rs` now includes topology-parameterized lanes for
  read-walk and projection-run evidence across `aos`, `scan`, `entity-local`,
  `tiled`, `tiled-simd`, and `all`, plus a `store_resource` sanity lane.
- Schema/chain/subscriber benches remain layout-neutral by design unless a
  topology-specific hypothesis appears.

## Bench Rule

Bench compilation is required when a bench file is added. A bench run is
best-effort; at least one chain-walk or body-hash benchmark should run when
feasible. Do not optimize the evidence layer during this pass unless a clear
pathological regression appears.

## Config / Platform Gremlin Audit

Configuration can change cost, admission, or availability. It must not silently
change canonical evidence semantics:

- `StoreConfig::index.topology` changes in-memory routing and diagnostics, not
  report body meaning.
- `segment_max_bytes`, `fd_budget`, checkpoint/mmap settings, and sync cadence
  change storage/runtime posture; report bodies remain based on visible event
  semantics and explicit frontiers.
- writer mailbox capacity and restart policy are visible through diagnostics or
  writer/frontier behavior; they must not weaken report determinism.
- durability gates affect append completion waits, not the definition of an
  already-observed report body.
- signing keys affect append/denial receipt signatures, not evidence-report
  canonical body hashing.
- open report observer and platform profile paths are open/admission surfaces;
  failures or degraded platform evidence should appear through open reports,
  diagnostics, or platform admission summaries rather than being hidden.
- dangerous test hooks stay feature-gated and must not become evidence-report
  contract vocabulary.

Platform/HAL posture:

- `PlatformEvidenceSummary` is exposed through `StoreDiagnostics`.
- Parent-directory sync, lock symlink protection, mmap posture, active segment
  read posture, and store-path status have explicit evidence/admission states.
- Degraded states such as rename-only parent sync, best-effort check-then-open
  locking, unsupported mmap, and probe failures are explicit platform facts.
- Target-specific platform verification remains outside the evidence-report
  family; run `cargo xtask platform ...` on the target when platform admission
  is the question.

### Subject identity check

The family can already represent report subjects through one of:

- `EventId` (`ChainWalkStartRef`)
- `Region` selectors (`ReadWalkRequest` to `source_refs`)
- stable ids (`SchemaSnapshot`)
- small enums over subject references (`ProjectionSourceRef`, `ReadWalkSourceRef`)

This aligns with the required batpak-native identity posture:
`Coordinate` / `Region` / `EventId` / `StableId` shape, not consumer-specific
nouns.

## Disposition Discipline

No primitive may remain in a vague status. Every primitive must carry:

1. explicit owner layer (`batpak core`, `batpak tooling/helper`, `above batpak`,
   or `already covered`)
2. proof path (tests, reopen/cold-start coverage, platform coverage)
3. build arc and blocker (if any)

Allowed dispositions in the closure matrix:

- `already covered`
- `implement in batpak core`
- `implement in batpak tooling/helper`
- `implement above batpak`
- `reject / not needed`

## Consumer Dependency Closure Matrix

This matrix maps generic substrate needs to batpak public surfaces. Status here
is architectural closure status, not a CI pass result.

### Matrix changelog

- **Matrix v7.2 (2026-05-08): Pre-Downstream closure — `StoreResourceEvidenceReport` ships in core;
  `cargo xtask evidence-audit` lands; `ForensicQueryEnvelope` disposition documented as ReadWalk +
  artifact composition; deterministic phase cache documented reject; host process and protocol
  semantic field planning captured under `docs/extraction/`.**
- **Matrix v7.1 (2026-05-08): Lane B seal — B1–B4 matrix rows show `none` next arc where primitives
  shipped; reservation row documents `batpak::reservation`; tooling/above-batpak wave is the
  intentional follow-on (no further Lane B core primitives in this rail without a new arc).**
- **Matrix v7 (2026-05-08): Lane B4 — `batpak::reservation` ships `ReservationTransition`,
  `ReservationEntry`, closed structural `RESERVATION_STATE_*` lanes, `simulate_reservation_ledger`,
  `ReservationLedgerReportBody` + `reservation_ledger_report_body_hash`, reconciliation buckets +
  `reservation_reconciliation_report_body_hash`, sorted structural `ReservationFinding` coverage in
  `tests/lane_b4_reservation_substrate.rs`.**
- **Matrix v6 (2026-05-08): Lane B3 — `batpak::transition` ships `StateTransitionEvent`,
  `build_state_transition_report` / `StateTransitionReportBody` with sorted causes/findings,
  `state_transition_event_digest`, and structural `StateTransitionFinding` coverage in
  `tests/lane_b3_transition_substrate.rs`.**
- **Matrix v5 (2026-05-08): Lane B2 — `batpak::store` backup manifest envelope (`BackupManifestBody`,
  `BackupManifestEnvelope` / `BackupEnvelope`), sorted segment digests, deterministic
  `restore_proof_report_body` / `restore_proof_report_body_hash`, and structural
  `BackupEnvelopeFinding` coverage, including unsupported manifest schema findings, in
  `tests/lane_b2_backup_envelope_substrate.rs`.**
- **Matrix v4 (2026-05-08): Lane B1 — `batpak::registry` ships attested row bodies
  (`row_hash` with sorted `named_digests`), drift and verification reports with
  `schema_version` + sorted findings, supersession hygiene helpers, and
  composition with `CanonicalArtifactEnvelope` on normalized signing bytes;
  proof in `tests/lane_b1_registry_substrate.rs`.**
- **Matrix v3: Lane A kickoff — Evidence v1 sealed; evidence-debt-zero closed;
  Lane A primitives are CanonicalArtifactEnvelope, CompactionReport,
  IdempotencyLedger prove-or-build, RegionBoundQuery prove-or-build.**
  **Implementation notes (same v3 freeze):** `batpak::artifact` ships with INV-3
  carve-out in `build.rs` (definitions in `src/artifact.rs`; `pub mod artifact;`
  in `src/lib.rs`; `pub use crate::artifact::{{ … }}` in `src/prelude.rs`);
  `CompactionReportBody` carries `compaction_id`, sorted source segment refs,
  input bounds, and structural findings including pre-swap rollback; A3 proves
  **reject / not needed**; A4 proves **reject / not needed** (proof + batch pin
  in `tests/lane_a_artifact_substrate.rs`, `tests/lane_a_store_substrate.rs`,
  `tests/idempotent_batch_crash_recovery.rs`;
  compaction lifecycle coverage also shares fixtures with `tests/store_snapshot_compaction.rs`).
- **v2 (2026-05-08):** Lane A fullsend — `batpak::envelope` (canonical body vs
  envelope digests), `CompactionReportBody` + `Store::compact_with_report`,
  idempotency and region-bound discipline **proved redundant** as separate
  ledger/query types (`tests/lane_a_*_substrate.rs` + this matrix).

| Generic substrate need | Current batpak support | Test/reopen/platform coverage | Downstream shrink target | Disposition | Next arc | Blocker |
| --- | --- | --- | --- | --- | --- | --- |
| Canonical encoding contract | `batpak::canonical`, `ADR-0019` | schema/report tests; hash-backend parity lane in CI | replace ad-hoc canonical prose with `ADR-0019` citation | already covered | none | none |
| Schema snapshot evidence | `SchemaSnapshotEvidenceReport` | `tests/schema_snapshot_report.rs`, family invariants, deterministic hash checks | cite schema drift report contract directly | already covered | none | none |
| Chain walk evidence | `ChainWalkEvidenceReport` + shared ancestry parent-hash helper | `tests/chain_walk_evidence_report.rs`, family invariants, reopen checks | remove custom continuity explanation text | already covered | none | none |
| Subscriber frontier evidence | `SubscriberFrontierEvidenceReport` | `tests/subscriber_frontier_observations.rs`, family invariants | cite lag/loss/frontier observation contract directly | already covered | none | none |
| Projection run evidence | `ProjectionRunEvidenceReport` with outcome-bound `input_frontier` | `tests/projection_run_evidence_report.rs`, family topology parity tests | reduce custom projection observability glue | already covered | none | none |
| Read walk evidence | `ReadWalkEvidenceReport` with index-visible boundary owner | `tests/read_walk_evidence_report.rs`, family topology parity tests, reopen checks | reduce custom read-observation wrappers | already covered | none | none |
| Forensic query export (nominal envelope) | `ReadWalkEvidenceReport` + `CanonicalArtifactEnvelope` composition | `tests/evidence_report_family.rs`, read-walk suites; `docs/extraction/forensic-query-envelope.md` | cite composition instead of a parallel nominal type | reject / not needed | none | no Downstream query grammar in core |
| Store resource evidence | `StoreResourceEvidenceReport` (`StoreResourceEnvelope`) over stable diagnostics; path identity via digest only | `tests/lane_store_resource_evidence.rs`; `Store::store_resource_evidence_report` | cite store resource report for operator substrate snapshots | implement in batpak core | none | **v1 shipped** — bodies are point-in-time; full equality across reopen is not a contract |
| Typed event compile-time binding | `EventPayload` + `EventSourced` derives | derive tests + compile-fail UI tests | cite derive contracts instead of duplicating macro law | already covered | none | none |
| Frontier/watermark runtime boundary | `FrontierView`, `WatermarkSnapshot`, wait APIs | store/frontier tests + evidence family coverage | cite frontier APIs directly for generic boundary mechanics | already covered | none | none |
| Platform/runtime evidence | `PlatformEvidenceSummary` + `cargo xtask platform ...` | `tests/platform_backend.rs`, platform profile tests, xtask verify lanes | cite platform evidence and admission states as substrate facts | already covered | none | per-target verify still required at deploy target |
| CanonicalArtifactEnvelope (`batpak::artifact`) | `CanonicalArtifactEnvelope<T>`, `ArtifactVerificationReport`, sorted signature/attestation refs, `ARTIFACT_ENVELOPE_FRAMING_VERSION` | `tests/lane_a_artifact_substrate.rs`; INV-3 + `build.rs` gate for `artifact` noun | cite `batpak::artifact` for body vs envelope identity | implement in batpak core | lane-a-core-primitives | **v1 shipped** — future envelope-schema evolution / multi-version verification needs explicit ADR; `trajectory`/`tenant` remain banned crate-wide on declarations |
| CompactionReport (`CompactionReportBody`, `Store::compact_with_report`) | structural report: `compaction_id`, sorted source segment ids, bounds, output segment byte hash when performed, findings | `tests/lane_a_store_substrate.rs` (incl. performed merge); lifecycle in `src/store/lifecycle.rs`; snapshot/compaction races in `tests/store_snapshot_compaction.rs` | cite compaction report vs ad-hoc merge prose | implement in batpak core | lane-a-core-primitives | **v1 shipped** — extend fields only via `schema_version` + deterministic ordering rules |
| IdempotencyLedger (prove-or-build) | key = provisional **event id**; single + batch replay via writer/index; TTL **out of scope** | `tests/lane_a_store_substrate.rs`; `tests/idempotent_batch_crash_recovery.rs` | no separate ledger type unless new invariants demand it | reject / not needed | lane-a-core-primitives | prove path complete; redundant ledger would duplicate index facts unless new semantics (e.g. expiry) gain an owner |
| RegionBoundQuery (prove-or-build) | public APIs require `Region` / entity / scope / kind / `cursor_guaranteed(&Region)` | `tests/lane_a_store_substrate.rs`; topology parity elsewhere | cite explicit bounds instead of a packaged query value | reject / not needed | lane-a-core-primitives | prove path complete unless cross-cutting evidence needs a nominal `RegionBoundQuery` struct |
| AttestedRegistry mechanics (`batpak::registry`) | `RegistryRowBody` + `row_hash`; `CanonicalArtifactEnvelope<RegistryRowBody>` verify path; drift + verification reports with `body_hash`; supersession/dup/cycle findings | `tests/lane_b1_registry_substrate.rs` | cite `batpak::registry` for signed-row / lifecycle / supersession substrate | implement in batpak core | none | **B1 v1 shipped** — extend only with `schema_version` bumps + deterministic ordering rules |
| BackupEnvelope (`batpak::store` backup_envelope) | `BackupManifestBody` + `backup_manifest_body_hash`; `BackupManifestEnvelope` / `BackupEnvelope`; `RestoreProofReportBody` + `restore_proof_report_body_hash`; `verify_backup_manifest_envelope` | `tests/lane_b2_backup_envelope_substrate.rs` | cite store backup envelope for segment digest restore proof | implement in batpak core | none | **B2 v1 shipped** — unsupported manifest `schema_version` is explicit evidence; extend via `schema_version` + sorted segment / finding rules only |
| StateTransition report/event (`batpak::transition`) | `StateTransitionEvent` + `state_transition_event_digest`; `build_state_transition_report` + `state_transition_report_body_hash`; sorted causes and allowed-edge hygiene findings | `tests/lane_b3_transition_substrate.rs` | cite `batpak::transition` for generic transition evidence | implement in batpak core | none | **B3 v1 shipped** — extend via `schema_version` + sorted cause/finding/edge rules only |
| Reservation ledger (`batpak::reservation`) | `ReservationTransition` + normalized transition log digest; `simulate_reservation_ledger` → `ReservationLedgerReportBody` + `reservation_ledger_report_body_hash`; `reservation_reconciliation_report` + body hash; sorted `ReservationFinding` invalid-transition reasons | `tests/lane_b4_reservation_substrate.rs` | cite `batpak::reservation` for generic reserve/commit/refund/expire/orphan lane mechanics | implement in batpak core | none | **B4 v1 shipped** — extend via `schema_version` + sorted entry/finding/cause/reconciliation rules only; no policy vocabulary |
| Audit assertion runner | `cargo xtask evidence-audit` (static anchors + vocabulary) | `tools/integrity/src/evidence_audit.rs`; extend with deeper AST rules as needed | run before Downstream host work when touching evidence bodies | implement in batpak tooling/helper | none | **v1 shipped** — runtime doctrine remains in `tests/evidence_report_family.rs` |
| Deterministic phase cache | design note only (`docs/extraction/deterministic-phase-cache.md`) | no hot-path duplication observed in xtask/integrity | avoid speculative cache crate surface | reject / not needed | none | revisit only after measured repeated deterministic-phase glue |
| process/sandbox/supervisor evidence | Downstream Host planning (`docs/extraction/downstream-host-process-boundary-evidence.md`) | N/A in batpak core | keep deployment/runtime orchestration semantics above batpak | implement above batpak | downstream-host | composes `PlatformEvidenceSummary`; no store runner primitive |
| protocol registry semantics / field classes | protocol-registry planning (`docs/extraction/protocol-registry-semantic-field-classes.md`) | N/A in batpak core | keep protocol/domain semantics above batpak | implement above batpak | protocol-registry | composes `batpak::registry` row mechanics only |

## Implementation Waves

### Lane A — **lane-a-core-primitives** (in this repo revision)

- **A1:** `batpak::artifact` / `CanonicalArtifactEnvelope` — **landed** (crate-level, no `store` import).
- **A2:** `CompactionReportBody` + `Store::compact_with_report` — **landed** (store-owned; composes `lifecycle::compact`).
- **A3:** Idempotency ledger — **reject / not needed** (append + batch replay proof); **TTL out of scope**.
- **A4:** Region-bound query — **reject / not needed** (explicit `Region` / predicate APIs); no nominal helper type.

### Lane B — attested substrate mechanics

- **B1 `AttestedRegistry` / `batpak::registry` — landed** (signed immutable rows, drift, verification, supersession helpers).
- **B2 `BackupEnvelope` / `batpak::store::backup_envelope` — landed** (manifest + segment digests + restore proof).
- **B3 `StateTransition` / `batpak::transition` — landed** (generic event + structural report + edge checks).
- **B4 `ReservationLedger` / `batpak::reservation` — landed** (structural simulation + ledger/reconciliation report hashes).
- **Lane B primitive rail (B1–B4) — sealed for this revision** (matrix + harness updated; full `cargo xtask ci` on landing commit; next intentional work is the tooling/above-batpak split, not reflexive core primitives).

### Tooling/Above-batpak wave (non-core ownership)

- `cargo xtask evidence-audit` (static evidence anchors — landed v1)
- `DeterministicPhaseCache` (documented reject until measured pain — `docs/extraction/deterministic-phase-cache.md`)
- process/sandbox/supervisor evidence (`docs/extraction/downstream-host-process-boundary-evidence.md`)
- protocol registry semantics / field classes (`docs/extraction/protocol-registry-semantic-field-classes.md`)

## Native-Quality Gate

Do not call the evidence family batpak-native quality unless:

1. workspace tests pass
2. clippy passes
3. docs pass
4. evidence-family invariant tests pass
5. Lane A substrate tests pass (`cargo test --test lane_a_artifact_substrate`; `cargo test --test lane_a_store_substrate`)
6. Lane B1 registry substrate tests pass (`cargo test --test lane_b1_registry_substrate`)
7. Lane B2 backup envelope substrate tests pass (`cargo test --test lane_b2_backup_envelope_substrate`)
8. Lane B3 transition substrate tests pass (`cargo test --test lane_b3_transition_substrate`)
9. Lane B4 reservation substrate tests pass (`cargo test --test lane_b4_reservation_substrate`)
10. real-store/reopen tests pass where applicable
11. bench compile passes if benches were added
12. `cargo xtask structural` passes (includes `HARNESS_LEDGER.md` lint)
13. doctrine-bearing tests and `HARNESS_LEDGER.md` updated when adding invariants
14. no forbidden downstream vocabulary appears in public API names
