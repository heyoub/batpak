---
category: handoff
provenance: downstream
status: non-SOT extraction memo
classification: Handoff only. Does not change Downstream behavior, canonical ownership, Pack law, or batpak API.
definition: Self-contained upstream design-spec seed for the batpak owner/agent. Downstream references are traceability only; batpak work should be derivable from this file without reading the Downstream doc spine.
---

# BATPAK_EXTRACT.md - batpak upstream design-spec seed

This file is intentionally under `archive/`. It is **not** one of Downstream's eight canonical root docs and is not source-of-truth for Downstream behavior.

Use it as a design seed for batpak work: what should become domain-neutral black-box substrate physics, what already exists in batpak, what APIs/docs/tests a batpak coding agent should draft, and what must never be pushed down because it carries Downstream law.

batpak should grow the reusable black-box physics. Downstream should shrink to lawful composition. But every candidate below must first prove which kind of proof object it emits -- Event, Receipt, Report, Observation, Envelope, or Ref -- and must not import Pack-authored meaning into the generic mechanism.

**Self-contained rule:** a batpak agent should be able to read this file alone. Downstream file paths below are backlinks for the Downstream deletion pass, not prerequisites for batpak design. If a row cannot be understood without opening a Downstream doc, rewrite the row here rather than asking batpak to learn Downstream.

The project owner also owns batpak. That changes the social state but not the architectural state: no external permission is needed to accept a row into batpak, but Downstream docs still cannot shrink until the batpak API exists, ships, and is cited as a dependency contract. This file is therefore a design map for batpak and a deletion map for Downstream.

## Memo Promotion Lifecycle

This file is coordination memory, not undead doctrine. Its lifecycle is:

| State | Meaning | Required marker |
|---|---|---|
| Active handoff | Used to seed batpak issues / ADRs. | Current state until export happens. |
| Exported | Surviving rows copied into batpak issues / ADR stubs. | Record export date and issue / ADR ids in the export ledger below. |
| Superseded | batpak has accepted issues / ADRs for the surviving rows; batpak artifacts now drive design. | Update this memo to point at those batpak artifacts and stop treating local sketches as active guidance. |
| Retired | Downstream cites shipped batpak APIs and root owner sections have shrunk. | Cite the batpak version / docs and paired Downstream shrink PR. |
| Deleted or archive-deep | No active extraction work depends on the memo. | Keep only if provenance still matters; otherwise delete. |

Archive extraction memos may seed external repo work, but Downstream agents must not implement behavior from them unless a root MW owner cites the resulting shipped dependency.

## Boundary Covenant

batpak may get bigger. That is not a threat to Downstream. The boundary is not "small batpak, large Downstream"; the boundary is:

> batpak owns generic runtime mechanics. Downstream owns Pack-authored meaning.

Three categories govern every extraction:

| Category | Meaning | Examples |
|---|---|---|
| batpak-owned primitive | Generic and usable unchanged by a non-agent event-sourced Rust system | `VisibilityFence`, `AtLeastOnce`, `AppendReceipt`, `ProjectionRunner`, store/platform `TargetProfile`, `HashChainWalker`, `SchemaSnapshot` |
| batpak-provided mechanism, Downstream-owned meaning | batpak provides generic mechanics; Downstream composes them into Pack law | sandbox/process evidence primitives composed into Pack-emitted sandbox profiles; lifecycle/readiness evidence composed into `HostDeploymentTarget`; generic schema harness composed into Pack IR and protocol fixture checks |
| Downstream-owned semantic law | Depends on Pack-authored agent execution meaning and must not move down | Pack, Pack.lock, Mission, Session, Run, Capability, Criticality, Budget, Topic, Effect, Port, Operation, six-membrane admission meaning, protocol adapter law, host exposure policy, IMAGE, Capsule, human approval, settings authority chain |

The danger is not a more capable batpak. The danger is a generic batpak mechanism being documented as if it owns Downstream deployment law. Safe phrasing:

> batpak may record, verify, and in narrow store-owned cases enforce generic process/evidence boundaries. Downstream composes those primitives into Pack-emitted sandbox, deployment, and service-supervisor artifacts.

Concrete split:

- batpak may own service-supervisor evidence primitives if they are generic; Downstream owns service-supervisor artifact projection for Pack deployments.
- batpak may own sandbox/process boundary primitives if they are generic; Downstream owns Pack-emitted sandbox policy.
- batpak may own IPC/event delivery primitives if they are generic; Downstream owns substrate-mediated operations that cross those portals under Pack authority.
- batpak macros may derive events, receipts, gates, projections, canonical hashes, replay harnesses, frontier checks, and schema snapshots.
- Downstream macros compose those derives into Mission lifecycle, capability checks, six-membrane admission, Pack IR lowering, and Port/protocol projections.

Never introduce `batpak::Mission`, `batpak::Capability`, `batpak::Budget`, `batpak::PortKind::MCP`, or `batpak::PackLock`. Those are contamination markers.

Paranoia earns existence only if it blocks execution, proves evidence, enables replay, or fails a build/audit. Otherwise demote it to docs/projection.

## Scope Boundary

batpak may own generic event-sourced consequence mechanics. It must stay domain-neutral and agent-ignorant.

**Eligible for batpak:** durable event append, append receipts, receipt-chain mechanics, cursor/replay primitives, watermarks/frontiers, causal ordering, generic projection replay, generic resource-accounting observations, and generic consistency/snapshot harnesses.

**Not eligible for batpak:** Pack, Mission, Session, Run, Capability meaning, Criticality, Budget semantics, Port/adapter law, MCP/A2A/AG-UI/A2UI/WebMCP semantics, Pack.lock, IMAGE, Capsule, human approval law, protocol projection law, host authentication policy, or any behavior whose correctness depends on Downstream's six-membrane admission pipeline.

Extraction test for every row:

> Can this API serve a non-agent Rust system such as a compliance event store, robotics recorder, local-first sync engine, financial ledger, or simulation log without importing Downstream vocabulary?

If yes, it may be a batpak RFC. If no, it stays Downstream-owned.

## What batpak Is Being Asked To Become

This extraction is not asking batpak to become a thin append log. It is asking batpak to become the reusable black-box physics layer that many Rust systems can build over.

batpak should make these things boring:

- append-only consequence recording
- canonical identity for recorded facts
- typed events and receipts
- denial evidence
- cursor/replay mechanics
- visibility and durability frontiers
- projection rebuilds
- receipt-chain verification
- resource pressure observation
- lifecycle evidence
- schema and canonical-byte conformance tests
- signed artifact / registry evidence where it is generic
- target/platform evidence where it is not domain-specific

That is why the extraction guide is allowed to be larger than a simple TODO list. The goal is not only "move text out of Downstream." The goal is to describe the reusable batpak surface clearly enough that a batpak implementation agent can act without learning Downstream law.

## Design Seed Versus Final batpak Spec

This file is not the final batpak canonical spec, but it is intentionally large enough to seed one.

The batpak agent should treat this file as:

- a requirements packet
- a first RFC outline
- an API sketch source
- a test-plan source
- a docs-outline source
- a contamination filter

The batpak agent should not treat this file as:

- final Rust API names
- final module paths
- final error enum variants
- final serialization grammar
- final public compatibility promise
- Downstream behavior authority

The expected batpak-side result is not "copy this file into batpak." The expected result is:

**Illustrative sketch warning.** All code shapes, module trees, feature flags, and PR titles below are constraint sketches, not proposed public API. Before naming any type or module, inspect batpak's existing modules and prefer strengthening existing API over adding new surfaces. If an existing batpak name already carries the guarantee, document and test that name instead of copying a sketch from this memo.

```text
batpak/
  docs/
    adr/
      ADR-00xx-receipt-chain-walker.md
      ADR-00xx-schema-snapshot.md
      ADR-00xx-subscriber-frontier.md
      ...
    guides/
      black-box-physics.md
      audit-and-replay.md
      projection-runners.md
  src/
    store/
      audit/
      schema/
      subscription/
      projection/
      resource/
      artifact/
      registry/
      read/
      lifecycle/
      platform/
```

The actual module paths should follow batpak's repository conventions. The tree above is an orientation target, not a command.

## Success Criteria For This Extraction

The batpak extraction succeeds only when the batpak repo can answer these questions without reading Downstream:

1. How does a consumer verify receipt-chain continuity?
2. How does a consumer prove schema / fixture compatibility across releases?
3. How does a lossy subscriber know what it missed?
4. How does a projection rebuild from durable events without publishing partial state?
5. How does a runtime record resource pressure without deciding policy?
6. How does a signed artifact preserve stable body identity while adding signature evidence?
7. How does an attested registry row evolve without deleting prior evidence?
8. How does a read path report what it observed, and when is that report persisted?
9. How does a lifecycle state transition emit report/event evidence without domain lifecycle names?
10. How does a reservation survive crash/reconcile without importing app policy?
11. How does a target/process/sandbox/supervisor evidence primitive prove machine facts without owning deployment law?

If those questions require opening Downstream, this seed failed.

## Batpak Design Principles

All code shapes below are illustrative constraint sketches, not proposed public API. Before naming any type, inspect batpak's existing modules and prefer strengthening existing API over adding new surfaces.

### Mechanism, Not Meaning

batpak should expose mechanics that other systems compose into meaning.

Good:

```rust
batpak::ReceiptChainWalker
batpak::SchemaSnapshot
batpak::SubscriberFrontier
batpak::StoreResourceEnvelope
batpak::ReadWalkReport
```

Bad:

```rust
batpak::MissionReceipt
batpak::CapabilityDenied
batpak::BudgetExceeded
batpak::McpToolCall
batpak::HostDeploymentTarget
```

The first group can serve ledgers, robotics logs, simulation systems, local-first databases, audit trails, and agent substrates. The second group imports one application's law.

### Evidence Before Convenience

Every new batpak primitive should name the evidence it emits before naming the convenience API.

For example, do not start with:

```rust
fn rebuild_projection(...)
```

Start with:

```rust
struct ProjectionRunReport { ... }
```

Then design the runner that emits the report.

### Policy-Neutral Failures

batpak failures should explain what happened, not decide what the application should do.

Good:

```text
resource ceiling exceeded
receipt parent missing
subscriber lag over threshold
schema hash changed
artifact signature invalid
```

Bad:

```text
mission must suspend
budget tier must escalate
human approval required
port authority denied
```

### Canonical Bytes Are Product Surface

If an object may appear in a receipt, audit report, schema snapshot, registry row, artifact envelope, or replay checkpoint, its canonical encoding is part of the product surface.

That means every candidate below should answer:

- What exact fields are identity-bearing?
- What fields are diagnostic only?
- What fields may be omitted?
- Does adding a signature change body identity?
- Does report ordering depend on storage iteration order?
- Can two equivalent runs produce byte-identical reports?

### Public API Before Downstream Shrink

Downstream can shrink only after batpak exposes a public API or an explicit docs guarantee.

Private batpak internals are not enough. A private module can change without a consumer-facing migration contract. Downstream needs either:

- a public type
- a public function
- a public trait
- a documented guarantee
- an ADR-backed compatibility promise

## Proposed batpak Module Families

This is the first-pass target map for the batpak repo. Names are suggestions.

| Family | Proposed home | Owns | Does not own |
|---|---|---|---|
| Chain audit | `store::audit::chain` | receipt graph / hash / parent / cycle verification | application authority |
| Schema snapshots | `store::schema` or `testing::schema` | schema hashes, fixture hashes, drift reports | Pack IR, protocol fields |
| Subscriber frontier | `store::subscription` | lag, loss, consumed frontier, policy observation | ExtProfile, UI, user presence |
| Projection runner | `store::projection` | replay, checkpoint, deterministic rebuild report | emitted artifact meaning |
| Store resource envelope | `store::resource` | store-owned counters, ceilings, pressure observations | host pools, budget / criticality response |
| Artifact envelope | `artifact` or `store::artifact` | stable body hash + signature envelope | Pack.lock / IMAGE / Capsule |
| Attested registry | `registry` | signed rows, lifecycle, supersession, drift | Pack-author semantic registry |
| Read reports | `store::read` | as-of reads, walked regions, dropped count evidence | retrieval ranking / authorization |
| State transition reports/events | `store::lifecycle` | generic transition evidence | Mission/Run lifecycle |
| Reservation transition reports | optional `store::reservation` / helper crate | reserve/commit/refund/expire/orphan mechanics | budget policy |
| Target/process evidence | `store::platform` public evidence layer or `runtime::evidence` | machine facts, lifecycle observations | deployment law |
| Canonical encoding contract | `encoding` / `canonical` | byte stability and version guarantees | Pack.lock policy |

The batpak agent should first inspect existing modules and fit these families into the current style rather than creating parallel structures by reflex.

## Batpak Issue Export Step

The next real artifact should live in batpak, not in this Downstream archive file. Export the first batch as batpak issues or ADR stubs, then mark this memo `Exported` with issue / ADR ids.

Suggested export batch:

| Export row | Batpak issue / ADR id | Export status | Exported on | Paired Downstream shrink target |
|---|---|---|---|---|
| Deterministic receipt-chain evidence report | TODO | not exported | TODO | remove generic receipt walking prose from Downstream receipt-audit / macro audit sections after batpak ships |
| Canonical encoding contract + schema snapshots | TODO | not exported | TODO | replace generic canonical-byte / schema-drift tutorials with batpak citation after batpak ships |
| Subscriber frontier for lossy subscriptions | TODO | not exported | TODO | shrink slow-consumer / lag mechanics to ExtProfile / Highway meaning plus batpak citation after batpak ships |
| Projection runner / watcher audit report | TODO | not exported | TODO | delete generic replay-runner tutorial prose while retaining Pack projection meaning after batpak ships |

Each exported issue must include a non-agent consumer proof:

```text
Generic users:
- compliance event store
- robotics recorder
- local-first sync engine
- financial ledger
- simulation log
```

At least two examples must fit the proposed API without Downstream vocabulary. If fewer than two fit, do not push the mechanism down.

## Minimum Public API Shape

