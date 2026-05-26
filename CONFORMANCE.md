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

Raw `cargo`, `npm`, and `pnpm` commands are implementation details unless routed through an explicit escape hatch:

```sh
just cargo -- <args>
just pnpm -- <args>
just npm -- <args>
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
| `just ship real` | Real release path. |

## Verification tiers

The factory uses tiered verification so humans, agents, and CI all speak the same command language without moving policy into YAML.

| Tier | Human face | Policy face (xtask) | Blocks merge? | Meaning |
| --- | --- | --- | --- | --- |
| Inspect | `just inspect` | structural, boundary, architecture IR, ast-grep | Optional/path-based | Fast shape checks before expensive proof. |
| Host profile | `just host-dev` | `host-dev` | No | Local end-to-end host proof: manifest → codegen → TS build/test → hbat boot → heartbeat-spike → deterministic regeneration. |
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

## Terminal Manifest

The hbat manifest must expose the six reference NETBAT operations:

- `system.heartbeat`
- `bank.commit`
- `event.get`
- `event.query`
- `receipt.verify`
- `event.walk`

`event.query` keeps external replay substrate-complete without introducing a
wire cursor session. It pages by `global_sequence`; callers resume by sending
the previous response's `next_after_global_sequence` as the next
`after_global_sequence`. `event.walk` exposes bounded hash-chain ancestry only
— not DAG or Moonwalker graph law. Cursor-style open/next/checkpoint protocols
belong to a later NETBAT version or product-specific layer.
