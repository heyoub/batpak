# Agent Guide

## ⛔ NON-NEGOTIABLE DOCTRINE — read before writing a single line

**1. ZERO `#[allow]`. Repo-wide. No exceptions.**
There are **zero** `#[allow(...)]`, `#![allow(...)]`, and `#[expect(...)]` attributes in
the entire workspace (`crates/`, `tools/`, tests, examples, benches). This is **enforced**
by an armed tripwire — an AST detector in `bpk-lib/crates/core/build_support/shared_checks.rs`
(mirrored in `bpk-lib/tools/integrity/src/structural.rs`) that **fails the build** the instant
any allow/expect attribute appears in tracked Rust. It is red-proven (planting a real
`#[allow(...)]` makes `cargo xtask structural` fail). **You cannot silence a lint. Do not
try.** The only occurrences that survive are *text data* — raw-string fixtures inside the
detector's own self-tests and error-message strings — never live attributes.

**Cure type-first, never silence.** A lint firing on production code is the type system
telling you it is underused. The fix is one of:
- The right type already exists and just needs *wiring* (e.g. route a value through it), **or**
- The right type exists and needs *one variant/field added* (e.g. a new typed error arm).
A production `expect`/`unwrap`/`panic`/`as`-cast on an invariant means "encode the invariant
in the type," not "add an allow." Patterns we use, by situation:
- **Storage↔compute width** (u32/u64 on disk/wire → usize in compute): convert at the honest
  boundary. Trusted-infallible: `usize::try_from(x).unwrap_or(usize::MAX)` (total, lint-clean;
  the `target_pointer_width >= 32` guard in `lib.rs` makes `Err` unreachable). **Untrusted disk
  bytes: `usize::try_from(x).map_err(|_| StoreError::CorruptSegment…)?`** — never `unwrap_or`,
  which would *hide* corruption. Capacity ceiling: typed error via `?`.