Each accepted family should expose four layers:

1. **Input request** — caller asks batpak to verify, run, observe, or record.
2. **Evidence report** — canonical, deterministic, auditable output.
3. **Error / diagnostic** — structured failure that does not decide app policy.
4. **Test harness helper** — helper that makes consumers prove the guarantee.

Example:

```rust
pub struct ReceiptWalkRequest { ... }
pub struct ReceiptWalkReport { ... }
pub enum ReceiptWalkError { ... }
pub fn walk_receipts(store: &Store, request: ReceiptWalkRequest) -> Result<ReceiptWalkReport, ReceiptWalkError>;
pub fn assert_receipt_chain_stable(report: &ReceiptWalkReport) -> Result<(), SnapshotError>;
```

Avoid APIs that return only booleans:

```rust
fn is_chain_valid(...) -> bool
```

That shape loses the evidence an auditor needs.

## Acceptance Test Families

Every candidate that reaches implementation should include tests in these families:

| Test family | Purpose |
|---|---|
| happy path | proves ordinary use works |
| corrupt input | proves structured evidence on bad state |
| deterministic bytes | proves report canonical encoding is stable |
| replay/restart | proves state survives close/reopen where relevant |
| partial failure | proves no half-visible checkpoint or half-committed report |
| non-policy | proves batpak reports facts without deciding app-specific consequences |
| docs example | proves public docs compile or execute |

For any new report type, add at least one golden canonical-byte fixture or equivalent snapshot test.

## Error Naming Rules

batpak error names should describe structural facts.

Good:

```rust
ReceiptParentMissing
ReceiptHashMismatch
ReceiptCycleDetected
SchemaHashMismatch
FixtureHashMismatch
SubscriberLagExceeded
ProjectionCheckpointIncomplete
ArtifactSignatureInvalid
RegistryRowUnsigned
ReservationOrphaned
TargetProfileUnsupported
```

Bad:

```rust
CapabilityDenied
MissionSuspended
BudgetEscalated
ProtocolAdapterRejected
HumanApprovalMissing
PackLockInvalid
```

If an error name mentions a user, agent, mission, capability, protocol adapter, host deployment target, budget tier, or Pack artifact, it probably belongs above batpak.

## Versioning Rules For Extracted APIs

batpak should version extracted APIs by guarantee class:

| Guarantee | Compatibility expectation |
|---|---|
| report canonical bytes | patch-stable unless explicitly versioned |
| request field additions | additive when defaultable |
| enum variant additions | breaking unless marked non-exhaustive |
| error shape changes | breaking if consumers match on variants |
| docs guarantee text | patch-stable once cited by Downstream |
| private helper changes | internal unless exposed in docs |

Downstream cannot cite an unstable private detail. If batpak wants freedom to change a candidate, mark it experimental and keep Downstream shrink blocked.

## Feature Flag Guidance

Some surfaces may be heavy. batpak can gate them, but the evidence types should remain coherent.

Suggested flags:

```toml
[features]
audit-chain = []
schema-snapshot = []
subscriber-frontier = []
projection-runner = []
store-resource-envelope = []
artifact-envelope = ["signature"]
attested-registry = ["artifact-envelope"]
read-reports = []
reservation-ledger = []
runtime-evidence = []
```

Feature flags must not create two semantic versions of the same evidence. Disabled feature = absent API or `Unsupported`, never degraded evidence.

## Local Glossary For batpak Work

These names are suggested generic batpak vocabulary. They are not Downstream primitive claims.

### Proof Object Taxonomy

Use these words precisely. If everything is called a receipt, receipt stops meaning black-box proof.

| Proof object | Meaning | Durable by default? |
|---|---|---|
| `Event` | Durable fact appended to the store. | Yes. |
| `AppendReceipt` | Proof that an append committed and where it landed. | Yes, as append evidence. |
| `DenialReceipt` | Proof that a generic gate/invariant rejected a proposed action. | Yes, when recorded as denial evidence. |
| `EvidenceReport` | Deterministic output of a verifier, audit, read, projection, or comparison run. | No, unless the caller appends it. |
| `ObservationRecord` | Runtime fact such as lag, pressure, lifecycle, or skew. | No, unless the caller appends it. |
| `Envelope` | Canonical body plus metadata, signatures, or attestations. | Depends on where stored. |
| `Ref` | Pointer to prior evidence. | No authority by itself. |

Candidate outputs should default to reports/observations unless they actually append an event and receive an `AppendReceipt`.

| Candidate | Primary output |
|---|---|
| `ReceiptChainWalker` | `EvidenceReport` |
| `SchemaSnapshot` | `EvidenceReport` |
| `SubscriberFrontier` / `LagPolicy` | `ObservationRecord` or `EvidenceReport` |
| `ProjectionRunner` | `EvidenceReport` plus optional checkpoint `Event` |
| `StoreResourceEnvelope` | `ObservationRecord` or `EvidenceReport` |
| `ReadWalk` | `EvidenceReport`; optional `ReadObserved` event if persisted |
| `StateTransition` | `Event` plus `AppendReceipt`, or `StateTransitionReport` before append |
| `Compaction` | `CompactionReport` by default; `CompactionReceipt` only if a compaction event appends |
| `ReservationLedger` | `ReservationTransitionReport` by default; receipt only for appended reserve/commit/refund events |
| `CanonicalArtifactEnvelope` | `Envelope` |
| `AttestedRegistry` | External signed row or registry event, depending implementation |

Identity-bearing names inside these objects should be stable IDs, not display strings. Plain `String` in sketches means "stable-id string or local newtype," not user-facing prose.

| Term | Generic meaning | Must not imply |
|---|---|---|
| `Event` | Durable fact appended to a store under a coordinate / region. | Mission, Run, Effect, or any agent lifecycle. |
| `Receipt` | Evidence that an append, denial, projection, read, lifecycle transition, or verification action happened. | Downstream admission approval. |
| `AppendReceipt` | Evidence for a successful append, including identity, ordering, and hash material. | Permission to perform the action. |
| `DenialReceipt` | Evidence that a requested append/action was rejected by a generic gate or invariant. | Downstream denial taxonomy. |
| `Gate` | Generic predicate / admission point that returns pass, deny, defer, or diagnostic evidence. | Downstream's six-membrane law. |
| `Pipeline` | Ordered composition of generic gates or stages with proof of traversal. | Downstream Pack Pipeline or admission pipeline. |
| `Projection` | Deterministic view derived from durable facts. | Protocol, UI, skill, SBOM, or Pack projection meaning. |
| `Frontier` | A named point in write/apply/visible/durable/replay progress. | A user-visible Downstream success state. |
| `VisibilityFence` | A guarantee that selected work is visible before a continuation proceeds. | Pack-level recovery or approval. |
| `Cursor` | Replay / subscription position with restart discipline. | ExtProfile context position or Mission Control state. |
| `AtLeastOnce` | Evidence that delivery may repeat and consumers must observe idempotently. | Operation semantics or compensation law. |
| `TargetProfile` | Machine / storage / runtime fact profile used by generic batpak guarantees. | Host deployment target or Pack compatibility promise. |
| `ArtifactEnvelope` | Canonical body plus signature / attestation metadata. | Pack.lock, IMAGE, or Capsule. |
| `AttestedRegistry` | Signed immutable registry entry set for generic artifacts. | Pack-author registries or protocol behavior admission. |

## Extraction Classes

Use these classes in batpak issues / PR descriptions. They are intentionally stricter than "nice to have."

| Class | Meaning | Default action |
|---|---|---|
| `READY_RFC` | Clean non-agent primitive. The API shape is plausible now. | Write a batpak RFC and minimal tests. |
| `SPLIT` | Generic mechanism belongs in batpak; policy meaning stays above. | Extract only the mechanism and add explicit non-goals. |
| `CANDIDATE` | Likely reusable but needs implementation pressure. | Park behind an issue and do not invent names prematurely. |
| `MOSTLY_DONE` | batpak already owns the important primitive. | Document, expose, or test the existing API instead of rebuilding. |
| `UPSTREAM_STRENGTHEN` | Existing primitive needs stronger guarantee language or tests. | Add conformance tests / docs before new APIs. |
| `DEFER` | Plausible but too easy to import Downstream law today. | Leave out until multiple non-agent users need it. |
| `DO_NOT_IMPORT_PACK_LAW` | Agent / Pack meaning. | Keep out of batpak. |

## RFC Card Template

Every batpak extraction RFC should fit this card. If it cannot, the candidate is probably too Downstream-shaped.

```text
Name:
Class:
Problem:
Generic users:
Core types:
Required evidence:
Ordering/frontier semantics:
Failure semantics:
Canonical encoding impact:
Replay/projection impact:
Minimum tests:
Non-goals:
Downstream shrink trigger:
```

The most important line is `Generic users`. A good answer is "financial ledger, robotics recorder, compliance event store, local-first sync engine." A bad answer is "Downstream needs this for Missions."

## High-Priority RFC Cards

These cards are the first extraction batch. They are intentionally written without requiring Downstream docs.

## Implementation Detail: Cross-Cutting Types

These type sketches intentionally use generic names. A batpak agent should adapt them to existing batpak primitives where they already exist.

### Identity References

```rust
pub struct ReceiptRef {
    pub coordinate: Coordinate,
    pub receipt_hash: CanonicalHash,
    pub hlc: HlcPoint,
}

pub struct NamedHash {
    pub name: String,
    pub hash: CanonicalHash,
}

pub struct FrontierPoint {
    pub hlc: HlcPoint,
    pub sequence: u64,
    pub watermark: WatermarkKind,
}
```

Rules:

- `ReceiptRef` is a pointer, not authority.
- `NamedHash` is diagnostic unless the containing type declares it identity-bearing.
- `FrontierPoint` must say which watermark it refers to.

### Evidence Reports

Reports should split deterministic body identity from envelope metadata. Do not hash wall-clock-only metadata into the body identity unless the report explicitly declares time-bound identity.

```rust
pub struct ReportBodyHeader {
    pub report_kind: String,
    pub input_hash: CanonicalHash,
}

pub struct EvidenceEnvelope {
    pub body_hash: CanonicalHash,
    pub envelope_hash: CanonicalHash,
    pub generated_at: HlcPoint,
    pub batpak_version: String,
    pub signatures: Vec<SignatureEnvelope>,
    pub diagnostics: Vec<NamedHash>,
}
```

Rules:

- `body_hash` hashes the canonical report body, not wall-clock-only metadata.
- `envelope_hash` includes envelope metadata when the report needs identity for the sealed envelope.
- `generated_at` is evidence, not identity, unless the report kind explicitly says otherwise.
- `input_hash` binds the report to the request that produced it.
- `report_kind`, edge labels, lifecycle kinds, registry row kinds, policy ids, subject ids, and phase ids should be stable IDs or newtypes over stable IDs, not display strings.

### Canonical Report Ordering

Any report containing arrays must define ordering:

- hashes sort lexicographically by canonical bytes
- coordinates sort by canonical coordinate bytes
- HLC points sort by physical then logical component
- equal keys must use a stable secondary key
- storage iteration order is never allowed to leak into report order

### Unsupported Versus Denied

batpak should distinguish:

```rust
pub enum MechanismStatus {
    Supported,
    Unsupported { reason: String },
    DisabledByFeature { feature: String },
}
```

This is not application denial. It means batpak cannot provide the requested mechanism on this target/build.

## Implementation Detail: Chain Audit

`ReceiptChainWalker` should be the first high-confidence extraction because it converts existing receipt/hash material into reusable audit evidence.

### Required Capabilities

The chain walker should support at least three modes:

| Mode | Checks |
|---|---|
| `Linear` | append order continuity, expected previous hash, no missing receipts |
| `ParentLinks` | declared parent/cause links exist and content hashes match |
| `FullReachability` | linear + parent links + fork/cycle detection |

The walker should not interpret link meaning. A link named `caused_by`, `parent`, `compacted_from`, or `read_from` is just a named edge unless the caller provides app-specific semantics above batpak.

### Suggested Types

```rust
pub enum ReceiptWalkMode {
    Linear,
    ParentLinks,
    FullReachability,
}

pub struct ReceiptEdge {
    pub from: ReceiptRef,
    pub to: ReceiptRef,
    pub label: String,
}

pub enum ReceiptWalkFinding {
    MissingReceipt { reference: ReceiptRef },
    HashMismatch { reference: ReceiptRef, expected: CanonicalHash, observed: CanonicalHash },
    CycleDetected { cycle: Vec<ReceiptRef> },
    UndeclaredFork { at: ReceiptRef, branches: Vec<ReceiptRef> },
    OrderingRegression { previous: ReceiptRef, next: ReceiptRef },
}

pub struct ReceiptWalkReport {
    pub envelope: EvidenceEnvelope,
    pub mode: ReceiptWalkMode,
    pub checked_count: u64,
    pub edge_count: u64,
    pub first: Option<ReceiptRef>,
    pub last: Option<ReceiptRef>,
    pub root_hash: CanonicalHash,
    pub findings: Vec<ReceiptWalkFinding>,
}
```

### API Sketch

```rust
pub fn walk_receipt_chain(
    store: &Store,
    request: ReceiptWalkRequest,
) -> Result<ReceiptWalkReport, ReceiptWalkError>;
```

### Acceptance Tests

- empty range returns checked_count = 0 and deterministic report
- valid linear range has no findings
- missing receipt creates `MissingReceipt`
- hash mismatch creates `HashMismatch`
- parent-link cycle creates `CycleDetected`
- branch accepted only when declared in request policy
- report byte identity stable across two runs

### Documentation Page

Draft docs page: `docs/guides/receipt-chain-walking.md`.

Sections:

- What chain walking proves
- What it does not prove
- Linear vs parent-link vs full reachability
- How to snapshot reports
- How applications layer meaning on links

## Implementation Detail: Schema Snapshots

`SchemaSnapshot` is a conformance harness, not a general schema system. Its job is to answer: "Did this type's durable shape or golden bytes drift?"

