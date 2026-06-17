# Decision Index

Architecture Decision Records live in `archive/decisions/`. They are historical
decision material, not the public reading path. All current ADRs are accepted;
shipped notes identify the first release where a decision became part of the
public or repository contract when that release is known.

For the cross-ADR evidence-report identity pattern, see
`../../RECEIPTS.md`.

Agent/human transcription rails live as `cookbook/200_*.md` files and are
indexed by `bpk-lib/traceability/agent_surface.yaml`.

Root layer docs:

| Doc | Layer |
| --- | --- |
| [`../../BATTERIES.md`](../../BATTERIES.md) | shipped battery family |
| [`../../MODEL.md`](../../MODEL.md) | canonical current ontology |
| [`../../INVARIANTS.md`](../../INVARIANTS.md) | narrative invariants; traceability remains machine law |
| [`../../CONFORMANCE.md`](../../CONFORMANCE.md) | command and proof contract |

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
| ADR-0027 | Snapshot Evidence Report v1 | Accepted |
| ADR-0028 | Syncbat Runtime Contract | Accepted; 0.7.6 correction cut |
| ADR-0029 | Netbat Boundary Contract | Accepted; 0.7.6 correction cut |
| ADR-0030 | Netbat Streaming Contract | Draft contract; keyed to batpak stream item vocabulary |
| ADR-0031 | 0.7.6 Release Proof Posture | Accepted; 0.7.6 correction cut |
| ADR-0032 | SIDX SDX3 Integrity Footer | Accepted; 0.8.3 audit-remediation cut |
| ADR-0033 | 0.8.3 Hardening Posture | Accepted; 0.8.3 audit-remediation cut |
