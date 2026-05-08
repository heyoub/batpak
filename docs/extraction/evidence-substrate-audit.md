# Evidence Substrate Audit

This pass audits the shipped evidence-report family as batpak-native substrate.
It does not add new report families in this arc.

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

| Surface | Current shape | Decision in this pass | Next requirement |
| --- | --- | --- | --- |
| `EventPayload` proc macro | `crates/macros/src/lib.rs` + compile-fail tests | Strengthen only | keep ui/trybuild parity as rustc evolves |
| `EventSourced` proc macro | `crates/macros/src/lib.rs` + derive tests | Strengthen only | expand compile-fail coverage when macro contract changes |
| `EvidenceReportBody` derive | not implemented | implement in tooling/helper only after pressure is proven | repeated boilerplate across multiple reports must be measured first |
| `SchemaSnapshotSource` derive/helper | not implemented | implement in tooling/helper only after schema API stabilization | lock stable input contract before deriving helpers |
| Declarative report helper macros | no dedicated family macro | reject for now (core), revisit only with measured duplication | keep helpers tiny; avoid DSL-style macro law |
| `build.rs` report logic | minimal for evidence family; broader invariants in build script | Keep tiny/deterministic | no env/network driven report behavior |
| `cargo xtask` family audit | no dedicated evidence-audit subcommand yet | Candidate | add only when family drift checks become repetitive |

Constraint kept: macro audit is classification-only. Do not implement
`#[derive(EvidenceReportBody)]` or `SchemaSnapshotSource` in this pass unless a
tiny correctness bug requires it; the goal is to identify whether future macro
extraction is earned, not to add the macro now.

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
  `tiled`, `tiled-simd`, and `all`.
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

- **v2 (2026-05-08):** Lane A fullsend — `batpak::envelope` (canonical body vs
  envelope digests), `CompactionReportBody` + `Store::compact_with_report`,
  idempotency and region-bound discipline **proved redundant** as separate
  ledger/query types (`tests/lane_a_fullsend_substrate.rs` + this matrix).

