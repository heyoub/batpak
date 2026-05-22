# Agent Guide

## Repo Map

- `bpk-lib/Cargo.toml`: workspace/control-plane manifest; primary package defaults to `bpk-lib/crates/core`
- `bpk-lib/crates/core/`: primary package (`package.name = "batpak"`)
  - `bpk-lib/crates/core/src/`: runtime crate
  - `bpk-lib/crates/core/src/store/`: see `mod.rs` for full submodule list. Key subdirectories:
    - `write/` — `writer.rs` (orchestration spine), `writer/{append,batch,fence_runtime,publish,runtime}.rs`, `fanout.rs` (notifications), `staging.rs`, `control/`
    - `segment/` — `mod.rs` (frame format, compaction), `scan.rs` (segment reading), `sidx.rs` (SIDX footer)
    - `index/` — `mod.rs` (in-memory query engine), `columnar.rs` (SoA/AoSoA overlays), `interner.rs` (string interning)
    - `cold_start/` — `mod.rs` (open/restore orchestration), `checkpoint.rs`, `mmap.rs`, `rebuild.rs`
    - `platform/` — target-sensitive machine-contact helpers and evidence boundary: fs/sync/lock/clock/mmap
    - `projection/` — `mod.rs` (cache traits), `flow.rs` (replay + incremental apply), `watch.rs`
    - `ancestry/` — `mod.rs`, `by_hash.rs`, `by_clock.rs`
    - `delivery/` — `subscription.rs` (lossy push), `cursor.rs` (ordered pull replay with optional durable checkpoints), `observation.rs` (delivery witness types)
    - Flat files: `append.rs` (`BatchAppendItem`, `CausationRef`, `AppendOptions`), `gate.rs` (`DurabilityGate`), `lifecycle.rs`, `hidden_ranges.rs`, `config.rs`, `error.rs`, `stats.rs`, `reactor_typed.rs`
    - `fault.rs` — fault injection (dangerous-test-hooks feature)
  - `bpk-lib/crates/core/tests/`: integration, property, compile-fail, and perf-gate tests
  - `bpk-lib/crates/core/examples/`: runnable usage patterns
  - `bpk-lib/crates/core/benches/`: Criterion surfaces
  - `bpk-lib/crates/core/fixtures/`: downstream and cross-crate fixture packages
- `bpk-lib/crates/macros/`, `bpk-lib/crates/macros-support/`, `bpk-lib/crates/bench-support/`: companion workspace crates
- `bpk-lib/tools/integrity/`: traceability and structural detectors
- `bpk-lib/tools/xtask/`: canonical developer command surface
- `README.md`: primary repo entrypoint
- `FACTORY.md`, `MODEL.md`, `INVARIANTS.md`, `BATTERIES.md`, `TERMINALS.md`, `EVENTS.md`, `RECEIPTS.md`, `CIRCUITS.md`, `REPLAY.md`, `PROJECTIONS.md`, `INTEGRATION.md`, `CONFORMANCE.md`, `COOKBOOK.md`: canonical factory reading surface
- `cookbook/README.md`, `cookbook/200_*.md`: recipe library indexed by `COOKBOOK.md`
- `000_REPO_MAP.md`, `001_*.md`, `010_*.md`, `020_*.md`, `025_*.md`, `030_*.md`, `040_*.md`, `041_*.md`, `050_*.md`, `060_*.md`, `080_*.md`, `099_DECISION_INDEX.md`, `100_ADR_*.md`: legacy root docs retained during the ordnance cut until machine references are migrated
- `bpk-lib/traceability/`: requirements, invariants, flows, artifacts

## Root Altitudes

