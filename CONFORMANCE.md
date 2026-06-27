# Conformance

Conformance is the factory contract made executable.

## Command Authority

The root `justfile` is the repository command authority.

Use:

```sh
just list
just inspect
just verify
just seal
just ship dry
```

Raw `cargo` commands are implementation details unless routed through the explicit escape hatch:

```sh
just cargo -- <args>
```

Repeated raw commands should become named `just` recipes.

## What The Commands Mean

| Command | Meaning |
| --- | --- |
| `just bench` | Benchmark or compile benchmark surfaces. |
| `just perf-gates` | Run ignored, hardware-dependent performance gates through the repo-owned xtask surface. |
| `just loom` | Run bounded loom schedule proofs through the repo-owned xtask surface. |
| `just inspect` | Structural doctrine, boundary checks, architecture IR, and ast-grep calipers. |
| `just verify` | Canonical preflight proof bundle. |
| `just seal` | Release-readiness checks for a clean tree. |
| `just ship dry` | Release dry run. |
| `just ship real` | Reserved manual publish path; xtask currently refuses non-dry-run release. |

## Verification tiers

The factory uses tiered verification so humans, agents, and CI all speak the same command language without moving policy into YAML.

| Tier | Human face | Policy face (xtask) | Blocks merge? | Meaning |
| --- | --- | --- | --- | --- |
| Inspect | `just inspect` | structural, boundary, architecture IR, ast-grep | Optional/path-based | Fast shape checks before expensive proof. |
| Host contract | `cargo test -p hostbat` | hostbat tests | No | `ClientManifest` projection, schema golden vectors, subscription descriptors. |
| NETBAT wire | `cargo test -p netbat` | netbat tests | No | NETBAT/1 goldens, stream runtime sessions, bounded request/response paths. |
| Factory ledger | `just ledger-list`, `just ledger-run -- …`, `just ledger-run-gate …` | `factory-ledger` | No | Opt-in local proof trail: command events plus optional named gate completions under `target/factory-ledger/store/`. Gates are proof markers, not CI enforcement. |
| Context packet | `just context` | `context` | No | Opt-in PCP-aligned handoff artifact under `target/context/latest.{json,md}`; captures git state, stack hints, ledger tail, boundary reminders. |
| Doctrine audit | traceability/docs artifact | traceability metadata | No | Records the substrate/product semantic firewall; not a CI or release gate unless promoted to an explicit command later. |
| Fast | `just ci-fast` | `ci-fast` | Yes | Early PR signal: format, clippy, checks, tests, dependency gates, traceability, structural law. |
| Integrity | `just verify` | `preflight` | Yes | Canonical Linux devcontainer proof: full CI, coverage threshold 80%, docs. |
| Windows surface | `just ci-windows` | `ci-windows-surface` | Yes | Native Windows compatibility surface, including platform-sensitive cargo/test behavior and kind-collision fixture. |
| Performance gates | `just perf-gates` | `perf-gates` | Manual / release proof | Hardware-dependent ignored tests for cold start, append/query/projection throughput, restore, and lifecycle timing. Run alone, not in parallel with other heavy Rust jobs. |
| Loom schedule proof | `just loom` | `loom` | Manual / label-gated CI | Bounded concurrency schedule proofs for writer/batch visibility, restart ownership, interner publication, group commit, and crash retry models. |
| Mutant smoke | `just mutants-smoke` | `mutants smoke` | Yes for Rust paths | Critical seam mutation gates; CI shards by seam, local runs all seams. |
| Mutant full | `just mutants-full` | `mutants full --shard …` | No | Manual repo-wide mutation ratchet; never scheduled in PR CI. |
| Release | `just seal`, `just ship dry` | seal / release manifest / dry-run release | Publish only | Packaging, manifests, provenance, and publish dry-run proof. |

Windows does not duplicate the canonical Linux devcontainer philosophy lane. It proves native surface compatibility. `just verify` remains the full merge-confidence bundle.

Linux `just verify` proves full canonical integrity (`ci` + coverage 80% + docs). Windows `just ci-windows` proves native surface compatibility only; the kind-collision composer fixture lives in xtask, not workflow YAML.

## Machine Law