It proves durable shape and fixture bytes. It does not classify fields as authority-bearing, privacy-sensitive, context, freshness, provenance, or side-effectful. Those semantic classes belong to Downstream / protocol-registry layers above batpak.

### Required Capabilities

The harness should compare:

- stable type id
- schema hash
- golden fixture hash
- encoder version
- optional lifecycle state
- optional compatibility classification

Compatibility classification should be explicit:

```rust
pub enum SchemaChangeKind {
    Unchanged,
    Additive,
    Breaking,
    Unknown,
}
```

Do not auto-classify unknown changes as additive.

### Suggested Types

```rust
pub struct SchemaSnapshot {
    pub stable_id: String,
    pub schema_hash: CanonicalHash,
    pub fixture_hash: CanonicalHash,
    pub encoder_version: String,
    pub lifecycle: SchemaLifecycle,
}

pub enum SchemaLifecycle {
    Live,
    Announced,
    Deprecated,
    Removed,
}

pub struct SchemaSnapshotReport {
    pub envelope: EvidenceEnvelope,
    pub stable_id: String,
    pub change_kind: SchemaChangeKind,
    pub expected_schema_hash: CanonicalHash,
    pub observed_schema_hash: CanonicalHash,
    pub expected_fixture_hash: CanonicalHash,
    pub observed_fixture_hash: CanonicalHash,
    pub field_findings: Vec<SchemaFieldFinding>,
}
```

### API Sketch

```rust
pub trait SchemaSnapshotSource {
    fn stable_id() -> &'static str;
    fn schema_bytes() -> Vec<u8>;
    fn fixture_bytes() -> Vec<u8>;
}

pub fn compare_schema_snapshot<T: SchemaSnapshotSource>(
    expected: &SchemaSnapshot,
) -> Result<SchemaSnapshotReport, SchemaSnapshotError>;
```

### Acceptance Tests

- unchanged schema reports `Unchanged`
- fixture-only drift is detected separately from schema drift
- removed field is `Breaking`
- renamed field is `Breaking`
- additive field requires explicit additive classification
- unknown field-path difference is `Unknown`, not accepted
- report byte identity stable

### Documentation Page

Draft docs page: `docs/guides/schema-snapshots.md`.

Sections:

- What schema snapshots are for
- Stable type ids
- Golden fixture bytes
- Additive versus breaking changes
- How to use snapshots in CI

## Implementation Detail: Subscriber Frontier

`SubscriberFrontier` should make lossy consumption observable. It is not a UI subscription system and not a context protocol.

### Required Capabilities

batpak should expose:

- consumed frontier
- available frontier
- lag in events
- lag in HLC physical time where meaningful
- last observed loss
- policy action taken
- restart behavior

### Suggested Types

```rust
pub struct SubscriberId(pub String);

pub struct SubscriberFrontier {
    pub subscriber_id: SubscriberId,
    pub consumed: FrontierPoint,
    pub available: FrontierPoint,
    pub lag_events: u64,
    pub lag_hlc_ms: Option<u64>,
    pub last_loss: Option<LossObservation>,
    pub policy: LagPolicy,
}

pub enum LagPolicy {
    ObserveOnly,
    DropOldestAt { lag_events: u64 },
    DropNewestAt { lag_events: u64 },
    BackpressureAt { lag_events: u64 },
    DisconnectAt { lag_events: u64 },
}

pub struct LossObservation {
    pub lost_from: FrontierPoint,
    pub lost_to: FrontierPoint,
    pub count: u64,
    pub observed_at: HlcPoint,
}

pub struct SubscriberLagReport {
    pub envelope: EvidenceEnvelope,
    pub frontier: SubscriberFrontier,
    pub action: Option<LagAction>,
}
```

### API Sketch

```rust
pub fn subscriber_frontier(
    store: &Store,
    subscriber_id: &SubscriberId,
) -> Result<SubscriberFrontier, SubscriberFrontierError>;

pub fn subscribe_lossy_with_policy<T>(
    store: &Store,
    region: Region,
    policy: LagPolicy,
) -> Result<Subscription<T>, SubscribeError>;
```

### Acceptance Tests

- no-lag subscriber reports zero lag
- lag below threshold reports no action
- lag above threshold reports configured action
- lossy drop records exact lost range
- restart preserves consumed frontier when checkpointed
- disconnected subscriber can query final frontier
- policy report bytes stable

### Documentation Page

Draft docs page: `docs/guides/subscriber-frontiers.md`.

Sections:

- Lossy subscription semantics
- Lag measurement
- Backpressure versus dropping
- Restart behavior
- How consumers layer app-specific policy

## Implementation Detail: Projection Runner

batpak already has projection-related surfaces. The extraction question is whether to expose a clearer runner/report contract.

### Required Capabilities

A public projection runner should provide:

- descriptor of source regions
- replay mode
- input frontier
- output checkpoint
- drift findings
- no half-visible checkpoint
- deterministic report

### Suggested Types

```rust
pub enum ReplayMode {
    Current,
    AsOf(FrontierPoint),
    TimeIndexed { from: HlcPoint, to: HlcPoint },
}

pub enum ProjectionValidity {
    Always,
    UntilFrontier(FrontierPoint),
    Predicate(String),
}

pub struct ProjectionDescriptor {
    pub projection_id: String,
    pub source_regions: Vec<Region>,
    pub output_kind: String,
    pub replay_mode: ReplayMode,
    pub validity: ProjectionValidity,
}

pub struct ProjectionRunReport {
    pub envelope: EvidenceEnvelope,
    pub descriptor_hash: CanonicalHash,
    pub input_frontier: FrontierPoint,
    pub output_frontier: FrontierPoint,
    pub checkpoint: CheckpointId,
    pub applied_count: u64,
    pub drift: Vec<ProjectionDrift>,
}
```

### API Sketch

```rust
pub trait ProjectionApply {
    type Event;
    type Output;

    fn apply(&mut self, event: Self::Event) -> Result<(), ProjectionApplyError>;
    fn checkpoint(&self) -> Result<ProjectionCheckpoint, ProjectionApplyError>;
}

pub fn run_projection<P: ProjectionApply>(
    store: &Store,
    descriptor: ProjectionDescriptor,
    projection: P,
) -> Result<ProjectionRunReport, ProjectionRunError>;
```

### Acceptance Tests

- rebuild from empty store
- resume from prior checkpoint
- crash before checkpoint does not publish visible output
- crash after checkpoint resumes from checkpoint
- drift report deterministic
- as-of replay does not observe later facts

### Documentation Page

Draft docs page: `docs/guides/projection-runners.md`.

Sections:

- Watcher versus runner
- Checkpoint visibility
- Drift reports
- As-of replay
- Consumer-owned projection meaning

## Implementation Detail: Store Resource Envelope

`StoreResourceEnvelope` should report store-owned resource facts without deciding consequences.

Split store resources from host resources:

- `StoreResourceEnvelope` candidates: writer queue pressure, segment bytes, index size, per-region/entity byte counts when supported, cursor lag, subscriber backlog, projection-runner memory/queue pressure if batpak owns the runner.
- host/application resource observations stay above batpak unless batpak owns the layer: HTTP connections, WebSocket buffers, adapter request limits, per-port network rates, DPoP nonce pressure, auth/session ticket pressure.

### Required Capabilities

Resource observation should cover:

- writer queue pressure
- per-region/entity bytes where available
- in-flight operation counts where generic
- subscriber backlog counts
- connection/message/buffer counts only if batpak owns that layer; otherwise leave them to the host/application layer
- ceiling comparisons
- recovery below ceiling

### Suggested Types

```rust
pub enum ResourceKind {
    Bytes,
    QueueDepth,
    InFlight,
    Connections,
    Messages,
    Buffers,
    Timeouts,
}

pub struct ResourceSubject {
    pub kind: String,
    pub coordinate: Option<Coordinate>,
    pub region: Option<Region>,
}

pub struct ResourceCounter {
    pub kind: ResourceKind,
    pub value: u64,
}

pub struct ResourceCeiling {
    pub kind: ResourceKind,
    pub limit: u64,
}

pub struct StoreResourceEnvelope {
    pub envelope: EvidenceEnvelope,
    pub subject: ResourceSubject,
    pub counters: Vec<ResourceCounter>,
    pub ceilings: Vec<ResourceCeiling>,
    pub breaches: Vec<ResourceBreach>,
}
```

### API Sketch

```rust
pub fn resource_envelope(
    store: &Store,
    subject: ResourceSubject,
) -> Result<StoreResourceEnvelope, StoreResourceEnvelopeError>;
```

### Acceptance Tests

- under-ceiling report
- at-ceiling report
- over-ceiling breach
- recovery below ceiling
- repeated breach recurrence
- report has no app-policy action

### Documentation Page

Draft docs page: `docs/guides/resource-envelopes.md`.

Sections:

- What resource envelopes observe
- What they do not decide
- Counter kinds
- Ceiling comparisons
- Replay and audit use

### `ReceiptChainWalker`

Class: `READY_RFC`.

Problem: batpak users need a standard way to verify receipt continuity, parent references, content hashes, segment links, and declared forks without each project writing its own audit walker.

Generic users: ledgers, local-first sync logs, append-only compliance records, robotics telemetry, simulation recorders.

Core types:

```rust
struct ReceiptWalkRequest {
    start: ReceiptRef,
    end: Option<ReceiptRef>,
    mode: WalkMode,
}

enum WalkMode {
    Linear,
    ParentLinks,
    FullReachability,
}

struct ReceiptWalkReport {
    checked_count: u64,
    first: ReceiptRef,
    last: ReceiptRef,
    root_hash: CanonicalHash,
    gaps: Vec<ReceiptGap>,
    forks: Vec<ReceiptFork>,
    cycles: Vec<ReceiptCycle>,
}
```

Required evidence: each checked receipt identity, parent link, content hash, HLC/order point, and detected gap/fork/cycle.

Ordering/frontier semantics: the walker must say whether it checked append order, HLC order, parent-link reachability, or all three.

Failure semantics: report evidence; do not panic on corrupt stores unless the caller requests fail-fast.

Canonical encoding impact: report output must have stable canonical bytes for audit snapshots.

Replay/projection impact: projection rebuild tools can call the walker before trusting a range.

Minimum tests:

- valid linear chain
- missing parent
- hash mismatch
- cycle
- declared branch accepted
- undeclared branch reported
- deterministic report bytes

Non-goals: authority, human approval, admission law, protocol semantics.

Downstream shrink trigger: once this exists, Downstream should stop explaining generic receipt walking and keep only its receipt fields and denial/authority extensions.

### `SchemaSnapshot`

Class: `READY_RFC`.

Problem: projects need stable tests that a typed event/receipt/projection shape still encodes to the same canonical schema and golden bytes across releases.

Generic users: any batpak consumer with durable data compatibility obligations.

Core types:

```rust
struct SchemaSnapshot {
    stable_id: String,
    schema_hash: CanonicalHash,
    fixture_hash: CanonicalHash,
    encoder_version: String,
}

struct SchemaSnapshotReport {
    stable_id: String,
    status: SnapshotStatus,
    expected_schema_hash: CanonicalHash,
    observed_schema_hash: CanonicalHash,
    expected_fixture_hash: CanonicalHash,
    observed_fixture_hash: CanonicalHash,
}
```

Required evidence: schema hash, fixture hash, encoder version, and stable type id.

Ordering/frontier semantics: none except deterministic test ordering.

Failure semantics: diff reports should identify which field path changed, whether the change is additive, breaking, or unknown.

Canonical encoding impact: this is a canonical encoding contract test.

Replay/projection impact: consumers can refuse replay across unknown schema drift.

Minimum tests:

- unchanged schema passes
- renamed field fails
- dropped field fails
- additive field classification is explicit
- fixture bytes drift fails
- report bytes deterministic

Non-goals: Pack IR schemas, protocol registry semantics, authoring UX.

Downstream shrink trigger: once this exists, Downstream can cite batpak for generic schema/golden-byte testing and retain only which Downstream types and protocol fixtures are pinned.

### `SubscriberFrontier` / `LagPolicy`

Class: `READY_RFC`.

Problem: lossy subscribers need explicit evidence about what they consumed, what they skipped, how far behind they are, and whether backpressure/loss policy fired.

Generic users: telemetry subscribers, UI projections, cache invalidators, background indexers, sync tails.

Core types:

```rust
struct SubscriberFrontier {
    subscriber_id: String,
    consumed: FrontierPoint,
    visible: FrontierPoint,
    durable: FrontierPoint,
    lag_events: u64,
    last_loss: Option<LossObservation>,
}

enum LagPolicy {
    DropOldest { max_lag: u64 },
    DropNewest { max_lag: u64 },
    Backpressure { max_lag: u64 },
    Disconnect { max_lag: u64 },
}
```

Required evidence: consumed frontier, available frontier, lag amount, loss marker, policy used.

Ordering/frontier semantics: must name which frontier lag is measured against.

Failure semantics: policy action must be observable as a receipt or structured observation.

Canonical encoding impact: frontier observations should be snapshot-testable.

Replay/projection impact: replay consumers can resume with knowledge of what was skipped.

Minimum tests:

- no lag
- lag below threshold
- lag crossing threshold
- lossy drop observed
- backpressure observed
- disconnect observed
- restart preserves consumed frontier

Non-goals: ExtProfile, user presence, Mission Control, protocol subscription semantics.

Downstream shrink trigger: once this exists, Downstream can stop carrying slow-consumer mechanics and keep only ExtProfile/Highway meaning.

### `ProjectionRunner`

Class: `CANDIDATE`, possibly `MOSTLY_DONE`.

Problem: batpak already has projection watching/cache concepts. The open question is whether consumers need a more explicit runner that owns replay, checkpointing, deterministic rebuilds, and drift reports.

Projection has three different altitudes:

- batpak projection: fold events into a deterministic view.
- Downstream projection: emit ecosystem-shaped artifacts such as MCP, A2A, AG-UI, OTel, SBOM, docs, and audit surfaces.
- protocol projection: render byte-for-byte protocol fixtures or descriptors.