| Generic substrate need | Current batpak support | Test/reopen/platform coverage | Downstream shrink target | Disposition | Next arc | Blocker |
| --- | --- | --- | --- | --- | --- | --- |
| Canonical encoding contract | `batpak::canonical`, `ADR-0019` | schema/report tests; hash-backend parity lane in CI | replace ad-hoc canonical prose with `ADR-0019` citation | already covered | none | none |
| Schema snapshot evidence | `SchemaSnapshotEvidenceReport` | `tests/schema_snapshot_report.rs`, family invariants, deterministic hash checks | cite schema drift report contract directly | already covered | none | none |
| Chain walk evidence | `ChainWalkEvidenceReport` + shared ancestry parent-hash helper | `tests/chain_walk_evidence_report.rs`, family invariants, reopen checks | remove custom continuity explanation text | already covered | none | none |
| Subscriber frontier evidence | `SubscriberFrontierEvidenceReport` | `tests/subscriber_frontier_observations.rs`, family invariants | cite lag/loss/frontier observation contract directly | already covered | none | none |
| Projection run evidence | `ProjectionRunEvidenceReport` with outcome-bound `input_frontier` | `tests/projection_run_evidence_report.rs`, family topology parity tests | reduce custom projection observability glue | already covered | none | none |
| Read walk evidence | `ReadWalkEvidenceReport` with index-visible boundary owner | `tests/read_walk_evidence_report.rs`, family topology parity tests, reopen checks | reduce custom read-observation wrappers | already covered | none | none |
| Typed event compile-time binding | `EventPayload` + `EventSourced` derives | derive tests + compile-fail UI tests | cite derive contracts instead of duplicating macro law | already covered | none | none |
| Frontier/watermark runtime boundary | `FrontierView`, `WatermarkSnapshot`, wait APIs | store/frontier tests + evidence family coverage | cite frontier APIs directly for generic boundary mechanics | already covered | none | none |
| Platform/runtime evidence | `PlatformEvidenceSummary` + `cargo xtask platform ...` | `tests/platform_backend.rs`, platform profile tests, xtask verify lanes | cite platform evidence and admission states as substrate facts | already covered | none | per-target verify still required at deploy target |
| Canonical envelope framing (`CanonicalEnvelope` in `batpak::envelope`) | body digest vs envelope digest + verification report | `tests/lane_a_fullsend_substrate.rs`; public names avoid INV-3 banned noun in `build.rs` | cite `batpak::envelope` instead of duplicating body/envelope hashing prose | already covered | none | none — design discussions may say “artifact envelope”; shipped API is `batpak::envelope` |
| Compaction structural evidence (`CompactionReportBody`, `Store::compact_with_report`) | deterministic report + `body_hash`; no policy semantics | `tests/lane_a_fullsend_substrate.rs`; segment outcome types serialized for evidence | cite compaction report for merge provenance | already covered | none | none |
| Idempotency / duplicate replay substrate | `AppendOptions::with_idempotency` sets provisional **event id**; writer returns prior receipt via global `by_id` lookup | `tests/lane_a_fullsend_substrate.rs` | document that idempotency key **is** the event id (no separate ledger row type) | reject / not needed | none | a distinct `IdempotencyLedger` type would duplicate index facts unless new invariants appear |
| Region-bound read discipline | public bulk reads take `Region`, entity, scope, kind, or cursor over `Region` — no hidden unbounded public iterator | `tests/lane_a_fullsend_substrate.rs` | cite `query` / `by_scope` / `stream` / `by_fact` / `cursor_guaranteed` | reject / not needed | none | a packaged `RegionBoundQuery` value type deferred until cross-cutting evidence needs outweigh tuple call sites |
| AttestedRegistry mechanics | no core immutable-row attestation primitive yet | no dedicated tests yet | move generic row attestation mechanics down; keep mapping semantics above | implement in batpak core | lane-b-attested-registry | depends on `batpak::envelope` framing |
| BackupEnvelope | backup/restore flows exist without envelope object | cold-start/rebuild tests exist; no backup envelope proof type | replace custom restore manifest prose with stable substrate envelope | implement in batpak core | lane-b-backup | depends on `batpak::envelope` and compaction provenance shape |
| StateTransition report/event | no generic state transition primitive | lifecycle and outcome tests exist; no generic transition report | collapse repeated generic transition narratives into one type | implement in batpak core or reject as domain law | lane-b-state-transition | needs strict generic boundary to avoid workflow semantics |
| Reservation ledger | no generic reserve/commit/refund primitive | no dedicated coverage | provide generic reservation accounting substrate if truly cross-consumer | implement in batpak core or reject as domain-specific | lane-b-reservation-ledger | must prove genericity without domain nouns |
| StoreResourceEnvelope beyond `WriterPressure` | partial via `WriterPressure`; broader counters not stabilized | diagnostics coverage exists; no stable broader envelope contract | unify generic store-resource evidence if stable counters exist | implement in batpak core or reject as premature | lane-b-resource-envelope | blocked on stable ownership and semantics of additional counters |
| Audit assertion runner | no core API; integrity + xtask infrastructure exists | integrity/structural checks already tested | centralize doctrine checks in tooling lane | implement in batpak tooling/helper | lane-tooling-audit-runner | choose host surface (`xtask` vs helper crate) |
| Deterministic phase cache | no dedicated primitive | no dedicated coverage | avoid repeated deterministic-phase glue if pressure proves it | implement in batpak tooling/helper or reject | lane-tooling-phase-cache | needs measured repeated pain before implementation |
| process/sandbox/supervisor evidence | intentionally outside store/platform substrate scope | N/A in batpak core | keep deployment/runtime orchestration semantics above batpak | implement above batpak | above-batpak-runtime | would require introducing non-store runtime law into batpak |
| protocol registry semantics / field classes | intentionally outside generic substrate | N/A in batpak core | keep protocol/domain semantics above batpak | implement above batpak | above-batpak-protocol-semantics | would import domain vocabulary into core |

## Implementation Waves

### Lane A (closest to current family contract) — **v2 landed**

- **A1:** `batpak::envelope` — canonical envelope framing (`CanonicalEnvelope`, …).
- **A2:** `CompactionReportBody`, `compact_with_report`, helpers in
  `src/store/compaction_report.rs`.
- **A3:** Idempotency — **reject / not needed** (substrate = append id + index); see matrix row.
- **A4:** Region bounds — **reject / not needed** (explicit `Region` / predicates); see matrix row.

### Lane B (needs stronger shared substrate first)

- `AttestedRegistry`
- `StateTransitionReport`
- `ReservationLedger`
- `BackupEnvelope`

### Tooling/Above-batpak wave (non-core ownership)

- `AuditAssertionRunner` (tooling/helper ownership)
- `DeterministicPhaseCache` (tooling/helper unless core pressure is proven)
- process/sandbox/supervisor evidence (above batpak ownership)
- protocol registry semantics / field classes (above batpak ownership)

## Native-Quality Gate

Do not call the evidence family batpak-native quality unless:

1. workspace tests pass
2. clippy passes
3. docs pass
4. evidence-family invariant tests pass
5. Lane A substrate tests pass (`cargo test --test lane_a_fullsend_substrate`)
6. real-store/reopen tests pass where applicable
7. bench compile passes if benches were added
8. `cargo xtask structural` passes (includes `HARNESS_LEDGER.md` lint)
9. doctrine-bearing tests and `HARNESS_LEDGER.md` updated when adding invariants
10. no forbidden downstream vocabulary appears in public API names

