# Contributing to `batpak`

## Canonical Environment

The checked-in devcontainer is the canonical environment. Native Windows and Linux are supported too, but they should use the same root-first commands:

```bash
cargo xtask doctor
cargo xtask ci
```

If your local toolchain is missing standard cargo helpers, run:

```bash
cargo xtask setup --install-tools
```

That setup command also installs the tracked `.githooks/` surface when no
custom `core.hooksPath` is active. If you keep a custom hooks path, batpak
will leave it alone; run `cargo xtask install-hooks` after clearing or changing
that config if you want the repo-managed pre-commit hook.

## Daily Commands

```bash
cargo xtask doctor
cargo xtask install-hooks
cargo xtask traceability
cargo xtask structural
cargo xtask pre-commit
cargo xtask ci
cargo xtask cover
cargo xtask mutants policy
cargo xtask mutants smoke
```

`just` recipes are wrappers around the same commands.

`cargo xtask doctor` warns when the repo-managed hooks are not installed.

## Contributor Workflow

1. Make the change.
2. Update docs, examples, traceability, and ADRs if the public surface or behavior changed.
3. Run `cargo xtask pre-commit`.
4. Run `cargo xtask preflight` before pushing. This enters the canonical devcontainer once, then runs CI, coverage, and docs from that single in-container proof session. It remains the closest local match to the GH `Integrity (ubuntu-devcontainer)` job, so it eliminates "passes locally, fails CI" surprises that `cargo xtask ci` on a native host cannot catch (different toolchain, missing system deps, wrong env vars). Use `cargo xtask ci` as a faster inner-loop check during iterative development, but always finish with `preflight` before the push that matters.

## Performance Surfaces

No current environment is both canonical and timing-stable.

- `cargo xtask ci` compiles the benchmark surfaces (`cargo xtask bench --compile`) as a build-integrity check only; it does not interpret timings.
- `cargo xtask perf-gates` is a catastrophic-regression guard with generous thresholds. Run it on stable hardware when you want a "something is badly wrong" signal.
- `.github/workflows/perf.yml` is Criterion trend collection, not a hard gate.
- `cargo xtask bench --surface ...` is the measurement lane for comparing surfaces and baselines intentionally.

## Mutation Policy

- `cargo xtask mutants policy` prints the repo-owned mutation policy from xtask itself.
- `cargo xtask mutants smoke` is the CI smoke lane: it runs the named critical seams first (`writer commit protocol`, `cursor delivery/checkpoint logic`, `projection replay/freshness logic`, `segment scan / corruption handling`, and `hash-chain / replay consistency`) and then repo-wide 1/48 ratchet shards on both feature surfaces.
- `cargo xtask mutants full` with no overrides runs the full policy locally. `cargo xtask mutants full --surface ... --shard ...` stays the targeted repo-wide lane for matrix jobs and focused investigation.
- Critical seams enforce an `85%` mutation-score threshold immediately. Repo-wide lanes use the staged ratchet phases owned by xtask; the current phase is `Phase0` record-only, which means xtask records the score and reports the next available floor without enforcing it yet.
- Mutation artifacts live under `tools/xtask/target/mutants/` so xtask owns the scratch surface.

## Public Surface Rules

- No async runtime dependency in production.
- No `unwrap()`, `todo!()`, `dbg!()`, or `panic!()` in production code.
- Keep host-specific linker and machine overrides out of the repo.
- New public APIs should have:
  - a doc comment or guide entry
  - an example or test by name
  - traceability updates when behavior or invariants change

## Docs And Release Hygiene

- `README.md` is the primary repo entrypoint and should stay enough for orientation
- `GUIDE.md` is the human-first usage surface
- `REFERENCE.md` is the technical reference surface
- ADRs live in `docs/adr/`

Before release-oriented changes, run:

```bash
cargo xtask docs
cargo xtask release --dry-run
```

If a change touches persistence artifacts or cold-start behavior, update the
release notes and reference docs explicitly. Operators need to know when reopen
falls back to scan and which older artifact versions load with additive root
defaults.

Coverage artifacts are retained under `target/xtask-cover/last-run/` so failed
or partial coverage runs can be inspected instead of disappearing into a temp
directory.