`ProjectionRunner` may only own the first altitude. It must not emit protocol descriptors, skill formats, OTel spans, SBOMs, or Downstream docs.

Generic users: read-model builders, cache maintainers, indexers, materialized views.

Core types:

```rust
struct ProjectionDescriptor {
    projection_id: String,
    source_regions: Vec<Region>,
    output_kind: String,
    replay_mode: ReplayMode,
    validity: ProjectionValidity,
}

struct ProjectionRunReport {
    projection_id: String,
    input_frontier: FrontierPoint,
    output_frontier: FrontierPoint,
    checkpoint: CheckpointId,
    applied_count: u64,
    drift: Vec<ProjectionDrift>,
}
```

Required evidence: input range, output checkpoint, applied count, drift observations.

Ordering/frontier semantics: replay must state as-of, current, or time-indexed semantics.

Failure semantics: partial internal work must not publish a visible checkpoint.

Canonical encoding impact: descriptors and reports should have stable bytes.

Replay/projection impact: this is the generic projection rebuild surface.

Minimum tests:

- rebuild from empty
- resume from checkpoint
- crash before visible checkpoint
- drift detected
- deterministic output report

Non-goals: protocol projections, UI surfaces, skills, OpenTelemetry, SBOM.

Downstream shrink trigger: once this is public, Downstream docs keep projection meanings but delete generic runner tutorials.

### `StoreResourceEnvelope`

Class: `SPLIT`.

Problem: users need generic evidence for bytes, counts, queue pressure, connection pressure, and over-ceiling observations without importing application policy.

Generic users: any long-running store or projection system with bounded memory / I/O / queue capacity.

Core types:

```rust
struct StoreResourceEnvelope {
    subject: ResourceSubject,
    observed_at: HlcPoint,
    counters: Vec<ResourceCounter>,
    ceilings: Vec<ResourceCeiling>,
    over_ceiling: Vec<ResourceBreach>,
}
```

Required evidence: subject, observed counter, ceiling, breach, and recurrence state.

Ordering/frontier semantics: resource observations must be ordered enough to correlate with append/replay pressure.

Failure semantics: batpak reports pressure; callers decide whether pressure denies, degrades, retries, or escalates.

Canonical encoding impact: reports should be compact and stable.

Replay/projection impact: resource observations may be replayed as operational evidence but must not mutate domain facts.

Minimum tests:

- under ceiling
- at ceiling
- over ceiling
- repeated breach
- recovery below ceiling
- deterministic report bytes

Non-goals: Budget tiers, Criticality, operator roles, Pack-authored ceilings.

Downstream shrink trigger: Downstream can cite this for resource observation and keep only policy consequences.

### `CanonicalArtifactEnvelope`

Class: `CANDIDATE`, feature-gated, only after the canonical encoding contract is stable enough to cite.

Problem: many systems need a deterministic artifact body hash plus signature/attestation metadata that does not perturb the identity hash.

Generic users: build artifacts, registry entries, schema bundles, replay bundles, export manifests.

Core types:

```rust
struct CanonicalArtifactEnvelope<T> {
    body: T,
    body_hash: CanonicalHash,
    envelope_hash: CanonicalHash,
    signatures: Vec<SignatureEnvelope>,
    attestations: Vec<AttestationRef>,
}
```

Required evidence: body hash, envelope hash, signer id, signer epoch or key epoch, signature algorithm, verification result.

Ordering/frontier semantics: optional; relevant only when the artifact enters a receipt stream.

Failure semantics: verification failure returns structured evidence.

Canonical encoding impact: signature metadata must not alter body hash.

Replay/projection impact: replay can import artifact evidence by stable body hash.

Minimum tests:

- body hash stable with added signature
- envelope hash changes with added signature
- invalid signature report
- signer epoch report
- deterministic verification report

Non-goals: Pack.lock, IMAGE, Capsule, protocol pin semantics.

Downstream shrink trigger: Downstream can stop hand-explaining generic signed envelopes and keep Pack-specific identity law.

## Implementation Detail: Canonical Artifact Envelope

`CanonicalArtifactEnvelope` should solve one narrow problem: stable body identity with external verification metadata.

### Required Capabilities

The envelope should support:

- canonical body bytes
- stable body hash
- envelope hash including metadata
- one or more signatures
- optional attestations
- verification report
- signer/key epoch metadata
- deterministic failure evidence

### Suggested Types

```rust
pub struct CanonicalArtifactEnvelope<T> {
    pub body: T,
    pub body_hash: CanonicalHash,
    pub envelope_hash: CanonicalHash,
    pub signatures: Vec<SignatureEnvelope>,
    pub attestations: Vec<AttestationRef>,
}

pub struct SignatureEnvelope {
    pub signer_id: String,
    pub key_epoch: String,
    pub algorithm: String,
    pub signature: Vec<u8>,
    pub signed_body_hash: CanonicalHash,
}

pub struct ArtifactVerificationReport {
    pub envelope: EvidenceEnvelope,
    pub body_hash: CanonicalHash,
    pub envelope_hash: CanonicalHash,
    pub signature_results: Vec<SignatureVerificationResult>,
}
```

### Identity Rules

- `body_hash` is computed only over the canonical body.
- Adding a signature must not change `body_hash`.
- Adding a signature must change `envelope_hash`.
- Verification reports are evidence, not artifact identity.
- Attestation refs must be hashed by canonical ref body, not display URI.

### API Sketch

```rust
pub fn seal_artifact<T: CanonicalEncode>(
    body: T,
    signatures: Vec<SignatureEnvelope>,
) -> Result<CanonicalArtifactEnvelope<T>, ArtifactEnvelopeError>;

pub fn verify_artifact<T: CanonicalEncode>(
    envelope: &CanonicalArtifactEnvelope<T>,
    verifier: &dyn SignatureVerifier,
) -> Result<ArtifactVerificationReport, ArtifactEnvelopeError>;
```

### Acceptance Tests

- same body, no signatures => same body hash
- same body, added signature => same body hash, different envelope hash
- invalid signature produces structured report
- signer epoch appears in report
- report order stable with multiple signatures
- unsupported algorithm fails closed

### Documentation Page

Draft docs page: `docs/guides/canonical-artifact-envelopes.md`.

Sections:

- Body identity versus envelope identity
- Signature metadata
- Verification reports
- Stable hashes
- How applications layer domain meaning

### `AttestedRegistry`

Class: `CANDIDATE`.

Problem: durable systems often need immutable registry rows with stable ids, artifact hashes, signer epochs, lifecycle status, deprecation/supersession links, and drift checks.

Generic users: schema registries, plugin registries, projection registries, fixture registries.

Core types:

```rust
struct AttestedRegistryRow {
    stable_id: String,
    kind: String,
    version: String,
    artifact_hashes: Vec<NamedHash>,
    signer_id: String,
    signer_epoch: String,
    lifecycle: RegistryLifecycle,
    supersedes: Option<String>,
    upstream: Option<UpstreamRef>,
}
```

Required evidence: row hash, signer, lifecycle state, supersession chain, drift report.

Ordering/frontier semantics: registry updates are append-only; supersession does not erase prior rows.

Failure semantics: unknown, unsigned, expired, or drifted rows fail closed unless caller opts into advisory mode.

Canonical encoding impact: row canonical bytes are the registry identity.

Replay/projection impact: replay can bind an action to the registry row that existed at the time.

Minimum tests:

- signed row verifies
- unsigned row rejected
- supersession chain valid
- drift detected
- lifecycle transition valid
- deterministic row hash

Non-goals: Pack-author transforms, protocol field semantics, admission behavior.

Downstream shrink trigger: Downstream can cite this for generic registry attestation and keep protocol/Pack semantic mapping above.

## Implementation Detail: Attested Registry

`AttestedRegistry` should provide append-only signed rows for generic artifact registries. It should not know what a row means to an application.

Keep the split strict:

- batpak `AttestedRegistry`: stable id, row kind, version, artifact hashes, signer, lifecycle, supersession, row hash, verification report.
- protocol/application registry: MCP/A2A/ACP/ExtProfile semantics, semantic field classes, drop policy, normalization profile, adapter mapping, conformance levels, watcher bots, automated PR generation.

batpak may carry a generic `upstream_ref` field for evidence. It should not own watcher automation as core behavior.

### Required Capabilities

The registry should support:

- stable row id
- row kind
- version
- artifact hashes
- signer id
- signer epoch
- lifecycle state
- supersession chain
- upstream reference metadata
- drift reports

### Suggested Types

```rust
pub enum RegistryLifecycle {
    Live,
    Announced,
    Deprecated,
    Removed,
}

pub struct UpstreamRef {
    pub source_kind: String,
    pub source_ref: String,
    pub last_observed_hash: Option<CanonicalHash>,
}

pub struct AttestedRegistryRow {
    pub stable_id: String,
    pub kind: String,
    pub version: String,
    pub artifact_hashes: Vec<NamedHash>,
    pub signer_id: String,
    pub signer_epoch: String,
    pub lifecycle: RegistryLifecycle,
    pub supersedes: Option<String>,
    pub upstream: Option<UpstreamRef>,
}

pub struct RegistryDriftReport {
    pub envelope: EvidenceEnvelope,
    pub stable_id: String,
    pub expected_row_hash: CanonicalHash,
    pub observed_row_hash: CanonicalHash,
    pub drift_fields: Vec<String>,
}
```

### API Sketch

```rust
pub fn verify_registry_row(
    row: &AttestedRegistryRow,
    verifier: &dyn SignatureVerifier,
) -> Result<RegistryRowVerificationReport, RegistryError>;

pub fn compare_registry_row(
    expected: &AttestedRegistryRow,
    observed: &AttestedRegistryRow,
) -> Result<RegistryDriftReport, RegistryError>;
```

### Lifecycle Rules

- `Live` rows may be selected by default.
- `Announced` rows may be visible but not default-selected unless caller opts in.
- `Deprecated` rows remain valid for prior pins but should not be selected for new pins by default.
- `Removed` rows remain audit-visible; removal does not delete the row.
- `supersedes` points backward to an older row.

### Acceptance Tests

- valid signed row verifies
- unsigned row rejected
- revoked signer fails with report
- lifecycle transition preserves old row
- supersession chain cycle detected
- drift report identifies changed field
- deterministic row hash

### Documentation Page

Draft docs page: `docs/guides/attested-registries.md`.

Sections:

- Signed rows
- Lifecycle states
- Supersession
- Drift reports
- Upstream reference metadata
- Application-owned semantics

### `ReadWalkReport`

Class: `CANDIDATE`.

Problem: reads can influence later actions. batpak should produce deterministic read evidence without knowing what the application will do with the data.

Generic users: compliance search, context assembly, forensic query, projection validation, audit exports.

Core types:

```rust
struct ReadWalkReport {
    read_id: String,
    as_of: FrontierPoint,
    regions: Vec<Region>,
    cursors: Vec<CursorPoint>,
    returned_count: u64,
    dropped_count: u64,
    limit_report: Vec<LimitObservation>,
    proof_refs: Vec<ReceiptRef>,
}
```

Required evidence: as-of point, walked regions, cursor positions, returned/dropped counts, proof handles.

Ordering/frontier semantics: must state whether the read is current, as-of, stale-allowed, or bounded by a visibility fence.

Failure semantics: partial reads either fail with evidence or succeed with explicit dropped/limit observations.

Canonical encoding impact: read reports should be compact enough to use in high-volume systems when enabled.

Replay/projection impact: replay can reconstruct what facts were visible to a decision, without interpreting the decision.

Minimum tests:

- complete read
- bounded read with dropped rows
- stale as-of read
- visibility-fenced read
- deterministic receipt bytes

Non-goals: ExtProfile, retrieval ranking, token budgeting, authorization semantics.

Downstream shrink trigger: Downstream can cite this for generic context/read evidence and keep context semantics above.

## Implementation Detail: Read Walk Reports

`ReadWalkReport` exists because reads can influence later writes. batpak should be able to report what a read observed without knowing what the caller will do next. It should not append on every read by default; applications decide whether a report is important enough to persist as a `ReadObserved` event.

### Required Capabilities

The read receipt should record:

- as-of frontier
- regions walked
- cursor positions
- filters applied where generic
- returned count
- dropped count
- limit observations
- proof refs
- stale/current/fenced mode

### Suggested Types

```rust
pub enum ReadConsistency {
    Current,
    AsOf(FrontierPoint),
    StaleAllowed { max_staleness_ms: u64 },
    VisibilityFenced { fence: CanonicalHash },
}

pub struct CursorPoint {
    pub region: Region,
    pub frontier: FrontierPoint,
}

pub struct LimitObservation {
    pub limit_name: String,
    pub limit: u64,
    pub observed: u64,
    pub dropped: u64,
}

pub struct ReadWalkReport {
    pub envelope: EvidenceEnvelope,
    pub read_id: String,
    pub consistency: ReadConsistency,
    pub regions: Vec<Region>,
    pub cursors: Vec<CursorPoint>,
    pub returned_count: u64,
    pub dropped_count: u64,
    pub limits: Vec<LimitObservation>,
    pub proof_refs: Vec<ReceiptRef>,
}
```

### API Sketch

```rust
pub fn read_with_report<Q, T>(
    store: &Store,
    query: Q,
    consistency: ReadConsistency,
) -> Result<(Vec<T>, ReadWalkReport), ReadWalkError>
where
    Q: BatpakQuery<T>;
```

### Acceptance Tests

- current read reports visible frontier
- as-of read does not include later writes
- stale-allowed read records staleness
- limit drops record dropped count
- proof refs are sorted deterministically
- report byte identity stable

### Documentation Page

Draft docs page: `docs/guides/read-walk-reports.md`.

Sections:

- Why report reads
- Consistency modes
- Limit and drop reporting
- Proof references
- Optional persisted `ReadObserved` events
- Application-owned authorization

### `StateTransitionReport` / `StateTransitionEvent`

Class: `CANDIDATE`.