Machine law lives in:

- `bpk-lib/traceability/`
- `bpk-lib/tools/integrity/`
- `bpk-lib/tools/xtask/`
- `sgconfig.yml` and `ast-grep/rules/`
- tests, benches, fixtures, and release manifests

Docs explain the current contract. Gates decide whether the contract still holds.
`traceability/product_doctrine_audit.yaml` is doctrine law for naming and
boundary review; it is not a runtime feature and not a CI gate by itself.

### Testing doctrine

The goal is proof density, not test count or a vanity coverage number: more
decisions covered per harness, clearer receipts for what each harness proves,
fewer story tests that walk one happy path. Coverage is a consequence of harness
density; where a delta is unmeasured, say so instead of pretending.

`bpk-lib/traceability/testing_ledger.yaml` is the machine-law ledger of
doctrine-bearing suites, validated by `just inspect`. Each entry names its
harness pattern, status, locations, commands, and the catalog `INV-*` ids it
witnesses. Every catalog invariant must have a direct test artifact, a ledger
entry, or a recorded waiver — the invariant bridge hard-fails otherwise. A suite
belongs in the ledger when it proves a named invariant or boundary contract,
would leave a real proof hole if deleted, is stable enough to run intentionally,
and points back to a concrete runtime seam.

Every doctrine-bearing harness must be deterministic; use no network, Docker, or
external services; fail closed rather than skip when it cannot decide; run under
`just verify`; add no new test-framework dependency; require no production
rewrite for testability without explicit approval; and split by seam or evidence
shape past roughly 500 lines. Its module header must declare `PROVES:`,
`CATCHES:`, and `SEEDED:` so a later reader knows why the file exists. Each
ledger entry's recorded commands are restricted to a narrow approved prefix set
(`cargo test`, gated chaos test, `cargo mutants`, or the repo xtask surface);
widening that set is a structural-lint policy change, not a per-entry escape
hatch.

Classify each suite by evidence shape, not subsystem, into one of the repo-owned
harness patterns: Oracle (obvious-correct reference vs optimized path), Property
(seeded/enumerated input families against invariants), State-Machine (protocol,
lifecycle, or bounded schedule where transitions matter), Equivalence (multiple
paths claiming the same semantics stay aligned), Fault-Injection (corruption,
bad input, or illegal shapes must fail structurally with no phantom success),
plus the Runtime-And-Boundary and Structural patterns the ledger also tracks.
One file gets one primary pattern. Do not invent a new pattern because a suite
feels special; tighten the existing ones instead.

## Terminal Manifest

The refbat manifest must expose the ten reference NETBAT operations — the six
core ops plus the four domain-neutral `evidence.*` ops:

- `system.heartbeat`
- `bank.commit`
- `event.get`
- `event.query`
- `receipt.verify`
- `event.walk`
- `evidence.chain_walk`
- `evidence.store_resource`
- `evidence.read_walk`
- `evidence.projection_run`

`event.query` keeps external replay substrate-complete without introducing a
wire cursor session. It pages by `global_sequence`; callers resume by sending
the previous response's `next_after_global_sequence` as the next
`after_global_sequence`. `event.walk` exposes bounded hash-chain ancestry only
— not DAG or Moonwalker graph law. Cursor-style open/next/checkpoint protocols
belong to a later NETBAT version or product-specific layer.

The `evidence.*` ops are thin wire adapters over the substrate evidence reports
`Store` already produces (`chain_walk_evidence`, `store_resource_evidence_report`,
`query_with_read_walk_evidence`, `project_run_evidence`). Each ack carries the
report body as a canonical blob (`report_hex`) plus its `body_hash` identity and
a `truncated` flag; a consumer re-hashes `report_hex` to confirm it equals
`body_hash`. Evidence requests use domain-neutral substrate selectors —
entity/scope prefixes, optional kind filters, optional per-entity clock range
on `evidence.read_walk`, projection ids on `evidence.projection_run`, and
event-id hex on `evidence.chain_walk` — and traversal returns evidence/metadata
only, never decoded domain payloads.
`evidence.projection_run` is dispatched through an embedder-populated projection
registry; the reference host registers none, so it answers every projection id
with an unknown-projection error.
