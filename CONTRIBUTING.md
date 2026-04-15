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

- `cargo xtask perf-gates` is a catastrophic-regression guard with generous thresholds. Run it on stable hardware when you want a "something is badly wrong" signal.
- `.github/workflows/perf.yml` is Criterion trend collection, not a hard gate.
- `cargo xtask bench --surface ...` is the measurement lane for comparing surfaces and baselines intentionally.

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

Coverage artifacts are retained under `target/xtask-cover/last-run/` so failed
or partial coverage runs can be inspected instead of disappearing into a temp
directory.