- Canonical source lives under `bpk-lib/crates/core/` and companion `bpk-lib/crates/*` members.
- Proof and validation live under `bpk-lib/crates/core/tests/`, `bpk-lib/crates/core/benches/`, `bpk-lib/crates/core/fixtures/`, `bpk-lib/traceability/`, and root `040_*/041_*` harness docs.
- Package-owned Cargo examples live under the owning crate. Today that means `bpk-lib/crates/core/examples/` for `batpak`; do not add root `examples/`.
- Runtime/network crates (`syncbat`, `netbat`) must have integration `tests/`. Proc-macro/support crates may be tested through their owning consumer crates instead of carrying empty `tests/` folders.
- Repo-owned Rust tools live under `bpk-lib/tools/`, with root `scripts/` reserved for CI/devcontainer boundary wrappers only.
- Public docs stay flat at root. The canonical reading surface is `README.md` plus the factory docs listed above; historical numbered docs are migration inputs until archived.
- Tool-standard config paths live where their tools require them: `bpk-lib/.cargo/` and `bpk-lib/.config/` for the Cargo workspace; root `.devcontainer/`, `.github/`, and `.githooks/` for repo/CI entrypoints.
- Agent/local workspace state (`.cursor/`, `.claude/`, `.codex/`, `.agents/`, `bpk-lib/target/`) is not substrate source.

## Canonical Commands

At repo root, agents use `just`. Raw `cargo`, `npm`, and `pnpm` are implementation details unless routed through an explicit escape hatch.

- `just list` — show the command surface
- `just inspect` — structural doctrine, boundary checks, architecture IR, and ast-grep calipers
- `just verify` — canonical preflight proof bundle
- `just seal` — release-readiness checks for a clean tree
- `just ship dry` — release dry run
- `just cargo -- <args>` — explicit Cargo escape hatch
- `just pnpm -- <args>` — explicit pnpm escape hatch
- `just npm -- <args>` — explicit npm escape hatch

Implementation commands still live under `bpk-lib/` and remain valid when a task specifically needs the machinery layer:

- `cd bpk-lib && cargo xtask doctor`
- `cd bpk-lib && cargo xtask install-hooks`
- `cd bpk-lib && cargo xtask preflight`     — canonical devcontainer verification bundle for CI + coverage + docs from one in-container session. Prefer this over bare `cargo xtask ci` for pushes that touch store internals, xtask itself, or CI config, but do not describe it as the full proof chain unless you also run the extra hard gates (`mutants smoke`, perf gates, targeted fuzz/chaos).
- `cd bpk-lib && cargo xtask ci`
- `cd bpk-lib && cargo xtask structural`
- `cd bpk-lib && cargo xtask layout` — discoverable alias for the repo layout contract enforced by structural
- `cd bpk-lib && cargo xtask boundary` — discoverable alias for stack dependency direction and runtime boundary discipline
- `cd bpk-lib && cargo xtask stale-paths` — discoverable alias for moved/retired path reference checks
- `cd bpk-lib && cargo xtask disk-audit` — read-only report for repo-local artifact/cache sprawl
- `cd bpk-lib && cargo xtask clean-generated [--apply]` — dry-run by default; removes only generated sprawl outside the Cargo workspace `target/`
- `cd bpk-lib && cargo xtask package-leak-scan [--allow-dirty] [--strict-language]` — builds the local `.crate` and scans package contents for leak-shaped text
- `cd bpk-lib && cargo xtask semver-check [--strict]` — release-oriented semver check; advisory by default during the 0.7.6 correction cut
- `cd bpk-lib && cargo xtask public-api [--strict]` — human-readable public API snapshot under `bpk-lib/target/`; advisory by default during the 0.7.6 correction cut
- `cd bpk-lib && cargo xtask evidence-audit` — static evidence-report schema anchors and prelude/store export vocabulary (runs `batpak-integrity evidence-audit`)
- `cd bpk-lib && cargo xtask agent-doctor` — fast agent-facing repair hints for topology, stale paths, templates, and surface-map drift
- `cd bpk-lib && cargo xtask architecture-ir [--out <path>] [--check]` — emit or verify the machine-readable architecture view under `bpk-lib/target/` by default
- `cd bpk-lib && cargo xtask scaffold <pattern> --name <name> [--path <dir>]`
- `cd bpk-lib && cargo xtask mutants policy`
- `cd bpk-lib && cargo xtask mutants smoke`
- `cd bpk-lib && cargo xtask platform doctor --store-path <dir>`
- `cd bpk-lib && cargo xtask platform probe --store-path <dir> --profile <file>`
- `cd bpk-lib && cargo xtask platform verify --store-path <dir> --profile <file>`
- `cd bpk-lib && cargo xtask platform bless --store-path <dir> --profile <file>`
- `cd bpk-lib && cargo xtask platform audit`
- `cd bpk-lib && cargo xtask perf-gates`
- `cd bpk-lib && cargo xtask bench --surface neutral|native [--save <baseline-label>|--compare|--compile]`
- `cd bpk-lib && cargo xtask templates`
- `cd bpk-lib && cargo xtask template-freshness` — focused template smoke plus generated-lock drift check
- `cd bpk-lib && cargo xtask staged-diff` — inspect staged files for generated artifacts, retired paths, and conflict markers
- `cd bpk-lib && cargo xtask release-manifest` — write a local proof summary under `bpk-lib/target/`
- `cd bpk-lib && cargo xtask public-api --strict --check-baseline` — verify the checked-in post-cleanup public API snapshot
- `cd bpk-lib && cargo xtask cover [--ci|--json|--threshold N]`
- `cd bpk-lib && cargo xtask docs`
- `cd bpk-lib && cargo xtask release --dry-run`

