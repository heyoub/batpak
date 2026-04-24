# ADR-0012: No Dead-Code Silencers

## Status
Accepted

## Context
`#[allow(dead_code)]` and its variants (`#[expect(dead_code)]`, `#[allow(unused)]`, and `cfg_attr`-wrapped forms of each) normalized across many agent-authored changes as the short-circuit answer to an unused-code warning. Each individual site looked reasonable; the accumulated pattern obscured a real signal. When the compiler reports dead code, the honest response is one of three concrete edits:

- Test-only code → move it behind `#[cfg(test)]` so non-test builds never compile it.
- Truly unused code → delete it.
- Shared infrastructure that appears unused per-binary because each consumer uses only a subset → restructure so the compilation surface matches actual ownership (a dedicated workspace crate, a narrower helper module, or finer-grained `#[path]` includes in `tests/`).

Silencing the warning is a fourth answer that erases the signal without addressing any of the three underlying shapes. A live proof found 19+ prior agent interactions that took the silencing path over the restructuring path; the invariant is recorded here so the gate is legible and the answer is mechanical.

A companion post-mortem (`src/test_support.rs`, since removed) made the same point from the other direction: a helper module parked under `src/` avoided the warnings by crossing into production-code lint territory (`clippy::expect_used`, public-surface rules). That was not neutral — hiding test helpers in `src/` upgrades them into production code, which is the wrong category. The fix was to move the helpers back under `tests/` as finer-grained `#[path]`-included modules whose contents each consumer fully uses. See ADR-0005 for the legitimate `dangerous-test-hooks` feature-gated test surface, which remains the only accepted production-adjacent test helper shape.

## Decision
Any `#[allow(...)]`, `#[expect(...)]`, or `cfg_attr`-wrapped attribute whose lint list mentions `dead_code` or the `unused` lint group (which subsumes `dead_code`) is banned in every tracked Rust source file under `src/`, `tests/`, `examples/`, `benches/`, `build.rs`, `crates/macros/src/`, `crates/macros-support/src/`, `tools/xtask/src/`, and `tools/integrity/src/`.

The ban is enforced by the AST walker in `shared_checks::collect_dead_code_silencer_sites` in `tools/shared/shared_checks.rs`, called from two detectors:

- `build.rs::check_no_dead_code_silencers` runs on every `cargo build`, `cargo check`, and `cargo test`.
- `tools/integrity/src/main.rs::check_no_dead_code_silencers` runs as part of `cargo xtask structural`, which `cargo xtask ci` calls automatically.

The walker catches:

- `#[allow(dead_code)]`, `#[expect(dead_code)]`, `#![allow(dead_code)]`, `#![expect(dead_code)]`
- Lint lists containing `dead_code`: `#[allow(dead_code, unused_imports)]`, `#[allow(clippy::needless_return, dead_code)]`
- `#[allow(unused)]`, `#[expect(unused)]`, `#![allow(unused)]`, `#![expect(unused)]` — the `unused` lint group contains `dead_code`
- `cfg_attr` wrappers around every form above, including multi-line wrappers: `#[cfg_attr(not(test), allow(dead_code))]`, `#[cfg_attr(feature = "x", expect(unused))]`, etc.

It deliberately does NOT catch sibling `unused_*` lints that do not subsume `dead_code`: `unused_imports`, `unused_variables`, `unused_mut`, `unused_must_use`. Those target unrelated behaviour and remain available with the usual `// justifies:` anchor requirement per INV-ALLOW-IS-DESIGN.

The regression test for the detector lives in `tools/shared/shared_checks.rs` under `#[cfg(test)] mod tests`. It records both single-line and multi-line banned shapes plus explicitly allowed sibling `unused_*` lints, so any future loosening is a deliberate edit.

There is one explicit escape hatch: `traceability/dead_code_silencer_allowlist.yaml`. Each entry names one exact site as `path: "repo/file.rs:line"` and must also provide non-empty `reason` and `adr` fields. Both detectors validate the file schema and the referenced ADR before honoring an entry. The default posture is an empty allowlist.

## Consequences
- Dead-code warnings surface real structural questions (test-only? unused? shared?) instead of being silenced in place.
- `src/` stays production code; test helpers live under `tests/` at a granularity that matches consumption. A helper file includable via `#[path]` contains only items that every consuming binary uses.
- Bench helpers live in their own workspace crate (`crates/bench-support`) rather than hiding under `src/` or `tests/`; the compilation unit matches the ownership surface.
- The detector is symmetric across `build.rs` and `cargo xtask structural`, so the policy is enforced from both the fast local loop and the CI gate.
- Legitimate exceptions are explicit, traceable, and reviewable: add one exact-site entry to `traceability/dead_code_silencer_allowlist.yaml` with a real `reason` and `adr`. There is no narrative `// justifies:` escape hatch for this lint — the whole point is that the pattern does not have a legitimate site-local answer.