Problem: many event-sourced systems express lifecycle as state transitions. The generic report/event should record prior state, next state, transition cause, and receipt pointer without knowing domain names.

Generic users: job runners, workflow engines, device lifecycle logs, compliance case systems.

Core types:

```rust
struct StateTransitionReport<S> {
    machine_id: String,
    previous: S,
    next: S,
    transition: String,
    cause: ReceiptRef,
    observed_at: HlcPoint,
}
```

Required evidence: previous state, next state, transition id, cause pointer, HLC.

Ordering/frontier semantics: transitions must be totally ordered per machine id.

Failure semantics: invalid transition returns a typed denial/diagnostic.

Canonical encoding impact: state names and transition ids need stable encoding.

Replay/projection impact: replay can reconstruct lifecycle state without domain code guessing from loose events.

Minimum tests:

- legal transition
- illegal transition denied
- duplicate transition id handled
- replay reconstructs final state
- deterministic receipt bytes

Non-goals: Mission/Session/Run/Effect lifecycle names or rules.

Downstream shrink trigger: Downstream may compose generic lifecycle evidence while keeping its own lifecycle law.

## Implementation Detail: State Transition Reports

`StateTransitionReport` should give batpak users a typed way to prove lifecycle transitions without importing domain lifecycle names. If a transition is appended, the durable fact is a `StateTransitionEvent` and batpak returns the normal `AppendReceipt`; the report explains the transition evidence.

### Required Capabilities

The primitive should support:

- state enum or string type
- transition id
- prior state
- next state
- cause receipt
- invalid transition denial
- replay reconstruction

### Suggested Types

```rust
pub trait StateMachineSpec {
    type State: CanonicalEncode + Clone + Eq;
    type Transition: CanonicalEncode + Clone + Eq;

    fn initial_state() -> Self::State;
    fn transition(
        current: &Self::State,
        transition: &Self::Transition,
    ) -> Result<Self::State, StateTransitionError>;
}

pub struct StateTransitionReport<S, T> {
    pub envelope: EvidenceEnvelope,
    pub machine_id: String,
    pub previous: S,
    pub next: S,
    pub transition: T,
    pub cause: ReceiptRef,
}
```

### API Sketch

```rust
pub fn apply_state_transition<M: StateMachineSpec>(
    store: &Store,
    machine_id: &str,
    transition: M::Transition,
    cause: ReceiptRef,
) -> Result<StateTransitionReport<M::State, M::Transition>, StateMachineError>;
```

### Acceptance Tests

- initial transition creates expected state
- legal transition emits a report or appended transition event
- illegal transition emits structured denial
- replay reconstructs final state
- duplicate transition does not corrupt sequence
- canonical bytes stable

### Documentation Page

Draft docs page: `docs/guides/state-transitions.md`.

Sections:

- Generic lifecycle reports/events
- Transition tables
- Invalid transitions
- Replay reconstruction
- Domain-owned state names

### `ReservationLedger`

Class: `DEFER` / external adjunct candidate until two non-Downstream, non-budget consumers need the same generic shape.

Problem: reserve/commit/refund appears in budgets, inventory, quotas, locks, scheduling, and external effect coordination. batpak can provide the durable mechanics without knowing policy.

Generic users: quota systems, financial holds, inventory allocation, job schedulers, external effect coordinators.

Core types:

```rust
struct Quantity {
    dimension: StableIdString,
    units: i128,
}

enum ReservationState {
    Reserved,
    Committed,
    Refunded,
    Expired,
    Orphaned,
}

struct ReservationTransitionReport {
    reservation_id: String,
    subject: String,
    quantity: Quantity,
    state: ReservationState,
    cause: ReceiptRef,
}
```

Required evidence: reservation id, subject, quantity, state, cause, expiration/reconciliation data.

Ordering/frontier semantics: state transitions must be linear per reservation id.

Failure semantics: orphaned reservations are observable and reconcilable.

Canonical encoding impact: quantities and subjects need stable encoding.

Replay/projection impact: replay can derive outstanding holds and reconcile crash recovery.

Minimum tests:

- reserve then commit
- reserve then refund
- reserve then expire
- crash before commit
- orphan detection
- deterministic ledger projection

Non-goals: Capability supply, Budget tier policy, Criticality escalation.

Downstream shrink trigger: Downstream can compose this for budgets/effects if useful while keeping policy above.

## Implementation Detail: Reservation Ledger

`ReservationLedger` may provide crash-recoverable reserve/commit/refund mechanics, but it is deliberately not first-wave work. It is closer to application policy than chain walking, schema snapshots, subscriber frontier, and projection runners. If kept, use generic quantities with stable dimensions; do not hard-code financial-looking `amount` semantics.

### Required Capabilities

The ledger should support:

- reserve
- commit
- refund
- expire
- orphan detection
- reconciliation report
- per-reservation linearity

### Suggested Types

```rust
pub struct ReservationId(pub String);

pub enum ReservationState {
    Reserved,
    Committed,
    Refunded,
    Expired,
    Orphaned,
}

pub struct ReservationRecord {
    pub id: ReservationId,
    pub subject: String,
    pub quantity: Quantity,
    pub state: ReservationState,
    pub expires_at: Option<HlcPoint>,
}

pub struct ReservationTransitionReport {
    pub envelope: EvidenceEnvelope,
    pub record: ReservationRecord,
    pub cause: ReceiptRef,
}

pub struct ReservationReconciliationReport {
    pub envelope: EvidenceEnvelope,
    pub open: Vec<ReservationId>,
    pub orphaned: Vec<ReservationId>,
    pub expired: Vec<ReservationId>,
}
```

### API Sketch

```rust
pub fn reserve(
    store: &Store,
    subject: String,
    quantity: Quantity,
    expires_at: Option<HlcPoint>,
) -> Result<ReservationTransitionReport, ReservationError>;

pub fn commit(
    store: &Store,
    id: ReservationId,
    cause: ReceiptRef,
) -> Result<ReservationTransitionReport, ReservationError>;

pub fn refund(
    store: &Store,
    id: ReservationId,
    cause: ReceiptRef,
) -> Result<ReservationTransitionReport, ReservationError>;

pub fn reconcile_reservations(
    store: &Store,
) -> Result<ReservationReconciliationReport, ReservationError>;
```

### Acceptance Tests

- reserve then commit
- reserve then refund
- reserve then expire
- double commit rejected
- refund after commit rejected unless policy allows reversal above batpak
- crash after reserve before commit reports open/orphaned
- reconciliation report deterministic

### Documentation Page

Draft docs page: `docs/guides/reservation-ledger.md`.

Sections:

- Reservation lifecycle
- Crash recovery
- Orphan detection
- Reconciliation
- Application-owned consequences

### `TargetProfile` / Process Evidence

Class: `SPLIT`.

Problem: platform facts and store-owned path-boundary facts are reusable mechanics. Process lifecycle evidence may become reusable only if batpak owns a generic runtime-evidence layer. Deployment meaning belongs to the application.

Generic users: local daemons, CLIs with durable state, robotics runtimes, and durable store users that need machine-fact evidence. Service-supervisor and sandboxed-tool use cases stay above batpak unless batpak grows a generic process runner.

Core types:

```rust
struct TargetProfile {
    os: String,
    filesystem: FilesystemProfile,
    clock: ClockProfile,
    process: Option<ProcessProfile>, // runtime-evidence feature only
    storage: StorageProfile,
}

struct ProcessBoundaryEvidence {
    process_id: String,
    boundary_kind: String,
    profile_hash: CanonicalHash,
    evidence: Vec<NamedHash>,
}
```

Required evidence: probed facts, profile hash, lifecycle observations, readiness/exit/drain evidence where applicable.

Ordering/frontier semantics: lifecycle observations must be ordered and replayable.

Failure semantics: unsupported profile is reported as a capability absence, not silently approximated.

Canonical encoding impact: profile hashes must be stable across equivalent probes.

Replay/projection impact: replay can explain which machine guarantees were assumed.

Minimum tests:

- stable profile hash
- changed profile detected
- process ready/exit evidence only if runtime-evidence exists
- path-boundary denial
- no sandbox/supervisor public identity in core

Non-goals: HostDeploymentTarget, Pack sandbox profile, auth, bind policy, service unit generation.

Downstream shrink trigger: Downstream can delete generic platform/process mechanics once batpak exposes them, while retaining deployment law.

## Implementation Detail: Target Profile And Process Evidence

This is the most dangerous extraction because it can accidentally absorb deployment law. Keep it narrow and split into lanes:

| Lane | Owner | Examples |
|---|---|---|
| store/platform facts | batpak core | filesystem profile, storage durability profile, clock profile, mmap/lock/sync facts, store lifecycle open/close/recover evidence |
| optional runtime-evidence feature | batpak only if it owns a generic process runner/evidence API | process started/stopped/crashed report, generic target profile hash |
| path-boundary safety | batpak core only for store/file safety | no-follow open, normalized path, root-bound proof tied to file safety |
| deployment/supervisor law | application / Downstream Host | generated service unit, sandbox profile emission, capability-to-syscall mapping, bind mounts, auth, network exposure, rollout/update rules |

Do not promote "sandbox evidence" or "supervisor" as public batpak identity phrases until batpak itself owns a generic process/sandbox/supervision mechanism. Otherwise keep them as application-layer composition over store/platform facts.

### Required Capabilities

batpak may expose:

- filesystem profile facts
- clock profile facts
- storage durability profile facts
- path-boundary proof if generic
- optional process lifecycle observations only behind runtime-evidence

batpak must not expose application-specific:

- deployment target closed sums
- generated service-unit law
- capability-to-syscall policy
- auth policy
- bind-mount declarations
- network exposure law

### Suggested Types

```rust
pub struct TargetProfile {
    pub os: String,
    pub arch: String,
    pub filesystem: FilesystemProfile,
    pub clock: ClockProfile,
    pub storage: StorageProfile,
    pub process: Option<ProcessProfile>,
}

pub struct ProcessLifecycleEvent {
    pub process_id: String,
    pub event: ProcessLifecycleKind,
    pub observed_at: HlcPoint,
    pub evidence: Vec<NamedHash>,
}

pub enum ProcessLifecycleKind {
    Started,
    Ready,
    DrainBegan,
    DrainCompleted,
    Stopped,
    Crashed,
    RestartIntentRecorded, // only if batpak owns a generic process runner
}

pub struct ProcessBoundaryEvidence {
    pub envelope: EvidenceEnvelope,
    pub process_id: String,
    pub boundary_kind: String,
    pub profile_hash: CanonicalHash,
    pub lifecycle: Vec<ProcessLifecycleEvent>,
}
```

### Path-Boundary Evidence

If batpak exposes path-boundary normalization, it should report:

- original path bytes
- normalized path
- root boundary
- symlink encounter
- no-follow open result
- denial reason

```rust
pub struct PathBoundaryEvidence {
    pub original_hash: CanonicalHash,
    pub normalized: String,
    pub root: String,
    pub symlink_encountered: bool,
    pub no_follow: bool,
    pub result: PathBoundaryResult,
}
```

### API Sketch

```rust
pub fn target_profile() -> Result<TargetProfile, TargetProfileError>;

pub fn record_process_lifecycle(
    store: &Store,
    event: ProcessLifecycleEvent,
) -> Result<ProcessBoundaryEvidence, ProcessEvidenceError>;

pub fn normalize_path_boundary(
    root: &Path,
    path: &Path,
) -> Result<PathBoundaryEvidence, PathBoundaryError>;
```

### Acceptance Tests

- profile hash stable across equivalent probe
- changed filesystem profile changes hash
- process start/ready/stop sequence records ordered evidence only under runtime-evidence
- restart intent recorded only if batpak owns the runner
- symlink escape denied
- unsupported platform returns unsupported, not fake profile

### Documentation Page

Draft docs page: `docs/guides/target-and-process-evidence.md`.

Sections:

- Machine facts versus deployment law
- Target profiles
- Process lifecycle evidence
- Path-boundary evidence
- Unsupported targets

## Implementation Detail: Clock-Skew Evidence

Clock-skew evidence is generic if it records the relationship between wall time, HLC, and durable frontier without deciding application freshness policy.

### Required Capabilities

batpak may expose:

- observed wall time
- last durable HLC
- clamped HLC
- clamp reason
- recurrence count
- target clock profile hash

### Suggested Types

```rust
pub struct ClockSkewEvidence {
    pub observed_wall_ms: u64,
    pub durable_frontier_hlc: HlcPoint,
    pub clamped_hlc: HlcPoint,
    pub reason: ClockSkewReason,
    pub recurrence_count: u64,
}

pub enum ClockSkewReason {
    BackwardWallClock,
    DurableFrontierAhead,
    ClockSourceUnsupported,
}
```

### Non-Goals

- auth token freshness
- DPoP window policy
- session timeout policy
- application retry policy

### Acceptance Tests

- wall clock ahead uses wall time
- wall clock behind clamps after durable frontier
- repeated skew increments recurrence
- evidence report stable
- auth freshness is not mentioned

## Implementation Detail: Audit Assertion Runner

An audit assertion runner is generic if it evaluates read-only predicates over evidence rows without owning application admission.

Classification: `DEFER` / helper-crate candidate. This can become a compliance DSL if pulled into core too early. Prefer `batpak-test` / `batpak-audit` tooling unless multiple core-store users need the same runner.

### Required Capabilities

The runner may support:

- no-result-where
- every-result-has
- count bounds
- gap checks
- deterministic result ordering
- source range / evidence hash

### Suggested Types

```rust
pub enum AuditAssertion {
    NoResultWhere(String),
    EveryResultHas(String),
    CountAtMost(u64),
    CountAtLeast(u64),
    NoGaps,
}

pub struct AuditAssertionReport {
    pub envelope: EvidenceEnvelope,
    pub assertion: AuditAssertion,
    pub outcome: AuditOutcome,
    pub evidence_hashes: Vec<CanonicalHash>,
}

pub enum AuditOutcome {
    Passed,
    Failed,
    Inconclusive,
}
```

### Non-Goals

- application-named audit queries
- admission denial
- compliance dashboard semantics
- Pack compile diagnostics