## Change Map

- Public API change:
  - update `README.md`, `EVENTS.md`, `RECEIPTS.md`, `REPLAY.md`, `PROJECTIONS.md`, `INTEGRATION.md`, or `CONFORMANCE.md` as appropriate
  - update examples if onboarding changed
  - update traceability if invariants/flows changed
- Store internals change:
  - run `just inspect`
  - run `just verify` when the change affects store behavior, xtask itself, or CI config
  - run the relevant perf surface
  - inspect `bpk-lib/crates/core/tests/perf_gates.rs` and the relevant factory root doc
- Benchmark harness change:
  - update `cargo xtask bench` surfaces in `bpk-lib/tools/xtask/src/bench.rs`
  - refresh baselines intentionally
  - keep backend-neutral vs backend-specific surfaces honest
- Coverage harness change:
  - update `bpk-lib/tools/xtask/src/coverage.rs`
  - keep JSON mode stdout-clean
  - keep retained artifacts under `bpk-lib/target/xtask-cover/last-run/`
- Docs-only change:
  - keep `README.md`, `MODEL.md`, `INVARIANTS.md`, `CONFORMANCE.md`, and related factory docs consistent

## Guardrails

- Do not introduce async runtime dependencies in production.
- Keep root-first commands and paths accurate.
- If you add a public item or named flow, update `bpk-lib/traceability/`.
- Prefer root `just` recipes over inventing new one-off local commands; use `xtask` for machinery that needs parsing, walking, validation, or receipts.
- **PCP boundary** — batpak may align with the sibling `PCP_SPEC`, but this crate
  does not implement PCP-Core or `contract.context_v1` wire validation. Treat
  `contract.context_v1` as a normative optional PCP profile only when
  `PCP_SPEC` is audit-clean; in batpak, PCP references are docs-only alignment
  unless a change explicitly adds codecs, tests, and traceability for a runtime
  surface. `authority_required` remains receiver-policy input, never granted
  authority.
