# Agent Guide

## Repo Map

- `src/`: runtime crate
  - `src/store/`: see `mod.rs` for full submodule list. Key subdirectories:
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
- `tests/`: integration, property, compile-fail, and perf-gate tests
- `examples/`: runnable usage patterns
- `benches/`: Criterion surfaces
- `tools/integrity/`: traceability and structural detectors
- `tools/xtask/`: canonical developer command surface
- `README.md`: primary repo entrypoint
- `GUIDE.md`: human-first workflows and usage
- `REFERENCE.md`: technical reference and invariants
- `docs/adr/`: decision records; start with `docs/adr/README.md` for the index
- `traceability/`: requirements, invariants, flows, artifacts

## Canonical Commands

- `cargo xtask doctor`
- `cargo xtask install-hooks`
- `cargo xtask preflight`     — canonical devcontainer verification bundle for CI + coverage + docs from one in-container session. Prefer this over bare `cargo xtask ci` for pushes that touch store internals, xtask itself, or CI config, but do not describe it as the full proof chain unless you also run the extra hard gates (`mutants smoke`, perf gates, targeted fuzz/chaos).
- `cargo xtask ci`
- `cargo xtask structural`
- `cargo xtask evidence-audit` — static evidence-report schema anchors and prelude/store export vocabulary (runs `batpak-integrity evidence-audit`)
- `cargo xtask mutants policy`
- `cargo xtask mutants smoke`
- `cargo xtask platform doctor --store-path <dir>`
- `cargo xtask platform probe --store-path <dir> --profile <file>`
- `cargo xtask platform verify --store-path <dir> --profile <file>`
- `cargo xtask platform bless --store-path <dir> --profile <file>`
- `cargo xtask platform audit`
- `cargo xtask perf-gates`    — hardware-dependent catastrophic-regression guards, not precision perf gates. Run only on stable hardware; no current environment is both canonical and timing-stable, so these thresholds stay intentionally generous and are excluded from `cargo xtask ci`.
- `cargo xtask bench --surface neutral|native [--save|--compare|--compile]`
- `cargo xtask cover [--ci|--json|--threshold N]`
- `cargo xtask docs`
- `cargo xtask release --dry-run`

## Change Map

- Public API change:
  - update README, GUIDE, or REFERENCE as appropriate
  - update examples if onboarding changed
  - update traceability and ADRs if invariants/flows changed
- Store internals change:
  - run `cargo xtask ci`
  - run the relevant perf surface
  - inspect `tests/perf_gates.rs` and `REFERENCE.md`
- Benchmark harness change:
  - update `cargo xtask bench` surfaces in `tools/xtask/src/bench.rs`
  - refresh baselines intentionally
  - keep backend-neutral vs backend-specific surfaces honest
- Coverage harness change:
  - update `tools/xtask/src/coverage.rs`
  - keep JSON mode stdout-clean
  - keep retained artifacts under `target/xtask-cover/last-run/`
- Docs-only change:
  - keep root README, GUIDE, and REFERENCE consistent

## Guardrails

- Do not introduce async runtime dependencies in production.
- Keep root-first commands and paths accurate.
- If you add a public item or named flow, update `traceability/`.
- Prefer `cargo xtask` over inventing new one-off local commands.
- `.githooks/` is the tracked repo hook surface. `cargo xtask setup --install-tools` will install it when no custom `core.hooksPath` is active; otherwise use `cargo xtask install-hooks` after clearing or changing the custom hook path.
- **Structural parity checks** — `cargo xtask structural` (called automatically by `cargo xtask ci`) runs two detectors you must not break:
  - `check_ci_parity` — fails if `.github/workflows/ci.yml` drifts from the xtask source tree or `.devcontainer/Dockerfile`. Specifically: every `cargo xtask <subcommand>` referenced in the workflow must exist as a subcommand in xtask; every `taiki-e/install-action` tool must be present in xtask's setup step; tool version pins must agree across all three files. **Rule:** if you modify `tools/xtask/src/main.rs`, `tools/xtask/src/commands.rs`, `.github/workflows/ci.yml`, or `.devcontainer/Dockerfile`, run `cargo xtask structural` before push.
  - `check_store_pub_fn_coverage` — uses `syn` to parse `src/store/mod.rs`, extracts every `pub fn` on `impl Store`, and asserts that each one has at least one method-call reference somewhere in `tests/` or `src/`. Catches orphan public methods that ship untested and invisible to mutation testing. **Rule:** if you add a `pub fn` to `Store`, ensure it has a call site in tests or the check will fail.

## Mutation Testing Gate

The `mutants` job in `ci.yml` runs on every `pull_request` and on main via `workflow_dispatch` or `schedule` — it is **not** report-only. `cargo xtask mutants smoke` is the repo-owned CI surface now: it runs the named critical seams first at an `85%` catch-rate threshold (`writer commit protocol`, `cursor delivery/checkpoint logic`, `projection replay/freshness logic`, `segment scan / corruption handling`, `hash-chain / replay consistency` across the feature lanes, platform backend admission/reverify, and harness-ledger linting), then runs repo-wide `1/48` shards on both feature surfaces under the current ratchet phase. Today the repo-wide phase is `Phase0` record-only, so xtask records the score and prints the next available ratchet floor without enforcing it yet. Run `cargo xtask mutants policy` to see the current thresholds and staged repo-wide floors from xtask itself.

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

Test files that use `panic!()` intentionally (as the loop-escape in property tests) need `#![allow(clippy::panic)]` at the module level. The project's `Cargo.toml` denies `panic` globally for `src/`, but test files use it on purpose and must opt out explicitly.

**Extract local visitor structs to module level for testability.** Visitor structs defined inside a function body (e.g., `U128Visitor`, `OptU128Visitor`, `VecU128Visitor` in `src/wire.rs`) are unreachable from `tests/` and invisible to mutation testing — mutations inside them go undetected. The fix is to move them to `pub(super) struct` at module level. Apply this pattern whenever you define a `serde::Visitor` or similar helper inside a function: the slight verbosity is worth the coverage gain.