### Acceptance Tests

- passing assertion
- failing assertion with evidence rows
- no-result assertion deterministic
- count assertion deterministic
- inconclusive when evidence missing

## Implementation Detail: Deterministic Phase Cache

A deterministic phase cache is generic if it memoizes content-addressed computation phases without knowing what the phases mean.

Classification: `DEFER` / tooling candidate. This is likely build/xtask/test infrastructure, not core store API, unless batpak itself grows content-addressed phase execution.

### Required Capabilities

- input hash
- dependency hash
- phase id
- output hash
- invalidation reason
- no timestamp invalidation

### Suggested Types

```rust
pub struct PhaseCacheKey {
    pub phase_id: String,
    pub input_hashes: Vec<NamedHash>,
}

pub struct PhaseCacheEntry {
    pub key: PhaseCacheKey,
    pub output_hash: CanonicalHash,
    pub evidence_hash: CanonicalHash,
}

pub enum PhaseCacheStatus {
    Hit,
    Miss { reason: CacheMissReason },
}
```

### Non-Goals

- Pack Pipeline phase names
- compiler diagnostics
- user-facing build policy

### Acceptance Tests

- same inputs hit
- changed dependency misses
- timestamp-only change does not invalidate
- output hash stable

## Candidate Registry

This registry is the compact view. The RFC cards above are the working detail.

| Candidate | Classification | Proposed batpak surface | Keep out |
|---|---|---|---|
| `ReceiptChainWalker` | `READY_RFC` | Generic chain pointer/hash/cycle/fork verification over append receipts. | Authority, causation, composition lineage, denial mapping. |
| `SchemaSnapshot` | `READY_RFC` | Canonical-byte snapshot harness for event/receipt schemas and emitted byte projections. | Protocol semantics and Pack IR meaning. |
| `SubscriberFrontier` + `LagPolicy` | `READY_RFC` | Generic subscriber lag, consumed frontier, lag age, loss marker, and back-pressure policy over lossy subscriptions. | ExtProfile, Mission Control, protocol subscription law. |
| `ProjectionRunner` | `CANDIDATE` / `MOSTLY_DONE` | Cursor-driven replay/apply runner with deterministic checkpointing and rebuild support. | Pack-declared projection targets, target schemas, emitted artifacts. |
| `StoreResourceEnvelope` | `SPLIT` | Generic store-owned bytes, queue, cursor, subscriber, and projection pressure observations. | Host connection pools, Criticality/Budget consequences, operator recovery, Pack-declared tightening. |
| `CanonicalArtifactEnvelope` | `CANDIDATE` / feature-gated | Stable body hash plus external signature/attestation envelope after canonical encoding contract is stable. | Pack.lock, IMAGE, Capsule. |
| `AttestedRegistry` | `CANDIDATE` | Signed immutable rows with stable id, artifact hashes, lifecycle, and drift checks. | Pack-author registry law and protocol field semantics. |
| `ReadWalkReport` | `CANDIDATE` | Deterministic read evidence with as-of, regions, cursors, dropped rows, limits, proof refs; optional persisted event above it. | ExtProfile, ranking, token budget, authorization. |
| `StateTransitionReport` / `StateTransitionEvent` | `CANDIDATE` | Generic transition evidence over prior/next/cause/HLC. | Mission/Session/Run/Effect lifecycle law. |
| `ReservationLedger` | `DEFER` | Durable reserve/commit/refund/expire/orphan mechanics after non-Downstream pressure proves the common shape. | Capability supply, Budget tier, Criticality. |
| `TargetProfile` / process evidence | `SPLIT` | Store/platform facts in core; process evidence only behind optional runtime-evidence if generic. | HostDeploymentTarget, Pack sandbox policy, auth, service unit law. |
| Cursor checkpoint/restart helpers | `MOSTLY_DONE` | Small helper APIs around checkpoint identity, restart monotonicity, and cursor worker lifecycle if current API is not enough. | Reaction semantics and application idempotency law. |
| Canonical encoding/version-anchor contract | `UPSTREAM_STRENGTHEN` | Stronger public statement and tests for canonical-byte stability across batpak patch releases. | Pack.lock/IMAGE verification and explicit re-pin ceremony. |
| Generic staged-pipeline proof token | `DEFER` | Possible affine stage-token utility for ordered pipelines. | Downstream-style named admission membranes. |
| Settings value handle / provenance cascade | `DO_NOT_IMPORT_PACK_LAW` | Only future generic typed setting provenance if stripped of application authority. | Settings authority chains and deployment config law. |

## Current Batpak Reality Check

Observed snapshot: 2026-05-07 against `../batpak/batpak`. Revalidate before opening batpak PRs. batpak already has a large part of the desired foundation; the extraction work is mostly promotion, naming, and rounding, not greenfield invention.

| Desired surface | Current batpak evidence | Disposition for batpak |
|---|---|---|
| typed event/receipt derives | `EventPayload`, `EventSourced`, `MultiEventReactor`; public derives in `src/lib.rs`; ADR-0010 | Already real. Downstream should compose, not wrap. |
| typed append/query paths | `append_typed`, `submit_typed`, `try_submit_typed`, `BatchAppendItem::typed`, `by_fact_typed`, `Transition::from_payload`; ADR-0010 | Already real. Downstream macros should emit these directly, never raw byte-level event paths. |
| append receipts / denial receipts / extension keys | `AppendReceipt`, `DenialReceipt`, `ExtensionKey` in `src/store/append.rs` | Already real. Need chain-walking/verifier convenience, not new receipt identity. |
| frontier states | `FrontierView`, `WatermarkSnapshot`, `HlcPoint`, `WatermarkKind` in `src/store/stats.rs`; ADR-0014 | Already real. Downstream should cite these for received/written/durable/visible/applied/emitted posture. |
| visibility fence | public `VisibilityFence` in `src/store/write/control/fence.rs`; guide section "Visibility fences" | Already real. Do not invent `FrontierState` unless it adds missing public semantics. |
| HLC / lifecycle ordering | `HlcPoint`; `SYSTEM_OPEN_COMPLETED` / `SYSTEM_CLOSE_COMPLETED`; reopen monotonicity in store lifecycle | Already real. Downstream should cite lifecycle evidence instead of explaining restart physics. |
| durability gates and waits | `DurabilityGate`; `AppendOptions::with_gate`; `Store::wait_for_durable` / `wait_for_applied` / `wait_for_visible`; ADR-0016 | Already real. Downstream should only name which Pack declaration selects which watermark. |
| batch atomicity / fenced batch submit | `append_batch`, `append_batch_with_options`, `BatchAppendItem`, batch BEGIN/COMMIT markers, `VisibilityFence::submit_batch` | Already real. Do not add Downstream batch mechanics unless they carry Pack compensation/Effect meaning. |
| outbox staging | `Store::outbox`, `Outbox::stage`, `Outbox::flush`, `Outbox::submit_flush`; guide section "Outbox" | Already real for batch-shaped internal commits. Does not own Downstream Effect/Port/provider delivery semantics. |
| append idempotency | `AppendOptions::with_idempotency`, `StoreError::IdempotencyRequired`, group-commit idempotency invariant | Already real for append/batch dedupe. Downstream still owns wire-level idempotency scope and replay-response semantics. |
| publish-before-reply visibility | `publish_then_broadcast_unfenced` establishes visibility before append returns | Already real. Downstream read-your-writes doctrine should cite this instead of re-teaching publish ordering. |
| coordinate-region containment | `coord_in_region(coord, region)` segment-boundary predicate | Already real or batpak-bound by Downstream G2. Downstream must not implement prefix matching locally. |
| at-least-once witnesses | `AtLeastOnce`, `CheckpointId`, `ObservedOnce` in `src/store/delivery/observation.rs`; ADR-0017 | Already real. Downstream cursor-reactor should use directly. |
| cursor checkpoint/gap/restart | `Cursor`, `CursorGapConfig`, `GapObservation`, `cursor_worker`, `CursorWorkerHandle`, `RestartPolicy::{Once, Bounded}` in delivery cursor / ADR-0011 | Mostly real. May need naming polish for subscriber-frontier use, not new cursor machinery. |
| lossy subscription | `Subscription`, `SubscriptionOps`, `subscribe_lossy` in guide/API | Real but lacks explicit public `SubscriberFrontier` / slow-consumer lag contract. |
| projection watching/cache | `ProjectionWatcher`, `ProjectionCache`, `NativeCache`, `NoCache`, `Freshness` | Real. Need decide whether a public `ProjectionRunner` adds value beyond existing watcher/cache/replay plan surfaces. |
| platform profile/evidence | private `src/store/platform/*`; public `PlatformEvidenceSummary`, `StoreConfig::with_platform_profile_path`, `cargo xtask platform ...`; ADR-0018 | Real for store-path platform posture. Keep private unless non-store target evidence needs public API. |
| writer pressure | `WriterPressure`, `Store::writer_pressure()` | Real queue-pressure primitive; not the same as full `StoreResourceEnvelope`. |
| canonical encoding | `pub use crate::encoding as canonical`; docs say stronger canonical-bytes contract is still phased in | Exists but needs strengthened version contract/tests before Downstream can delete caveat prose. |

Immediate implication: do not build all-new types by reflex. First publicly name existing guarantees, add focused tests/docs, and expose small helpers only where the current API is missing a reusable generic surface.

## Batpak Build Queue

This is the conceptual target for batpak. Names are suggested batpak names, not Downstream commitments. Build against the `Boundary` column; do not chase Downstream-specific anchors.

| Priority | Surface | Action | Boundary |
|---|---|---|---|
| A | `ReceiptChainWalker` | Add verifier over existing `AppendReceipt` / segment hash-chain data. | Knows receipt continuity; not Mission/Capability/Budget/Port. |
| A | projection runner / watcher docs | Decide whether existing `ProjectionWatcher` / cache / replay-plan APIs already are the runner. | Knows cursor replay; not Pack projection meanings. |
| A | `SchemaSnapshot` harness | Add only if existing derives/schema tests cannot cover fixture needs. | Knows canonical bytes; not MCP/A2A/ExtProfile semantics. |
| A | canonical encoding stability | Strengthen version contract/tests for `batpak::canonical`. | Knows byte stability; not Pack.lock policy. |
| B | `SubscriberFrontier` / `LagPolicy` | Add public slow-consumer frontier over `Subscription` / `subscribe_lossy`. | Knows lag/loss; not ExtProfile/Mission Control authority. |
| B | `StoreResourceEnvelope` | Extend beyond `WriterPressure` into per-region/entity accounting if needed. | Knows store pressure; not host pools or Budget/Criticality consequences. |
| C | `TargetProfile` / optional runtime evidence | Broaden only as store/platform facts first; process evidence only if generic. | Knows machine facts; not Pack deployment law. |

## Proposed Implementation Waves For batpak

These waves are ordered to minimize semantic risk.

### Wave A - Strengthen Existing Surfaces

Goal: expose or document what batpak already has, then add small audit helpers.

Work:

1. Document current typed event/receipt derives.
2. Document current append/query typed paths.
3. Document existing `FrontierView`, `VisibilityFence`, `AtLeastOnce`, `ObservedOnce`, `ProjectionWatcher`, and `PlatformEvidenceSummary`.
4. Strengthen canonical encoding version contract.
5. Add `ReceiptChainWalker` over existing receipt/hash material.
6. Add or document `SchemaSnapshot` harness.

Exit criteria:

- batpak docs can explain receipt continuity without Downstream
- canonical encoding contract is line-citable
- schema/golden fixture drift can fail a batpak test
- Downstream can cite public batpak APIs for chain walking and schema snapshots

### Wave B - Consumption And Projection Evidence

Goal: make replay/subscription/projection behavior auditable.

Work:

1. Add `SubscriberFrontier` / `LagPolicy` over lossy subscriptions.
2. Decide whether `ProjectionWatcher` is enough or whether `ProjectionRunner` is needed.
3. Add projection run reports if needed.
4. Add `ReadWalkReport`.
5. Add checkpoint-visible replay tests.

Exit criteria:

- slow consumers have explicit lag/loss evidence
- projection rebuilds can prove no half-visible checkpoint
- important reads can be reported without application semantics; persistence is caller-owned

### Wave C - Resource And Lifecycle Evidence

Goal: make runtime pressure and lifecycle facts observable without policy.

Work:

1. Extend `WriterPressure` into `StoreResourceEnvelope` only if the extra store-owned counters are real.
2. Add `StateTransitionReport` / `StateTransitionEvent`.
3. Keep `ReservationLedger` deferred unless non-Downstream pressure proves the shared shape.
4. Add clock-skew evidence if not already covered by lifecycle/frontier docs.
5. Add thread/process lifecycle evidence only where batpak can observe it generically.

Exit criteria:

- resource pressure reports facts only
- state machines can replay lifecycle generically
- reserve/commit/refund remains deferred or proves crash/reconcile without policy
- clock skew evidence exists without auth policy

### Wave D - Artifact / Registry / Target Evidence

Goal: support signed artifacts, generic registries, and machine-fact evidence without absorbing deployment law.

Work:

1. Add `CanonicalArtifactEnvelope` if no existing crate/pattern already covers it.
2. Add `AttestedRegistry` if multiple batpak consumers need signed row lifecycle.
3. Publicly document target profile evidence already present.
4. Add path-boundary evidence only for store/file safety; process evidence only behind optional runtime-evidence; do not add sandbox/supervisor identity until batpak owns the mechanism.
5. Add backup envelope only if batpak owns backup mechanics generically.

Exit criteria:

- signed artifact body identity is stable
- registry rows can be verified and drift-compared
- target facts are evidence, not deployment promises

## Batpak ADR Queue

A batpak coding agent should create ADRs in this order unless the repo already has equivalent ADRs.

