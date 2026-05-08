# Evidence Substrate Audit

This pass audits the shipped evidence-report family as batpak-native substrate.
It does not add new report families and does not unpark parked concepts.

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
| `EvidenceReportBody` derive | not implemented | Park (classification only) | only consider after repeated boilerplate proves value |
| `SchemaSnapshotSource` derive/helper | not implemented | Park (classification only) | only after schema snapshot API stabilizes further |
| Declarative report helper macros | no dedicated family macro | Park | keep helpers tiny; avoid DSL-style macro law |
| `build.rs` report logic | minimal for evidence family; broader invariants in build script | Keep tiny/deterministic | no env/network driven report behavior |
| `cargo xtask` family audit | no dedicated evidence-audit subcommand yet | Candidate | add only when family drift checks become repetitive |

Constraint kept: macro audit is classification-only. Do not implement
`#[derive(EvidenceReportBody)]` or `SchemaSnapshotSource` in this pass unless a
tiny correctness bug requires it; the goal is to identify whether future macro
extraction is earned, not to add the macro now.

## Platform Test Style Note

The easy platform unit-test suppressions in `src/store/platform/profile.rs` were
removed during the QA pass. The broader `tests/platform_backend.rs` integration
file still carries its pre-existing panic/unwrap assertion-style allowance; that
is legacy platform-test debt, not part of the evidence-report API contract and
not a pattern for new evidence tests.

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

Known bench gap:

- `benches/evidence_reports.rs` measures evidence-report construction on the
  default topology. It does not yet split evidence cost by topology. Add
  topology-parametrized evidence benches only if performance work needs that
  distinction; do not optimize absent a measured pathological regression.

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

## Parked-Item Promotion Gate

A parked item may only move out of parked if all pass:

1. Reuses the evidence-report family contract (deterministic body + canonical
   bytes + active batpak hash backend for `body_hash`).
2. Has at least one generic consumer example.
3. Fits subject identity via `Coordinate` / `Region` / `EventId` / `StableId`.
4. Has a deterministic test plan (including body-hash stability + sorting guarantees).
5. Introduces no consumer-specific semantic vocabulary in batpak public names.

## Consumer Dependency Closure Matrix

This matrix maps generic substrate needs to batpak public surfaces. Status here
is architectural closure status, not a CI pass result.

| Generic substrate need | batpak API/report/macro/xtask support | Test coverage in batpak | Reopen/cold-start coverage | Target/platform concern | Consumer replacement target | Status |
| --- | --- | --- | --- | --- | --- | --- |
| Canonical body identity | `batpak::canonical`, ADR-0019 | schema + report tests | N/A | hash backend feature parity (`blake3` vs fallback) | replace generic canonical prose with ADR-0019 citation | proven |
| Schema/fixture drift evidence | `SchemaSnapshotEvidenceReport` | `tests/schema_snapshot_report.rs`, family tests | snapshot/report deterministic checks | none special | cite batpak snapshot report contract | proven |
| Chain structural continuity | `ChainWalkEvidenceReport` | `tests/chain_walk_evidence_report.rs`, family tests | reopen chain test added | segment read behavior by backend | cite batpak chain walk report instead of custom explanation | proven |
| Subscriber lag/loss/frontier observation | `SubscriberFrontierEvidenceReport` | `tests/subscriber_frontier_observations.rs`, family tests | covered as store-backed request/report path | lossy vs cursor precision truthfulness | cite batpak frontier observation semantics | proven |
| Projection run evidence | `ProjectionRunEvidenceReport` | `tests/projection_run_evidence_report.rs`, family tests | reopen structural-field checks added | frontier volatility across reopen expected | shrink projection evidence prose to batpak report citation | proven |
| Read walk evidence | `ReadWalkEvidenceReport` | `tests/read_walk_evidence_report.rs`, family tests | reopen structural-field checks added | visible frontier sequence can move on lifecycle events | shrink read evidence prose to batpak report citation | proven |
| Typed event compile-time binding | `EventPayload` / `EventSourced` derives | derive tests + UI compile-fail | N/A | rustc diagnostic drift | cite batpak derive contracts, remove duplicated generic macro prose | proven |
| Frontier/watermark runtime boundary | `FrontierView`, `WatermarkSnapshot`, wait APIs | existing store/frontier tests + report tests | partially covered through reopen report tests | backend-specific durability/clock evidence | cite batpak frontier APIs directly | proven |
| Platform/runtime evidence | `PlatformEvidenceSummary`, platform xtask surfaces | platform tests + xtask | open/reopen platform posture already in store flows | per-target probe/verify still required | cite batpak platform evidence refs for generic host/runtime substrate notes | implemented; platform verification target-specific |
| Non-generic consumer semantics | intentionally not in batpak | N/A | N/A | N/A | keep in the consumer layer | consumer-owned |

## Revised Promotion Lanes

### Lane A (closest to current family contract)

- `CanonicalArtifactEnvelope`
- `CompactionReport`
- `IdempotencyLedger`
- `RegionBoundQuery`

### Lane B (needs stronger shared substrate first)

- `AttestedRegistry`
- `StateTransitionReport`
- `ReservationLedger`
- `BackupEnvelope`

### Lane C (remain parked until explicit generic pressure)

- process/sandbox/supervisor evidence
- audit assertion runner
- deterministic phase cache

## Native-Quality Gate

Do not call the evidence family batpak-native quality unless:

1. workspace tests pass
2. clippy passes
3. docs pass
4. evidence-family invariant tests pass
5. real-store/reopen tests pass where applicable
6. bench compile passes if benches were added
7. no forbidden downstream vocabulary appears in public API names

