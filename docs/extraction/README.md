# Extraction Seeds

This directory contains extraction and classification notes for generic batpak
substrate candidates.

They are not batpak API promises until promoted into batpak ADRs, public docs, tests, or exported Rust APIs.

Use these files as coordination memory and contamination filters. Before implementing any row, inspect batpak's current modules and prefer strengthening existing public guarantees over copying sketch names literally.

## Intake Rule

Treat extraction seeds as private pressure, not durable batpak design language.
Accepted work must be restated as batpak-native requirements before it reaches
ADRs, public docs, tests, or exported Rust APIs.

batpak may grow, but only as a generic event-sourced runtime and evidence
substrate: canonical encoding, append receipts, deterministic evidence reports,
chain walking, schema and fixture snapshots, frontiers, cursor and replay
mechanics, projection evidence, subscriber lag/loss observations, and
store/platform facts.

Do not import consumer-specific semantics or policy into batpak. If a proposed
API cannot serve multiple generic event-sourced systems unchanged, park it.

## Inventory Before Implementation

The first artifact for any extraction pass is a classification table, not a new
module tree. Inspect existing batpak code and ADRs first, then classify the
smallest missing public guarantee:

```text
Candidate:
Existing batpak surface:
Generic consumer examples:
What already works:
What is missing:
Smallest public API/docs/test needed:
Domain-coupling risk:
Decision: docs only / helper API / report API / park
```

Prefer docs, tests, deterministic report APIs, and small helpers over new
architecture. Do not rewrap existing primitives unless the current surface lacks
a concrete public guarantee.

For dependency-closure passes, include a `consumer replacement target` column.
The point is not merely to show that batpak can do the work; it is to identify
which generic mechanism prose can shrink to batpak citations while
consumer-specific policy stays outside this crate.

## First-Wave Candidates

The safest first-wave candidates are narrow, generic, and evidence-shaped:

1. Canonical encoding contract.
2. Schema/fixture snapshot reports.
3. Receipt-chain evidence reports.
4. Subscriber frontier observations for lossy delivery.

Use "report" for deterministic verifier/audit/read/projection output unless the
operation actually appends, denies, or commits and therefore earns an append or
denial receipt.

## Candidate Classification

| Candidate | Existing batpak surface | Generic consumer examples | What already works | What is missing | Smallest public API/docs/test needed | Domain-coupling risk | Decision |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Canonical encoding contract | `batpak::canonical` (`src/encoding.rs`) | compliance ledger, signed audit export | Named-field MessagePack helpers exist and are used in signing/encoding paths | Explicit compatibility contract boundary | ADR defining compatibility promises and non-promises | Low | docs only |
| Schema/fixture snapshot reports | `SchemaSnapshotEvidenceReport` (`src/schema.rs`) | data emitter, regulated audit harness | Deterministic schema/fixture drift report contract exists | Optional derive/helper ergonomics only | Keep core report stable; add tooling helpers only when repeated callers need them | Medium | shipped report API |
| Receipt-chain evidence report | `AppendReceipt`, hash chain on stored events, `Store::walk_ancestors`, receipt verification helpers | financial ledger, compliance event store | Chain material exists and can be walked | Structured findings and deterministic report ordering | New report API + tests over continuity/missing/hash mismatch | Low | report API |
| Subscriber frontier observations | lossy `Subscription`, cursor worker checkpoints, frontier and gap observations | local-first sync engine, realtime feed processor | Lossy delivery and durable cursor mechanics are implemented | Public lag/loss/frontier evidence contract | Small helper/report surface + docs + tests | Low | report API |
| Projection replay evidence | `ProjectionWatcher`, `project_if_changed`, frontier waits | projection-backed analytics, stateful cache rebuilder | Projection replay/apply mechanics are mature | Public deterministic run/checkpoint evidence shape | Implemented as `Store::project_run_evidence`; no projection registry or workflow semantics | Medium | report API |
| Read evidence reports | `query`, `stream`, `by_scope`, `by_fact_typed`, `cursor` | forensic search, compliance read pipeline | Read/query primitives are available | Deterministic as-of/walked/dropped/proof report type | Implemented as opt-in `query_with_read_walk_evidence`; `freshness_intent` records caller intent while v1 samples current visible state | Medium | report API |
| Store resource envelope | `StoreResourceEvidenceReport` / `StoreResourceEnvelope` | embedded store operator, robotics recorder | Point-in-time store-owned diagnostics/frontier/resource report exists | Future counters only when store-owned facts are stable | Keep v1 point-in-time contract; extend by schema version only | Low | shipped report API |
| Canonical artifact envelope | `CanonicalArtifactEnvelope<T>` (`src/artifact.rs`) | signed artifact archive, plugin package manager | Generic body-vs-envelope identity model exists | Multi-version envelope verification policy | Keep v1 framing stable; extend by ADR/schema version | High | shipped core primitive |
| Attested registry | `RegistryRowBody` + `CanonicalArtifactEnvelope<RegistryRowBody>` | schema registry, plugin registry | Signed immutable row lifecycle, drift, verification, supersession checks exist | Consumer-specific row-kind registries | Keep row payload opaque; no domain vocabulary in core | High | shipped core primitive |
| Reservation ledger | `ReservationTransition`, `simulate_reservation_ledger`, reconciliation reports | inventory allocator, quota scheduler | Generic reserve/commit/refund/expire/orphan mechanics exist | Consumer policy for subject meaning and allocation rules | Keep dimensionless units and opaque subject refs | High | shipped core primitive |
| State transition report | `StateTransitionEvent`, `build_state_transition_report` | workflow engine, device lifecycle tracker | Generic transition evidence report API exists | Consumer policy for state meaning | Keep states as caller-defined lanes and findings structural | Medium | shipped report API |
| Process/sandbox/supervisor evidence | `PlatformEvidenceSummary`, profile verify APIs | host diagnostics, portable durability admission | Store-path/platform evidence is public | Generic process lifecycle evidence primitive | External/above-batpak owner; core owns only platform/store-path evidence | High | external owner |
| Audit assertion runner | rich test/harness surfaces in repo tooling | compliance policy pack, release audit gate | Assertions exist in integrity/xtask/tooling layers | Generic assertion DSL/executor API | Tooling/helper owner until repeated core need appears | Medium | tooling owner |
| Deterministic phase cache | existing deterministic tests/fixtures; no public phase cache API | simulation pipeline, deterministic build graph | Determinism discipline exists in tests/tooling | Generic phase cache contract | Tooling/consumer owner until a content-addressed phase executor exists | Medium | tooling/consumer owner |

Use this table as the intake filter for follow-on slices: prefer docs and report
shapes over new module trees, keep shipped substrate primitives named as
shipped, and assign non-core concepts to an explicit external or tooling owner
instead of leaving vague parked states.
