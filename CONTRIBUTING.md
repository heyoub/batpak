# Contributing

## Canonical Environment

The checked-in devcontainer is the canonical environment. Native Windows and Linux are supported too, but they should use the same root-first commands:

```bash
just doctor
just verify
```

The Rust workspace lives at `bpk-lib/Cargo.toml`; the project root is the
factory control plane. Use root `just` recipes unless a command explicitly says
otherwise.

If your local toolchain is missing standard cargo helpers, run:

```bash
just install-hooks
```

That setup command also installs the tracked `.githooks/` surface when no
custom `core.hooksPath` is active. If you keep a custom hooks path, batpak
will leave it alone; run `just install-hooks` after clearing or changing
that config if you want the repo-managed pre-commit hook.

## Daily Commands

```bash
just doctor
just install-hooks
just traceability
just inspect
just pre-commit
just verify
just cover
just mutants-smoke
```

`just` recipes are wrappers around the same `xtask` machinery.

`just doctor` warns when the repo-managed hooks are not installed.

## Contributor Workflow

1. Make the change.
2. Update docs, examples, and traceability if the public surface or behavior changed.
3. Run `just pre-commit`.
4. Run `just verify` before pushing. This enters the canonical devcontainer once, then runs CI, coverage, and docs from that single in-container proof session. It remains the closest local match to the GH `Integrity (ubuntu-devcontainer)` job, so it eliminates "passes locally, fails CI" surprises that a native host cannot catch (different toolchain, missing system deps, wrong env vars). Use faster inner-loop recipes during iterative development, but finish with `just verify` before the push that matters.

## Performance Surfaces

No current environment is both canonical and timing-stable.

- `just ci` compiles the benchmark surfaces (`just bench-compile`) as a build-integrity check only; it does not interpret timings.
- `just cargo -- xtask perf-gates` is a catastrophic-regression guard with generous thresholds. Run it on stable hardware when you want a "something is badly wrong" signal.
- `.github/workflows/perf.yml` is Criterion trend collection, not a hard gate.
- `just bench-save` and `just bench-compare` are the measurement lanes for comparing surfaces and baselines intentionally.

## Mutation Policy

- `just cargo -- xtask mutants policy` prints the repo-owned mutation policy from xtask itself.
- `just mutants-smoke` is the CI smoke lane: it runs the named critical seams first (`writer commit protocol`, `cursor delivery/checkpoint logic`, `projection replay/freshness logic`, `segment scan / corruption handling`, `hash-chain / replay consistency`, platform backend admission/reverify, and testing-ledger linting) and then repo-wide 0/48 ratchet shards on both feature surfaces.
- `just mutants-full` with no overrides runs the full policy locally. `just cargo -- xtask mutants full --surface ... --shard ...` stays the targeted repo-wide lane for matrix jobs and focused investigation.
- Critical seams enforce an `85%` mutation-score threshold immediately. Repo-wide lanes use the staged ratchet phases owned by xtask; the current phase is `Phase0` record-only, which means xtask records the score and reports the next available floor without enforcing it yet.
- Mutation artifacts live under `target/xtask-mutants/` so xtask owns the scratch surface.

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
- `FACTORY.md`, `MODEL.md`, and `INVARIANTS.md` are the primary ontology surface
- `CONFORMANCE.md` is the command and proof contract
- Historical decisions live under `archive/decisions/` and are not the public reading path.

Before release-oriented changes, run:

```bash
just docs
just ship dry
```

If a change touches persistence artifacts or cold-start behavior, update the
release notes and reference docs explicitly. Operators need to know when reopen
falls back to scan and which older artifact versions load with additive root
defaults.

After an intentional UI compile-fail test change, regenerate trybuild goldens
with `TRYBUILD=overwrite cargo test --test <name>` and review the `.stderr`
diff before committing.

Coverage artifacts are retained under `target/xtask-cover/last-run/` so failed
or partial coverage runs can be inspected instead of disappearing into a temp
directory.
