# Decision Index

Architecture Decision Records live as flat root `100_ADR_*.md` files. All
current ADRs are accepted; shipped notes identify the first release where a
decision became part of the public or repository contract when that release is
known.

For the cross-ADR evidence-report identity pattern, see
`080_EVIDENCE_REPORTS.md`.

Agent/human transcription rails live as `cookbook/200_*.md` files and are
indexed by `bpk-lib/traceability/agent_surface.yaml`.

Root layer docs:

| Doc | Layer |
| --- | --- |
| [`001_BATPAK_SUBSTRATE.md`](001_BATPAK_SUBSTRATE.md) | `bp` substrate |
| [`002_SYNCBAT_RUNTIME.md`](002_SYNCBAT_RUNTIME.md) | `sb` sync runtime |
| [`003_CLAWBAT_KIT.md`](003_CLAWBAT_KIT.md) | `cb` operation kit |
| [`004_NETBAT_NETWORK.md`](004_NETBAT_NETWORK.md) | `nb` network/server boundary |
| [`025_VOCABULARY.md`](025_VOCABULARY.md) | canonical naming and public-surface correction target |

| ADR | Title | Status |
| --- | --- | --- |
| ADR-0001 | Sync-Only Store API | Accepted |
| ADR-0002 | Single Writer Thread Commit Path | Accepted |
| ADR-0003 | Projection Cache Safety and Capability Signaling | Accepted; motivating backend removed in 0.3.0 |
| ADR-0004 | Compaction and Concurrent Appends | Accepted |
| ADR-0005 | Test Support Feature Boundary | Accepted |
| ADR-0006 | Writer Restart Policy | Accepted |
| ADR-0007 | Unified Store Control Surface And Fast-Start Restore | Accepted |
| ADR-0008 | Restore Planner and Projection Trait Evolution | Accepted |
| ADR-0009 | Position Hints and Artifact Upgrade Contract | Accepted |
| ADR-0010 | EventPayload Macro Surface | Accepted; shipped in 0.6.0 |
| ADR-0011 | Reactor Canal | Accepted; shipped in 0.6.0 |
| ADR-0012 | No Dead-Code Silencers | Accepted |
| ADR-0013 | Substrate-owner Performance Findings | Accepted |
| ADR-0014 | Durable Frontier Observability | Accepted; shipped in 0.7.0 |
| ADR-0015 | dm-flakey Chaos Harness | Accepted; shipped in 0.7.0 |
| ADR-0016 | Durability Gating | Accepted; shipped in 0.7.0 |
| ADR-0017 | At-Least-Once Witness Surface | Accepted; shipped in 0.7.0 |
| ADR-0018 | Store Platform Backend | Accepted |
| ADR-0019 | Canonical Encoding Compatibility Contract | Accepted |
| ADR-0020 | Schema Snapshot Drift Evidence Report | Accepted |
| ADR-0021 | Chain Walk Evidence Report | Accepted |
| ADR-0022 | Subscriber Frontier Observations | Accepted |
| ADR-0023 | Projection Run Evidence Report (Design Precursor) | Accepted; superseded for v1 implementation by ADR-0024 |
| ADR-0024 | Projection Run Evidence Report v1 | Accepted |
| ADR-0025 | Read Walk Evidence Report v1 | Accepted |
| ADR-0026 | Pre-1.0 Public Surface Strategy | Accepted; correction strategy for 0.7.6 |