- `.githooks/` is the tracked repo hook surface. `cargo xtask setup --install-tools` will install it when no custom `core.hooksPath` is active; otherwise use `cargo xtask install-hooks` after clearing or changing the custom hook path.
- **Structural parity checks** — `just inspect` runs the focused structural surface. The underlying `cd bpk-lib && cargo xtask structural` command (called automatically by `cargo xtask ci`) runs two detectors you must not break:
  - `check_ci_parity` — fails if `.github/workflows/ci.yml` drifts from the xtask source tree or `.devcontainer/Dockerfile`. Specifically: every `cargo xtask <subcommand>` referenced in the workflow must exist as a subcommand in xtask; every `taiki-e/install-action` tool must be present in xtask's setup step; tool version pins must agree across all three files. **Rule:** if you modify `bpk-lib/tools/xtask/src/main.rs`, `bpk-lib/tools/xtask/src/commands.rs`, `.github/workflows/ci.yml`, or `.devcontainer/Dockerfile`, run `cd bpk-lib && cargo xtask structural` before push.
  - `check_store_pub_fn_coverage` — uses `syn` to parse `bpk-lib/crates/core/src/store/`, extracts every `pub fn` on `impl Store`, and asserts that each one has at least one method-call reference somewhere in `bpk-lib/crates/core/tests/` or `bpk-lib/crates/core/src/`. Catches orphan public methods that ship untested and invisible to mutation testing. **Rule:** if you add a `pub fn` to `Store`, ensure it has a call site in tests or the check will fail.
- **Stack boundary checks** — `cd bpk-lib && cargo xtask boundary` is the focused name for the layer checks enforced by structural. It keeps `batpak` below `syncbat` and `syncbat` below `netbat`, while downstream kit/agent layers stay outside this workspace; it also rejects production async runtime dependencies and unsafe/async runtime shapes in the family crates.
- **Stale path checks** — `cd bpk-lib && cargo xtask stale-paths` is the focused name for structural checks that keep moved docs, retired scripts, old store paths, and pre-`bpk-lib` layout references from creeping back into live surfaces.

## Mutation Testing Gate

The `mutants` job in `ci.yml` runs on every `pull_request` and on main via `workflow_dispatch` or `schedule` — it is **not** report-only. `cargo xtask mutants smoke` is the repo-owned CI surface now: it runs the named critical seams first at an `85%` catch-rate threshold (`writer commit protocol`, `cursor delivery/checkpoint logic`, `projection replay/freshness logic`, `segment scan / corruption handling`, `hash-chain / replay consistency` across the feature lanes, platform backend admission/reverify, and testing-ledger linting), then runs repo-wide `1/48` shards on both feature surfaces under the current ratchet phase. Today the repo-wide phase is `Phase0` record-only, so xtask records the score and prints the next available ratchet floor without enforcing it yet. Run `cargo xtask mutants policy` to see the current thresholds and staged repo-wide floors from xtask itself.

**Rule:** if you delete a test, expect either a critical-seam threshold failure or a repo-wide score drop; replace it with an equivalent test or write a stronger one that subsumes its coverage.

## Test-Authoring Caveats

**`expect_err` is off-limits for `Store` and `Receipt` results.** The audit found five agent-authored sites that reached for `Result::expect_err`, which requires `T: Debug` on the `Ok` variant. Neither `Store` nor `Receipt<&str>` implements `Debug`. Use the explicit-panic pattern instead:

```rust
let err = match result {
    Ok(_) => panic!("PROPERTY: expected an error here but got Ok"),
    Err(e) => e,
};
assert!(matches!(err, StoreError::SpecificVariant { .. }), "wrong variant: {:?}", err);
```

Test files that use `panic!()` intentionally (as the loop-escape in property tests) need `#![allow(clippy::panic)]` at the module level. The project's `Cargo.toml` denies `panic` globally for `bpk-lib/crates/core/src/`, but test files use it on purpose and must opt out explicitly.

**Extract local visitor structs to module level for testability.** Visitor structs defined inside a function body (e.g., `U128Visitor`, `OptU128Visitor`, `VecU128Visitor` in `bpk-lib/crates/core/src/wire.rs`) are unreachable from `bpk-lib/crates/core/tests/` and invisible to mutation testing — mutations inside them go undetected. The fix is to move them to `pub(super) struct` at module level. Apply this pattern whenever you define a `serde::Visitor` or similar helper inside a function: the slight verbosity is worth the coverage gain.
