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
| Fast | `just ci-fast` | `ci-fast` | Yes | Early PR signal: format, clippy, checks, tests, dependency gates, traceability, structural law. |
| Integrity | `just verify` | `preflight` | Yes | Canonical Linux devcontainer proof: full CI, coverage threshold 80%, docs. |
| Windows surface | `just ci-windows` | `ci-windows-surface` | Yes | Native Windows compatibility surface, including platform-sensitive cargo/test behavior and kind-collision fixture. |
| Mutant smoke | `just mutants-smoke` | `mutants smoke` | Yes for Rust paths | Critical seam mutation gates; CI shards by seam, local runs all seams. |
| Mutant full | `just mutants-full` | `mutants full --shard …` | No | Scheduled/manual repo-wide mutation ratchet. |
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

## Terminal Manifest

The hbat manifest must expose the four reference NETBAT operations:

- `system.heartbeat`
- `bank.commit`
- `event.get`
- `event.query`

`event.query` keeps external replay substrate-complete without introducing a
wire cursor session. Cursor-style open/next/checkpoint protocols belong to a
later NETBAT version or product-specific layer.