| ADR | Purpose | Depends on |
|---|---|---|
| Receipt chain walking | Public audit walker over append receipts | existing receipt/hash structure |
| Canonical encoding contract | Patch/minor stability policy for canonical bytes | current encoding module |
| Schema snapshots | Stable schema/fixture drift harness | canonical encoding contract |
| Subscriber frontier | Lossy subscriber lag/loss evidence | subscription/cursor APIs |
| Projection runner report | Public rebuild/checkpoint evidence | projection watcher/cache APIs |
| Read walk reports | Deterministic read-as-evidence surface; optional persisted event above it | cursor/query APIs |
| Resource envelope | Policy-neutral pressure observations | writer pressure APIs |
| State transition reports/events | Generic lifecycle transition evidence | append receipts |
| Reservation ledger | Deferred reserve/commit/refund/reconcile candidate | state transition reports helpful but not required |
| Artifact envelope | Stable body hash + signature metadata | canonical encoding contract |
| Attested registry | Signed immutable row lifecycle | artifact envelope helpful but not required |
| Target/process evidence | Machine and process evidence split from app deployment | existing platform evidence |

Each ADR should include:

- problem
- non-agent users
- API sketch
- evidence emitted
- canonical encoding impact
- error semantics
- tests
- explicit non-goals
- compatibility promise

## Batpak Docs Queue

The batpak repo should gain docs that stand alone from Downstream.

Suggested docs:

```text
docs/guides/black-box-physics.md
docs/guides/receipt-chain-walking.md
docs/guides/canonical-encoding.md
docs/guides/schema-snapshots.md
docs/guides/subscriber-frontiers.md
docs/guides/projection-runners.md
docs/guides/read-walk-reports.md
docs/guides/resource-envelopes.md
docs/guides/state-transitions.md
docs/guides/reservation-ledger.md
docs/guides/canonical-artifact-envelopes.md
docs/guides/attested-registries.md
docs/guides/target-and-process-evidence.md
```

`black-box-physics.md` should be the batpak front door for this extraction. It should say:

```text
batpak records durable consequences, frontiers, receipts, projections,
resource observations, lifecycle evidence, and replayable reports.
Applications compose those mechanics into domain law.
```

It should not mention Downstream except as one consumer in an examples list, if at all.

## Example Non-Agent Consumers

Use these examples in batpak docs and tests to keep the design domain-neutral.

### Compliance Event Store

Needs:

- append-only records
- receipt-chain walking
- schema snapshots
- read-walk reports
- audit assertions
- signed export envelopes

Does not need:

- agents
- Missions
- protocol adapters

### Robotics Recorder

Needs:

- HLC ordering
- process lifecycle evidence
- target profile evidence
- projection replay
- resource pressure reports
- state-transition reports/events

Does not need:

- Pack authoring
- human approval
- budget tiers

### Local-First Sync Engine

Needs:

- cursor checkpoints
- at-least-once witnesses
- subscriber frontier
- schema lifecycle
- receipt chain
- projection rebuilds

Does not need:

- agent context retrieval
- MCP/A2A
- host deployment law

### Financial Ledger

Needs:

- append receipts
- reservation ledger
- canonical artifact envelope
- attested registry
- audit assertion runner
- deterministic query envelopes

Does not need:

- Mission lifecycle
- PortKind
- ExtProfile

### Simulation Log

Needs:

- deterministic phase cache
- state-transition reports/events
- projection runner
- canonical encoding stability
- schema snapshots
- target profile evidence

Does not need:

- user-facing projections
- agent roles
- auth policy

If a proposed API cannot be explained to at least two of these consumers, it is probably too application-specific.

## Mailbox / JMAP Lineage Note

JMAP/mailbox lineage is not batpak API and should not become batpak identity. However, mailbox-shaped carrier primitives may inform generic cursor/frontier/read-report work:

- account / mailbox namespace
- object id
- state token
- changes-since-state query
- blob reference
- push / change notification
- delivery / read / applied frontier split

Carrier authority belongs above batpak. batpak may mine the mechanics; it should not import JMAP vocabulary as public substrate identity.

## Batpak Test Matrix

Each implemented candidate should be tested across three axes:

### Store State

| State | Must test |
|---|---|
| empty store | report is deterministic |
| small valid store | happy path |
| corrupt/missing link | structured finding |
| reopened store | restart evidence preserved |
| partial work | no half-visible result |

### Encoding

| State | Must test |
|---|---|
| same input twice | identical canonical bytes |
| field order changed | identity unchanged if canonical map semantics allow |
| added diagnostic metadata | body identity unchanged when promised |
| enum variant unknown | fail closed unless non-exhaustive/advisory |

### Policy Neutrality

| Test | Failure signal |
|---|---|
| no app vocabulary appears in public type | fail review |
| no agent vocabulary appears in errors | fail review |
| report contains facts, not decisions | fail review |
| caller decides consequence | pass |

## Contamination Review Checklist

Before merging any batpak extraction PR, search the diff for these tokens:

```text
Pack
Mission
Session
Run
Capability
Criticality
Budget
PortKind
MCP
A2A
AG-UI
A2UI
ExtProfile
IMAGE
Capsule
HumanApproval
HostDeploymentTarget
InferencePort
```

Allowed appearances:

- in a non-goals section
- in a "not this" example
- in tests proving no vocabulary leak

Disallowed appearances:

- public type names
- public error names
- module names
- trait names
- feature names
- docs explaining batpak identity

Also search for weak catch-all names:

```text
Manager
Handler
Processor
Controller
Orchestrator
Service
Engine
```

batpak may already have some conventional names; do not churn existing code only for naming. For new extraction surfaces, prefer precise evidence names over generic weak nouns.

## Negative Prompt Filter For Downstream Shaving

Use this section when reading Downstream root docs. If a paragraph primarily describes one of these generic mechanisms, it probably does not belong in canonical Downstream prose except as a short dependency placeholder.

| If Downstream prose explains... | Replace with placeholder for... | Downstream keeps... |
|---|---|---|
| parent/hash/cycle/fork receipt walking | `ReceiptChainWalker` | which receipt fields are authority-bearing and which denial variant surfaces failure |
| schema hash / fixture hash drift mechanics | `SchemaSnapshot` | which Pack/protocol/artifact shapes are registered for snapshots |
| slow subscriber lag/loss/backpressure mechanics | `SubscriberFrontier` / `LagPolicy` | ExtProfile/Highway consumer meaning and user-visible consequences |
| projection rebuild/checkpoint/as-of runner mechanics | `ProjectionRunner` / checkpoint-visible replay fence | projection target meaning and Pack-declared projection law |
| store bytes/counts/queues/in-flight ceiling observations | `StoreResourceEnvelope` | host resource pools, Budget/Criticality consequences, and Pack-authored ceilings |
| body hash plus signature envelope mechanics | `CanonicalArtifactEnvelope` | Pack.lock / IMAGE / Capsule identity law |
| signed immutable row lifecycle / supersession / drift | `AttestedRegistry` | Pack-author registry categories and protocol semantic mapping |
| read-as-of / walked regions / dropped rows / proof refs | `ReadWalkReport` | ExtProfile meaning, retrieval ranking, token budget, auth |
| prior/next state transition evidence | `StateTransitionReport` / `StateTransitionEvent` | Mission/Session/Run/Effect lifecycle law |
| reserve/commit/refund/expire/orphan mechanics | deferred `ReservationLedger` | Budget policy, Capability supply, Criticality escalation |
| target profile / process started-ready-stopped evidence | `TargetProfile` / `ProcessBoundaryEvidence` | HostDeploymentTarget and deployment artifact law |
| wall-clock/HLC/frontier clamp evidence | `ClockSkewEvidence` | auth freshness vs receipt-order distinction |
| no-follow open / path normalization / root-bound proof | `PathBoundaryEvidence` | Pack bind-mount meaning and File capability law |
| sealed backup manifest / segment hashes / restore proof | `BackupEnvelope` | which Pack/IMAGE/Mission facts the backup must contain |
| deterministic audit result rows | deferred `AuditAssertionRunner` / helper crate | named Downstream audit queries and filter grammar |
| content-hash phase memoization | deferred `DeterministicPhaseCache` / tooling | Pack Pipeline phase names and author diagnostics |
| compaction input/output/range evidence | `CompactionReport` / optional compaction event | Pack retention policy and Topic-specific compaction law |
| idempotency key lookup/replay cache mechanics | `IdempotencyLedger` | operation write-intent and replay-response semantics |
| generic region query constructors / scan bans | `RegionBoundQuery` | tenant/capability/scope policy |

The edit rule:

```text
If the paragraph can serve a financial ledger, robotics recorder, local-first
sync engine, compliance event store, or simulation log unchanged, move the
mechanism to batpak and leave a placeholder in Downstream.
```

The exception:

```text
If the paragraph explains Pack-authored meaning, admission semantics, Capability
law, Budget/Criticality consequences, Port/protocol law, IMAGE/Capsule identity,
or human approval, keep it in Downstream.
```

## Additional Spec Seeds For Easy-To-Miss Surfaces

These surfaces were not in the first high-priority RFC batch, but they are likely to matter once batpak starts absorbing more black-box mechanics.

### `BackupEnvelope`

Generic shape:

```rust
pub struct BackupEnvelope {
    pub manifest_hash: CanonicalHash,
    pub segment_hashes: Vec<NamedHash>,
    pub inspector_hash: Option<CanonicalHash>,
    pub signature_refs: Vec<SignatureEnvelope>,
    pub restore_report_hash: Option<CanonicalHash>,
}
```

batpak owns:

- segment hash verification
- manifest body hash
- signature verification report
- bundled inspector metadata
- restore proof shape

batpak does not own:

- Pack identity
- Mission manifest meaning
- IMAGE activation
- retention policy

### `CompactionReport`

Generic shape:

```rust
pub struct CompactionReport {
    pub input_range_hash: CanonicalHash,
    pub output_hash: CanonicalHash,
    pub source_receipts: Vec<ReceiptRef>,
    pub target_region: Region,
    pub policy_id: String,
}
```

batpak owns:

- input range proof
- output proof
- source receipt links
- deterministic report
- optional appended compaction event only if the caller chooses to persist the report

batpak does not own:

- retention law
- legal erasure meaning
- Topic-specific compaction policy

### `IdempotencyLedger`

Generic shape:

```rust
pub struct IdempotencyLedgerEntry {
    pub key: String,
    pub region: Region,
    pub first_receipt: ReceiptRef,
    pub expires_at: Option<HlcPoint>,
}
```

batpak owns:

- key lookup
- prior receipt reference
- TTL expiration evidence
- duplicate observation

batpak does not own:

- operation write intent
- replay response policy
- compensation law

### `ForensicQueryEnvelope`

Generic shape:

```rust
pub struct ForensicQueryEnvelope {
    pub query_hash: CanonicalHash,
    pub as_of: FrontierPoint,
    pub result_hashes: Vec<CanonicalHash>,
    pub order: QueryOrder,
}
```

batpak owns:

- deterministic ordering
- as-of evidence
- result hashes
- no storage-iteration-order leakage

batpak does not own:

- named audit query catalog
- allegation-to-evidence playbook
- Pack-specific filters

### Receipt Proof Scaling Strategy

Classification: parked strategy note.

The first extraction path should assume linear receipt-chain walking over existing append receipt / segment hash-chain evidence. Merkle transparency logs, sparse proofs, and forward-secure receipt signing are deliberately unresolved until regulator, customer, or scale pressure proves the need.

Do not solve this in the first batpak extraction PR. Preserve the option by keeping report bodies deterministic, proof refs explicit, and link labels generic.

### `RegionBoundQuery`

Generic shape:

```rust
pub struct RegionBoundQuery {
    pub region: Region,
    pub predicate_hash: CanonicalHash,
    pub limit: Option<u64>,
}
```

batpak owns:

- query must name region
- unbounded scan lint/helper
- deterministic query envelope

batpak does not own:

- tenant policy
- capability policy
- scope law

## Downstream Shrink Protocol

After a batpak API ships, Downstream should shrink in three steps:

1. Replace generic mechanism tutorial with a one-paragraph dependency citation.
2. Keep the Downstream semantic law.
3. Keep the receipt/admission/projection fields that are Downstream-specific.

Example:

Before:

```text
Downstream explains how to walk receipt parent links, check hashes,
detect cycles, identify forks, and produce chain-integrity reports.
```

After:

```text
Downstream uses batpak::ReceiptChainWalker for generic receipt-chain
verification. Downstream-specific law is limited to which receipt fields
are authority-bearing and which denial variant reports a failed audit.
```

Do not shrink before:

- batpak API is public
- batpak docs are line-citable
- Downstream pins a version containing it
- tests prove the cited guarantee

Downstream shrink PRs cannot merge on intent alone. A shrink PR must prove:

- the Downstream workspace pins a batpak version that contains the cited public API
- batpak docs or ADRs are line-citable for the guarantee
- Downstream tests or compile checks consume the public API where applicable
- old Downstream generic-mechanism prose is replaced with a dependency citation plus Downstream-owned meaning

## Paired Deletion Tickets

Every high-priority batpak export row should have a paired Downstream deletion candidate. The paired deletion does not execute until the batpak release gate above is satisfied.

| Batpak export row | Downstream deletion candidate | Keep in Downstream |
|---|---|---|
| `ReceiptChainWalker` / receipt-chain evidence report | Delete generic hash / parent / fork / cycle walking tutorial prose. | receipt envelope fields, authority-bearing meaning, Downstream denial mapping |
| canonical encoding contract + `SchemaSnapshot` | Delete generic schema / fixture drift mechanics. | which Pack IR structs, protocol fixtures, and projections are pinned |
| `SubscriberFrontier` / `LagPolicy` | Delete generic slow-consumer lag / loss / backpressure mechanics. | ExtProfile / Highway consumer meaning and user-visible consequences |
| projection runner / watcher report | Delete generic rebuild / checkpoint / as-of runner tutorial prose. | Pack-declared projection targets and protocol / skill / OTel / SBOM meaning |

## What To Do In The batpak Repo First

A coding agent entering batpak should not start by implementing everything. It should do this:

1. Inspect existing public exports in `src/lib.rs`.
2. Inspect existing ADRs for typed events, receipts, frontiers, visibility fences, cursor workers, and platform evidence.
3. Create an issue or ADR for `ReceiptChainWalker`.
4. Add the smallest public chain-walk report over existing receipt data.
5. Add deterministic report tests.
6. Document the guarantee.
7. Only then move to `SchemaSnapshot`.

The first PR should be boring and narrow. If the first PR tries to add artifact envelopes, registries, target profiles, and reservation ledgers at once, it is too large.

## Suggested First PR

Title:

```text
Add deterministic receipt-chain evidence report
```

Scope:

- report body/envelope split
- public request/report/finding types
- walker over existing receipt/hash material
- deterministic report ordering
- tests for missing parent/hash mismatch/cycle
- docs guide
- optional future event append is out of scope

Non-goals:

- application authority
- schema snapshots
- projection runner
- registry rows
- target profile

Acceptance:

- `cargo test receipt_chain`
- report canonical bytes snapshot
- docs compile if doctests are enabled

## Suggested Second PR

Title:

```text
Strengthen canonical encoding contract and schema snapshots
```

Scope:

- document canonical byte stability promise
- add schema snapshot request/report types
- add fixture drift tests
- add additive/breaking/unknown classification

Non-goals:

- protocol semantics
- Pack IR
- registry lifecycle

Acceptance:

- schema unchanged test passes
- field removal fails
- fixture drift fails
- report bytes deterministic

## Suggested Third PR

Title:

```text
Expose subscriber frontier for lossy subscriptions
```

Scope:

- subscriber id/frontier types
- lag policy
- loss observation
- query API over lossy subscription state
- restart behavior tests

Non-goals:

- UI subscriptions
- context protocols
- application backpressure decisions

Acceptance:

- lag over threshold reported
- loss range captured
- restart preserves frontier when checkpointed

## Suggested Fourth PR

Title:

```text
Document projection runner semantics or add run report
```

Scope:

- decide whether existing projection watcher/cache is enough
- add report type only if needed
- prove no half-visible checkpoint
- document replay mode

Non-goals:

- application projection targets
- wire protocols

Acceptance:

- rebuild/resume/crash tests
- deterministic report

## Release Note Template For batpak

When these land, batpak release notes should separate mechanism from consumer meaning:

```text
Added:
- ReceiptChainWalker: verifies generic append receipt continuity and
  parent-link reachability. Does not interpret application authority.

Changed:
- canonical encoding docs now specify patch-level stability for report
  bodies emitted by audit helpers.

Experimental:
- SubscriberFrontier behind feature `subscriber-frontier`.
```

Avoid:

```text
Added Downstream support for Missions.
```

batpak should say what batpak provides, not what Downstream composes.

## Rubber-Duck Backlog

Six doc-specific read-throughs on 2026-05-07 found these additional generic candidates. Treat this table as self-contained batpak backlog; the Downstream terms in the boundary column are warning labels, not prerequisites.

| Surface | Generic batpak shape | Boundary / keep-out |
|---|---|---|
| `StateTransitionReport` / transition-table derive | Closed transition table where each transition records prior state, next state, cause, HLC, and receipt pointer. | No Mission / Session / Run / Effect lifecycle law. |
| `ReservationLedger` / `ReserveCommitRefund` | DEFER: durable reservation lifecycle with Reserve, Commit, Refund, orphan detection, and crash reconciliation. | No Capability supply, Budget tier, Criticality, or admission-denial consequence. |
| Multi-link `ReceiptChainWalker` | Verify named receipt links, not only linear hash chain: parent exists, content hash matches, no cycles, no fork without declared branch. | Link names are generic; Downstream owns causation-vs-authority meaning. |
| Checkpoint-visible replay fence | Cursor/replay boundary where partial work may happen internally, but no half-completed checkpoint becomes visible. | No Pack migration law or Topic-specific failure mapping. |
| Schema/version lifecycle | Event schema hash envelope, strict-reject unknown schema by default, `live -> announced -> deprecated -> removed` lifecycle, migration hook points. | No Pack-declared migration functions or Pack.lock policy. |
| Projection descriptor | Generic projection metadata: source regions, output shape, reactivity mode, validity predicate, drift-test hook, current/as-of/time-indexed replay. | No protocol, skill, OTel, UI, or Pack projection meaning. |
| Versioned fact lifecycle | Append-only fact lifecycle with `live`, `faded`, `superseded_by`, and preserved existence marker. | No Topic, PII, KeyedBinding, GDPR, or Downstream extension keys. |
| Temporal hold / throttle observation | Retry-after HLC, circuit-open-until HLC, backpressure restoration pulse, and hold expiry evidence. | No Pack fallback policy or criticality escalation. |
| Emission-liveness harness | Generic assertion that a declared append/projection/write path has observable output or a receipted no-op/denial. | No Pack harness lattice or named audit pass ownership. |
| Compaction report / optional compaction event | Compaction evidence with input range hash, output hash, target coordinate/region, policy id, and source receipt pointers. | No Pack compaction policy or retention law. |
| Read-walk report | Read/context evidence: coordinates walked, cursor positions, as-of point, dropped entries, budget/limit counters, proof handles. | No ExtProfile, retrieval ranking, token-budget, or Capability semantics. |
| Audit result envelope | DEFER/helper crate: generic evidence row for lint/property/audit result: test name, outcome, evidence hash, observed/expected fields, and source range. | No Downstream twenty-pass harness names. |
| Signed canonical artifact envelope | Deterministic canonical identity body plus signed envelope; signature metadata outside stable body hash. | No Pack.lock, IMAGE, Capsule, or signer-policy law. |
| Layered artifact identity | Separate raw bytes hash, canonical source hash, and lowered/behavior hash for deterministic artifact pipelines. | No Pack source/IR semantics. |
| Versioned spec pin verifier | `spec_version` plus schema hash, implementation/emitter hash, fixture hash, signer id, signer epoch, and drift-field reporting. | No MCP/A2A/AG-UI/Skill/OTel meanings. |
| Build-to-runtime evidence import | Import compile/build evidence hashes into runtime receipt stream as sealed references. | No PackCompiled / PackLockSealed / ImageActivated taxonomy. |
| Deterministic phase cache | DEFER/tooling: content-hash-keyed phase memoization with no timestamp invalidation and explicit upstream-state hash inputs. | No Pack Pipeline phase names. |
| Forensic query envelope | Read-only evidence rows with deterministic ordering, canonical JSONL/dCBOR output, and no storage-iteration-order dependence. | No Downstream audit filter grammar or named queries. |
| Merkle committed payload | Canonical content hash, Merkle path verification, and visibility-fenced publication of sealed payloads. | No `Capsule<T>`, attestation modes, or Pack criticality law. |
| Attested registry | Signed immutable entries with stable id, artifact hash, signer epoch, lifecycle state, deprecation chain, and drift check. | No transforms/dimensions/predicates/adapters as Pack-author law. |
| Schema validation descriptor | Reusable descriptor, field path, expected/observed shape, and violation report. | No Pack-declared type syntax or PackCompileError variants. |
| Idempotency ledger | Key + scope/region + TTL + prior append/receipt lookup + replay response primitive. | No operation write-intent, Gate, or Port semantics. |
| Region-bound query API | Typed coordinate-region query constructors and lintable ban on unbounded scans. | No tenant-first convention or capability policy. |
| Audit assertion runner | DEFER/helper crate: read-only assertions over receipt/query rows: no-result-where, every-result-has, count bounds, gap checks. | No admission law from audit checks. |
| Clock-skew evidence | HLC/frontier clamp observation: observed wall time, durable frontier, clamped HLC, and recurrence evidence. | No auth freshness or DPoP wall-time policy. |
| Fault-injection / audit snapshot hooks | Append fault injection points and coherent watermark/frontier snapshot hooks for tests. | No Downstream red-team fixture names. |
| Thread/process lifecycle evidence | Generic named worker/process started, ready, stopped, crashed, restarted, drain-began, drain-completed evidence. | No `mw-*` thread names, Highway, ExtProfile, SystemMission, or Port law. |
| Path-boundary normalization | Atomic no-follow open, normalized path evidence, symlink escape denial, and root-bound proof. | No Pack bind-mount or FileRead/FileWrite Capability semantics. |
| Service-supervisor evidence | Process invoked, readiness notification observed, exit code captured, restart intent recorded. | No HostDeploymentTarget or generated unit-file law. |
| Backup envelope | Self-contained sealed backup manifest, segment hashes, signature, bundled inspector metadata, and restore proof. | No Pack/IMAGE/Mission backup semantics. |
| Host resource ceiling observation | Keep above batpak unless batpak owns the runtime layer; generic store resource observations are separate. | No host config authority chain or adapter-specific auth. |

### Do Not Build In Batpak

These names are useful because they tell the batpak agent when to stop.

```text
batpak::Pack
batpak::PackLock
batpak::Mission
batpak::Session
batpak::Run
batpak::Capability
batpak::Criticality
batpak::Budget
batpak::PortKind
batpak::Mcp
batpak::A2A
batpak::ExtProfile
batpak::Image
batpak::Capsule
batpak::HumanApproval
batpak::ProtocolSpecName
batpak::InferencePort
batpak::HostDeploymentTarget
```

These are forbidden as public semantic types, enums, modules, traits, or feature names. Literal strings may appear in non-goals, fixtures, registry-row examples, or tests proving no vocabulary leak. A generic registry row with stable id `"extprofile"` is fine; `batpak::PcpContextBundle` or `batpak::ProtocolSpecName::Mcp` is not.

If a batpak design wants one of those public names, it is accidentally becoming Downstream.

## Downstream Shrink Ledger

Downstream docs stay large until these rows land in batpak. After a row ships, shrink the named Downstream owner sections to composition law plus a dependency citation.

| If batpak ships... | Downstream sections to shrink | Keep in Downstream |
|---|---|---|
| `ReceiptChainWalker` | `MW_FEATURES.md` receipt envelope audit prose; `MW_MACRO.md` receipt-stream walking/audit mechanism prose | receipt envelope fields, Downstream denial/authority/cause extensions |
| `ProjectionWatcher` / public projection runner | `MW_FEATURES.md` generic projection replay discipline; `MW_MACRO.md` projection runtime plumbing | Pack-declared projection targets, protocol/skill/OTel/SBOM meanings |
| `FrontierView` / `VisibilityFence` | repeated frontier state explanations across `MW_FEATURES.md`, `MW_RUNTIME.md`, `MW_HOST.md` readiness/recovery prose | which Downstream action advances which frontier and what receipt is emitted |
| `SchemaSnapshot` | `MW_MACRO.md` schema/wire snapshot harness mechanics | which Pack IR structs and protocol fixtures must be snapshotted |
| strengthened `batpak::canonical` contract | `MW_FEATURES.md` canonical encoding caveats; `MW_RUNTIME.md` source-verification prose | Pack.lock re-pin policy and IMAGE verification |
| `SubscriberFrontier` / `LagPolicy` | `MW_RUNTIME.md` push-down candidate row; `MW_MACRO.md` ExtProfile subscriber-lag mechanics | ExtProfile meaning, Highway-vs-ExtProfile distinction, Mission Control consumer behavior |
| `StoreResourceEnvelope` | `MW_RUNTIME.md` memory pressure mechanics | Pack-authored ceilings, host pools, Budget/Criticality/operator consequences |
| `Cursor` / `AtLeastOnce` / checkpoint helpers | cursor-reactor restart boilerplate in `MW_RUNTIME.md` / `MW_MACRO.md` | reaction-replay-vs-projection-replay law and idempotency composition |
| platform profile / future `TargetProfile` | platform/target probe prose in `MW_RUNTIME.md` and deployment profile compatibility prose in `MW_HOST.md` | Pack target declarations and host exposure law |
| `PathBoundaryEvidence` / optional `ProcessBoundaryEvidence` | path/sandbox-loader evidence mechanics in `MW_HOST.md` / `MW_MACRO.md` | Pack capability-to-sandbox-profile mapping and emitted profile policy |
| `SupervisorLifecycleEvent` | service-supervisor evidence mechanics in `MW_HOST.md` | `HostDeploymentTarget` artifact projection and operator runbook |
| generic IPC/event delivery | sandbox portal transport mechanics in `MW_MACRO.md` | admission semantics for operations crossing the portal |

Deletion rule:

> Once batpak owns a generic mechanism, Downstream should not keep a parallel tutorial for that mechanism. Downstream should keep only the Pack-authored meaning, the owner section, and the citation to the batpak API.

That is how the docs get smaller.

## Classification Meanings

- `READY_RFC` - clean non-agent substrate candidate; write a batpak RFC with minimal Downstream vocabulary.
- `SPLIT` - generic observation/accounting can move down; Downstream keeps policy consequences.
- `CANDIDATE` - likely reusable, but needs implementation pressure before extraction.
- `MOSTLY_DONE` - batpak already owns the important primitive; prefer direct composition.
- `UPSTREAM_STRENGTHEN` - not a new API first; improve batpak guarantee language/tests.
- `DEFER` - plausible abstraction, high risk of importing Downstream law.
- `DO_NOT_IMPORT_PACK_LAW` - leave in Downstream unless a genuinely generic primitive emerges.

## Non-Extraction Rules

- Do not make batpak an agent framework.
- Do not move Pack/Mission/Capability/Criticality/Budget law into batpak.
- Do not move protocol adapter behavior into batpak.
- Do not use this file as a canonical Downstream owner section.
- Do not cite this file from `MW_FEATURES.md`, `MW_PACK.md`, `MW_MACRO.md`, `MW_DSL.md`, or `MW_HOST.md` as behavior authority.

## MW_RUNTIME Citation Gate

A row graduates from handoff memo to Downstream dependency only when all are true:

1. batpak accepts and ships the domain-neutral API.
2. Downstream pins the batpak version that contains it.
3. `MW_RUNTIME.md` Part 2 cites the accepted batpak API as part of the boundary contract.
4. Existing Downstream owner sections shrink to cite the boundary instead of re-specifying the generic mechanism.

Until then, this file is coordination memory only.
