# Repository Map

The repository root is the command deck. It is intentionally readable with
`ls`, `find`, and `rg` before any tool-specific setup.

## Root Docs

Root Markdown files are canonical navigation and governance documents.

- `000_REPO_MAP.md`: repository grammar and reading order.
- `001_*` through `004_*`: batpak-family layer primers.
- `010_*` through `080_*`: operating manuals, references, runbooks, testing
  doctrine, security, contribution, and evidence docs.
- `099_DECISION_INDEX.md`: decision navigation index.
- `100_ADR_*.md`: architecture decision records.

## Cookbook

[`cookbook/`](cookbook/) contains applied, task-shaped recipes.

Recipes answer "how do I do this correctly?" They are not architecture
decisions and should not compete with ADRs, security posture, release runbooks,
or layer contracts.

## Implementation Workspace

`bpk-lib/` is the Rust workspace and implementation root.

- `bpk-lib/Cargo.toml`: workspace manifest and xtask entrypoint context.
- `bpk-lib/crates/core/`: primary `batpak` package.
- `bpk-lib/crates/syncbat/`, `bpk-lib/crates/clawbat/`, and
  `bpk-lib/crates/netbat/`: stack layers over the substrate.
- `bpk-lib/tools/`: repo-owned Rust tools.
- `bpk-lib/traceability/`: machine-readable requirements, invariants, flows,
  and artifact registries.

## Boundary Wrappers

Root `scripts/` and `justfile` are convenience boundaries only.

- `justfile` gives short local commands.
- `scripts/` crosses shell, PowerShell, and devcontainer boundaries.
- `bpk-lib/tools/xtask/` owns the actual repo logic.

If a wrapper starts making decisions, move that logic into xtask.