- **Intentional runtime panic in a test/handler:** `assert!(std::hint::black_box(false), "…")`
  — panics at runtime, dodges `assertions_on_constants` (condition isn't constant) and isn't
  `clippy::panic`. **But NOT near `Instant::now()`/`Duration`** (trips the wall-clock detector
  GAUNT-FLAKE-7) — use `unreachable!()` there.
- **Typestate / data-carrying markers** to make an invariant unrepresentable (see the
  `Store<Open>` writer: `Open(WriterHandle)` + `StoreState::shutdown_writer`).

**2. `panic!`/`.unwrap()` are denied EVERYWHERE (tests included); `.expect("msg")` is the
sanctioned bail in tests; error-path tests assert the EXACT variant.**
`panic = "deny"` and `unwrap_used = "deny"` are **global** workspace clippy lints — they fire
in test code too, which is why there are **zero** `panic!`/`.unwrap()` in `tests/`.
`.expect()` is denied in production but **allowed in tests BY DESIGN** — the lint is
`#![cfg_attr(not(test), deny(clippy::expect_used))]`, and `invariants.yaml`
(INV-TEST-PANIC-AS-ASSERTION) endorses `.expect()`/`.expect_err()` as the test failure
signal. (Do NOT "cure" this by converting the ~4.4k `.expect()` sites to `?` — that is
ritual, not rigor: `.expect("what step failed")` gives BETTER diagnostics than a
`?`-propagated error, changes no assertion's strictness, and the lint deliberately permits
it.) So in **test code**:
- SETUP / preconditions ("the store must open"): `.expect("clear message")` — bails with your
  message + the real error + the line. Never `.unwrap()` (generic, lint-denied).
- The PROPERTY under test: assert the **EXACT** outcome, never just "it errored". For an error
  path, extract the error and check its VARIANT:
  `let e = op().expect_err("PROPERTY: <what>"); assert!(matches!(e, StoreError::Variant { .. }))`
  — a test that passes on the WRONG error is **vacuous**. Bare `expect_err`/`is_err()` with no
  variant check is acceptable ONLY when the property has a single failure mode (e.g. "this
  malformed input is rejected" — any error means rejected).
- `Result::expect_err` is forbidden on `Store`/`Receipt` (they lack `Debug`, so it will not
  compile) — there, use the return-`Result` + `Err(e)`-arm + `assert!(matches!(e, …))` pattern.

**3. Subagents inherit this doctrine.** If you dispatch an agent to edit Rust, paste the
relevant rules above into its prompt verbatim. Agents that don't know the doctrine *will* add
an allow to make the compiler happy and trip the wire. Also tell them to run
`cd bpk-lib && cargo xtask structural` (not just clippy) — the structural detectors catch
`repo_hygiene`/stale-path/allow violations that plain clippy misses.

**4. Concurrency is SYNC-FIRST + `flume`, never an async runtime — and the launcher child
window is async-signal-safe ONLY (no channels there).** Two rules that are easy to conflate
but are about *different* `async`es:
- **Library concurrency = `flume`, not tokio.** The `Store` API is synchronous and the
  workspace stays runtime-agnostic — **no `tokio`, no `async fn`/`-> impl Future` in the public
  surface** (INV-NO-TOKIO-PROD, INV-STORE-SYNC-ONLY; both currently weak-tier in
  `invariants.yaml`). Cross-thread coordination uses `flume` (v0.12) bounded channels: a
  **bidirectional highway** of a pull lane + a push lane, with ordering/visibility reconciled by
  the **HLC** (or another clock where HLC isn't apt — e.g. `WatermarkAdvanceHandle`). Async
  callers bridge via `recv_async()` / `spawn_blocking()`. Reference shapes: the writer command
  loop + `bounded(1)` reply tickets (`core/src/store/write_api.rs`, `lifecycle.rs`,
  `writer/runtime.rs`); rationale in `core/build.rs` + `core/src/lib.rs`. Don't reach for
  tokio/`async fn`; reach for a flume lane. A 1-bit level-triggered shutdown is correctly an
  `Arc<AtomicBool>`, not a channel — flume is for *delivering values between threads*.
- **The launcher's post-fork → pre-`fexecve` child window is the one place `flume` is BANNED.**
  Between `clone3`/fork and `execve` the child has only the calling thread but inherits every
  lock another parent thread held, so **`malloc` / any lock → deadlock**. Only async-signal-safe
  primitives are legal there. `flume` (and every channel, `Mutex`, allocation) is FORBIDDEN in
  any `pre_exec` closure and in the `crates/bvisor/.../sys.rs` unsafe basements. The discipline:
  **build everything that allocates in the PARENT before the fork** (landlock ruleset, plan
  parse, fd relocation) and run only alloc-free ops in the child (fd scrub → `fchdir` →
  `restrict_self` → `fexecve`). The host/launcher *parent* (before fork) is normal threaded code
  where flume is fine. A new channel/alloc in a `pre_exec` body is exactly how an
  async-signal-safety over-claim gets reintroduced. Witnessed (coarsely) by the launcher
  determinism oracles that drive the full child window repeatedly without deadlock —
  `crates/bvisor/tests/launcher_inherited_fds_linux.rs::fd_scrub_is_deterministic_across_runs`,
  `launcher_env_linux.rs::environment_isolation_is_deterministic_across_runs`. FOLLOW-UP (named
  gap, not fabricated): promote this to a machine invariant + a structural gate that proves every
  `pre_exec` closure is allocation-free, once bvisor joins the `traceability/` catalog universe.

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
  - `bpk-lib/crates/batpak-examples/src/bin/`: runnable usage demos
  - `bpk-lib/crates/core/benches/`: Criterion surfaces
  - `bpk-lib/crates/core/fixtures/`: downstream and cross-crate fixture packages
- `bpk-lib/crates/macros/`, `bpk-lib/crates/macros-support/`, `bpk-lib/crates/bench-support/`: companion workspace crates
- `bpk-lib/tools/integrity/`: traceability and structural detectors
- `bpk-lib/tools/xtask/`: canonical developer command surface
- `README.md`: primary repo entrypoint
- `01_FACTORY.md`, `02_MODEL.md`, `03_INVARIANTS.md`, `04_BATTERIES.md`, `05_TERMINALS.md`, `06_EVENTS.md`, `07_RECEIPTS.md`, `08_CIRCUITS.md`, `09_REPLAY.md`, `10_PROJECTIONS.md`, `11_INTEGRATION.md`, `12_CONFORMANCE.md`: canonical factory reading surface
- `cookbook/README.md`, `cookbook/200_*.md`: recipe library indexed by `cookbook/README.md`
- `archive/decisions/099_DECISION_INDEX.md`, `archive/decisions/100_ADR_*.md`: historical decisions; not the public reading path
- `bpk-lib/traceability/`: requirements, invariants, flows, artifacts

## Root Altitudes

- Canonical source lives under `bpk-lib/crates/core/` and companion `bpk-lib/crates/*` members.
- Proof and validation live under `bpk-lib/crates/core/tests/`, `bpk-lib/crates/core/benches/`, `bpk-lib/crates/core/fixtures/`, and `bpk-lib/traceability/` (including the machine-law testing ledger `bpk-lib/traceability/testing_ledger.yaml`). The testing doctrine itself lives in `12_CONFORMANCE.md`.
- Runnable demos live in the family-wide `bpk-lib/crates/batpak-examples/` crate (`src/bin/` binary targets); do not add root `examples/` or per-crate `examples/` folders.
- Runtime/network crates (`syncbat`, `netbat`) must have integration `tests/`. Proc-macro/support crates may be tested through their owning consumer crates instead of carrying empty `tests/` folders.
- Repo-owned Rust tools live under `bpk-lib/tools/`, with root `scripts/` reserved for CI/devcontainer boundary wrappers only.
- Public docs stay flat at root. The canonical reading surface is `README.md` plus the factory docs listed above; historical numbered docs are migration inputs until archived.
- Tool-standard config paths live where their tools require them: `bpk-lib/.cargo/` and `bpk-lib/.config/` for the Cargo workspace; root `.devcontainer/`, `.github/`, and `.githooks/` for repo/CI entrypoints.
- Agent/local workspace state (`.cursor/`, `.claude/`, `.codex/`, `.agents/`, `bpk-lib/target/`) is not substrate source.

## Canonical Commands

At repo root, agents use `just`. Raw `cargo` is an implementation detail unless routed through the explicit escape hatch.

- `just list` — show the command surface
- `just inspect` — structural doctrine, boundary checks, architecture IR, and ast-grep calipers
- `just ledger-list` — list recent factory command proof events from the opt-in ledger store
- `just ledger-run -- <command>` — run a command through the opt-in factory ledger wrapper
- `just ci-fast` — early PR signal (format, clippy, checks, tests, dependency gates, traceability, structural)
- `just verify` — canonical preflight proof bundle
- `just ci-windows` — native Windows surface compatibility lane
- `just perf-gates` — hardware-dependent performance gates; run alone when proving perf posture
- `just loom` — bounded loom schedule proofs
- `just seal` — release-readiness checks for a clean tree
- `just ship dry` — release dry run
- `just cargo -- <args>` — explicit Cargo escape hatch

Implementation commands still live under `bpk-lib/` and remain valid when a task specifically needs the machinery layer:

- `cd bpk-lib && cargo xtask doctor`
- `cd bpk-lib && cargo xtask install-hooks`
- `cd bpk-lib && cargo xtask factory-ledger list`
- `cd bpk-lib && cargo xtask factory-ledger run -- <command> [args...]`
- `cd bpk-lib && cargo xtask factory-ledger run --gate <name> -- <command> [args...]` — on success, records command completed + named gate completed
- `cd bpk-lib && cargo xtask factory-ledger record gate-completed --run-id … --gate … --command … --duration-ms … --summary …` — manual gate record (any status_code; for tests/import hooks)
- `cd bpk-lib && cargo xtask context [--ledger-limit N] [--notes TEXT]` — PCP-aligned handoff packet under `target/context/`; local capture only, not PCP-Core runtime
- `cd bpk-lib && cargo xtask ci-fast`       — early PR signal; version pins, format, clippy, checks, nextest, deny/audit, traceability, structural
- `cd bpk-lib && cargo xtask preflight`     — canonical devcontainer verification bundle for CI + coverage + docs from one in-container session. Prefer this over bare `cargo xtask ci` for pushes that touch store internals, xtask itself, or CI config, but do not describe it as the full proof chain unless you also run the extra hard gates (`mutants smoke`, perf gates, targeted fuzz/chaos).
- `cd bpk-lib && cargo xtask ci`            — full merge bundle (`ci-fast` plus doctor, templates, public-api, package-leak-scan, bench compile, unused-deps advisory)
- `cd bpk-lib && cargo xtask ci-windows-surface` — native Windows surface lane (not duplicate canonical Linux proof)
- `cd bpk-lib && cargo xtask structural`
- `cd bpk-lib && cargo xtask layout` — discoverable alias for the repo layout contract enforced by structural
- `cd bpk-lib && cargo xtask boundary` — discoverable alias for stack dependency direction and runtime boundary discipline
- `cd bpk-lib && cargo xtask stale-paths` — discoverable alias for moved/retired path reference checks
- `cd bpk-lib && cargo xtask disk-audit` — read-only report for repo-local artifact/cache sprawl
- `cd bpk-lib && cargo xtask clean-generated [--apply]` — dry-run by default; removes only generated sprawl outside the Cargo workspace `target/`
- `cd bpk-lib && cargo xtask package-leak-scan [--allow-dirty] [--strict-language]` — builds the local `.crate` and scans package contents for leak-shaped text
- `cd bpk-lib && cargo xtask semver-check [--strict]` — release-oriented semver check; strict in the release path
- `cd bpk-lib && cargo xtask public-api [--strict]` — human-readable public API snapshot under `bpk-lib/target/`; strict in the release path
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
- `cd bpk-lib && cargo xtask loom`
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
  - update `README.md`, `06_EVENTS.md`, `07_RECEIPTS.md`, `09_REPLAY.md`, `10_PROJECTIONS.md`, `11_INTEGRATION.md`, or `12_CONFORMANCE.md` as appropriate
  - update examples if onboarding changed
  - update traceability if invariants/flows changed
- Store internals change:
  - run `just inspect`
  - run `just verify` when the change affects store behavior, xtask itself, or CI config
  - run the relevant perf surface
  - inspect `bpk-lib/crates/core/tests/perf_gates.rs` and the relevant factory root doc
- hostbat manifest or subscription descriptor change:
  - run `cargo test -p hostbat`
  - run `just verify` when wire or host contract surfaces change
- netbat stream runtime or NETBAT wire change:
  - run `cargo test -p netbat`
  - run `just verify` when the change affects CI-facing proof
- Benchmark harness change:
  - update `cargo xtask bench` surfaces in `bpk-lib/tools/xtask/src/bench.rs`
  - refresh baselines intentionally
  - keep backend-neutral vs backend-specific surfaces honest
- Coverage harness change:
  - update `bpk-lib/tools/xtask/src/coverage.rs`
  - keep JSON mode stdout-clean
  - keep retained artifacts under `bpk-lib/target/xtask-cover/last-run/`
- Docs-only change:
  - keep `README.md`, `02_MODEL.md`, `03_INVARIANTS.md`, `12_CONFORMANCE.md`, and related factory docs consistent

## Guardrails

- Do not introduce async runtime dependencies in production.
- Keep root-first commands and paths accurate.
- If you add a public item or named flow, update `bpk-lib/traceability/`.
- Prefer root `just` recipes over inventing new one-off local commands; use `xtask` for machinery that needs parsing, walking, validation, or receipts.
- **Bidirectional substrate lane** — if a NETBAT terminal can commit substrate
  events, it must also preserve bounded domain-neutral traversal. The reference
  loop is `bank.commit` for write, `event.get` for point-read, `event.query`
  for commit-order log walking, `receipt.verify` for ack-shaped proof checks, and
  `event.walk` for bounded hash-chain ancestry (relation order, not DAG law).
  New traversal fields must name the axis as `global_sequence` when the axis is
  commit order. `after_global_sequence` is an exclusive resume point, not a
  stream cursor or server-held session; do not introduce ambiguous cursor names.
- **Domain graph boundary** — do not add Moonwalker, workflow, mission, or
  receipt-body verbs as batpak/refbat/netbat operations. Domain layers decode
  envelope payloads above batpak after `event.query` + `event.get`; substrate
  traversal returns metadata only.
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

The `mutants` surface is intentionally **not** automatic on every pull request. Default PR CI is the cheap fast lane. Run mutation proof explicitly with the `run-mutants` or `run-heavy-ci` pull-request label, or via `workflow_dispatch` with the `mutants` / `heavy` proof profile. There is no scheduled full-mutation run in `ci.yml`; full mutation is manual-only through the `heavy` proof profile or a local `just mutants-full` run. `cargo xtask mutants smoke` is the repo-owned CI surface now: it runs the named critical seams first at an `85%` catch-rate threshold (`writer commit protocol`, `cursor delivery/checkpoint logic`, `projection replay/freshness logic`, `segment scan / corruption handling`, `hash-chain / replay consistency` across the feature lanes, platform backend admission/reverify, and testing-ledger linting), then runs repo-wide `0/48` shards on both feature surfaces under the current ratchet phase.

**The repo-wide ratchet is now BLOCKING — the RecordOnly/`Phase0` record-only sentinel was deleted (GAUNT-MUT-4).** The current phase is **`Phase4`, a hard floor of 75%**: a repo-wide score below 75 fails CI today. The floor is **provisional pending the first cloud repo-wide smoke** — if the cloud-measured score is below 75 it's a one-line drop in `bpk-lib/tools/xtask/src/commands/mutants/policy.rs` to the highest staged phase ≤ measured (phases: P1=35, P2=50, P3=65, P4=75, **P5=85** = the climbed target). The ratchet is **monotonic** — the floor only ever climbs; `next_ratchet_floor` advertises the next available raise. Run `cargo xtask mutants policy` to see the live phase, floor, and staged thresholds from xtask itself.

**Rule:** if you delete a test, expect either a critical-seam threshold failure or a repo-wide score drop; replace it with an equivalent test or write a stronger one that subsumes its coverage.

## Test-Authoring Caveats

**`expect_err` is off-limits for `Store` and `Receipt` results.** The audit found five agent-authored sites that reached for `Result::expect_err`, which requires `T: Debug` on the `Ok` variant. Neither `Store` nor `Receipt<&str>` implements `Debug`.

**Do NOT replace it with `panic!()`** — `panic = "deny"` is a global lint (tests included) and there is **zero** `#[allow]` budget to opt out (see the doctrine block at the top). The repo has **zero** `panic!()` in `tests/`. Instead, make the test return `Result` and surface the violation as a returned `Err` — this is the live pattern across `tests/` (e.g. `platform_backend.rs`, `read_walk_evidence_report.rs`):

```rust
#[test]
fn invalid_input_must_be_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let err = match result {
        Ok(_) => {
            return Err(std::io::Error::other(
                "PROPERTY: expected an error here but got Ok",
            )
            .into())
        }
        Err(e) => e,
    };
    assert!(matches!(err, StoreError::SpecificVariant { .. }), "wrong variant: {err:?}");
    Ok(())
}
```

For an unconditional intentional failure inside non-`Result` test/handler bodies, use
`assert!(std::hint::black_box(false), "PROPERTY: <what was violated>")` (24 sites today) — it
panics at runtime, isn't `clippy::panic`, and the non-constant condition dodges
`assertions_on_constants`. Exception: do **not** use it near `Instant::now()`/`Duration`
(trips the wall-clock flake detector GAUNT-FLAKE-7) — use `unreachable!()` there.

**Extract local visitor structs to module level for testability.** Visitor structs defined inside a function body (e.g., `U128Visitor`, `OptU128Visitor`, `VecU128Visitor` in `bpk-lib/crates/core/src/wire.rs`) are unreachable from `bpk-lib/crates/core/tests/` and invisible to mutation testing — mutations inside them go undetected. The fix is to move them to `pub(super) struct` at module level. Apply this pattern whenever you define a `serde::Visitor` or similar helper inside a function: the slight verbosity is worth the coverage gain.
