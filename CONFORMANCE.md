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

## Machine Law

Machine law lives in:

- `bpk-lib/traceability/`
- `bpk-lib/tools/integrity/`
- `bpk-lib/tools/xtask/`
- `sgconfig.yml` and `ast-grep/rules/`
- tests, benches, fixtures, and release manifests

Docs explain the current contract. Gates decide whether the contract still holds.

