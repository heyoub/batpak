# Audit Report: batpak

Evidence run on 2026-03-30:
- `cargo test --all-features`: pass
- `cargo check --no-default-features`: pass
- `cargo fmt --check`: pass
- `cargo bench --no-run --all-features`: pass
- `cargo clippy --all-features --all-targets -- -D warnings`: fail at `tests/subscription_ops.rs:434` (`panic!`)
- `cargo deny check`: fail parsing `batpak/deny.toml` on `unmaintained = "warn"`

Evidence-only inputs used but not scored: `batpak/Cargo.lock`, `batpak/tests/fuzz_targets.proptest-regressions`, `batpak/tests/golden/*.hex`, `batpak/tests/ui/*.stderr`, `batpak/LICENSE-*`, `.claude/settings.local.json`.

## Crate: batpak

### batpak/.gitignore
Applicable Parameters: Build-Config
Score: 72/100
Notes: Basic hygiene is present, but this file is thin and does not help determinism or traceability beyond excluding local artifacts.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/.cargo/config.toml
Applicable Parameters: Build-Config
Score: 48/100
Notes: This hardcodes a developer-specific Windows MSVC linker path, which weakens Environment Determinism and makes the build sensitive to unmanaged host state.
Named Offenses / Forbidden Remedies: Structural finding: non-hermetic environment coupling.

### batpak/.config/nextest.toml
Applicable Parameters: Build-Config
Score: 84/100
Notes: Strong deterministic test-runner settings and JUnit output support self-accusation; limited only by being execution policy rather than architectural proof.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/.github/workflows/ci.yml
Applicable Parameters: Build-Config
Score: 74/100
Notes: CI covers checks, tests, benches, docs, fuzz/chaos, and coverage, but the clippy job omits `--all-targets`, which let the current `tests/subscription_ops.rs` lint failure escape the declared gate.
Named Offenses / Forbidden Remedies: Coverage Mirage risk on lint surface.

### batpak/ARCHITECTURE.md
Applicable Parameters: Spec-Docs
Score: 78/100
Notes: Useful narrative architecture guide with explicit invariants and build-time checks, but it remains prose rather than machine-checked traceability.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/build.rs
Applicable Parameters: Build-Config
Score: 87/100
Notes: This is the strongest self-accusation surface in the crate: it enforces no-stub, no-tokio, allow-justification, config wiring, and public-item test linkage. Score is reduced because it relies on string scanning and `panic!`-driven enforcement rather than richer structural analysis.
Named Offenses / Forbidden Remedies: None confirmed; enforcement uses intentional `panic!` in build context, not production paths.

### batpak/Cargo.toml
Applicable Parameters: Build-Config
Score: 89/100
Notes: Strong dependency pinning, feature isolation, clippy policy, and benchmark wiring. The main gap is that dependency governance is not fully aligned with the local `cargo deny` behavior.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/CHANGELOG.md
Applicable Parameters: Spec-Docs
Score: 68/100
Notes: Changelog exists, which helps decision capture, but it is too thin to support bidirectional traceability or freeze-conflict history on its own.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/clippy.toml
Applicable Parameters: Build-Config
Score: 84/100
Notes: Strong banned-method policy and complexity thresholds directly accuse common AI failure modes. Score is reduced because the active CI and local wrapper commands do not consistently apply this policy to all targets.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/deny.toml
Applicable Parameters: Build-Config
Score: 42/100
Notes: The intent is good, but the current config does not parse under the local `cargo deny check`, so the supply-chain gate is not currently trustworthy as executed evidence.
Named Offenses / Forbidden Remedies: Structural finding: broken dependency-audit gate.

### batpak/docs/superpowers/plans/2026-03-30-test-bench-reorganization.md
Applicable Parameters: Spec-Docs
Score: 34/100
Notes: This internal plan is materially stale: it still references `tests/self_benchmark.rs`, `tests/quiet_stragglers.rs`, outdated TODOs, and old test counts, so it now creates traceability drift instead of reducing it.
Named Offenses / Forbidden Remedies: Bidirectional traceability failure.

### batpak/justfile
Applicable Parameters: Build-Config
Score: 70/100
Notes: Useful operator shortcuts exist, but `ci` and `clip` do not run clippy against all targets and do not include the deny gate, so the local workflow under-enforces the declared standard.
Named Offenses / Forbidden Remedies: Coverage Mirage risk on local gate surface.

### batpak/README.md
Applicable Parameters: Spec-Docs
Score: 80/100
Notes: Clear public summary of invariants, architecture, and project layout. It is informative, but not enough to satisfy bidirectional requirement-to-artifact traceability by itself.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/rust-toolchain.toml
Applicable Parameters: Build-Config
Score: 88/100
Notes: Strong toolchain pinning for formatter, clippy, and rust-src improves reproducibility across environments.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/scripts/bench-report
Applicable Parameters: DevOps-Scripts
Score: 78/100
Notes: This strengthens benchmark feedback loops and baseline comparison, but it remains advisory rather than an invariant-enforcing gate.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/scripts/coverage-feedback
Applicable Parameters: DevOps-Scripts
Score: 80/100
Notes: Good coverage feedback support and CI threshold integration. Score is reduced because coverage remains line-oriented and therefore only partially addresses Coverage Mirage.
Named Offenses / Forbidden Remedies: Coverage Mirage risk remains partially unclosed.

### batpak/TUNING.md
Applicable Parameters: Spec-Docs
Score: 76/100
Notes: Helpful operational guidance for Store configuration and tradeoffs. It explains behavior but does not fully tie settings back to invariant proofs or rollout evidence.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/lib.rs
Applicable Parameters: Core-Source
Score: 86/100
Notes: Clean dependency-ordered module surface and compile-time feature guards align with architecture freeze. Score reduced for `unexpected_cfgs` allowances and prose-only dependency-order enforcement.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/prelude.rs
Applicable Parameters: Core-Source
Score: 84/100
Notes: Thin, honest re-export layer with no business logic. Score reduced because broad public aggregation increases surface area and makes visibility creep easier to miss.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/wire.rs
Applicable Parameters: Core-Source
Score: 91/100
Notes: Strong semantic serialization helpers with no stub patterns and clear purpose; golden and fuzz tests make this a well-proved low-level module.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/coordinate/mod.rs
Applicable Parameters: Core-Source
Score: 88/100
Notes: Semantic types (`Coordinate`, `Region`, `KindFilter`) preserve meaning at boundaries and are well-covered by API tests. Minor reduction for prose-only semantic contracts.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/coordinate/position.rs
Applicable Parameters: Core-Source
Score: 90/100
Notes: Compact, deterministic causal-position logic with strong direct test coverage and no silence/stub signals.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/event/hash.rs
Applicable Parameters: Core-Source
Score: 91/100
Notes: Small, focused hashing module with direct tamper-detection tests and no fake-success patterns.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/event/header.rs
Applicable Parameters: Core-Source
Score: 88/100
Notes: Strong wire-focused header model with explicit flags and justified cast suppression. Slight reduction because semantics are still mostly documented rather than encoded as richer types.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/event/kind.rs
Applicable Parameters: Core-Source
Score: 90/100
Notes: Strong sealed event-kind surface with reserved-system/effect separation and good direct tests.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/event/mod.rs
Applicable Parameters: Core-Source
Score: 90/100
Notes: Honest event container with typed mapping and no downgrade signatures; well-backed by API tests.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/event/sourcing.rs
Applicable Parameters: Core-Source
Score: 82/100
Notes: Clean trait surface for replay and reaction patterns, but this file mostly defines contracts and examples rather than self-accusing proofs; doctext still shows `unwrap()` in examples.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/guard/denial.rs
Applicable Parameters: Core-Source
Score: 89/100
Notes: Structured denial type preserves context and supports fail-visible behavior instead of laundering failures into defaults.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/guard/mod.rs
Applicable Parameters: Core-Source
Score: 90/100
Notes: Strong gate composition surface with both fail-fast and evaluate-all paths, and tests prove receipts are earned rather than fabricated.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/guard/receipt.rs
Applicable Parameters: Core-Source
Score: 92/100
Notes: Strong sealed receipt model directly resists receipt hollowing and TOCTOU-style forgery; compile-fail tests back the invariant.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/id/mod.rs
Applicable Parameters: Core-Source
Score: 88/100
Notes: Clean ID abstraction with v7 generation and macro-backed semantic typing. Score reduced slightly because macro-generated semantics are harder to inspect than hand-written types.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/outcome/combine.rs
Applicable Parameters: Core-Source
Score: 87/100
Notes: Real algebraic combination logic with strong downstream tests; minor reduction for wildcard-arm allowance and complexity.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/outcome/error.rs
Applicable Parameters: Core-Source
Score: 88/100
Notes: Structured error taxonomy with explicit domain/operational/retryable classification helps resist Error Path Hollowing.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/outcome/mod.rs
Applicable Parameters: Core-Source
Score: 87/100
Notes: Rich algebraic result surface with strong test evidence. Score reduced for size/branch density, which increases downgrade risk even though tests currently hold it in check.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/outcome/wait.rs
Applicable Parameters: Core-Source
Score: 88/100
Notes: Clear semantic enums with strong serializer/property coverage and no silence patterns.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/pipeline/bypass.rs
Applicable Parameters: Core-Source
Score: 88/100
Notes: Bypass is explicit and justified rather than hidden, which is the right shape under this protocol. The escape hatch still deserves audit attention because it is a privileged path.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/pipeline/mod.rs
Applicable Parameters: Core-Source
Score: 89/100
Notes: Gate-then-commit flow is explicit and well-covered; no fake-success or local-construction issues were found.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/store/cursor.rs
Applicable Parameters: Core-Source
Score: 86/100
Notes: Honest guaranteed-delivery pull surface with straightforward implementation and good integration coverage.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/store/index.rs
Applicable Parameters: Core-Source
Score: 88/100
Notes: Real shared-state index and entity-lock ownership are clearly modeled. Slight reduction for internal complexity and reliance on indirect proofs through larger store tests.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/store/mod.rs
Applicable Parameters: Core-Source
Score: 80/100
Notes: This is a real orchestration core with strong end-to-end coverage, but it is very large, owns many invariants, exposes a hidden public test hook (`panic_writer_for_test`), and documents a compaction concurrency caveat instead of fully proving it away.
Named Offenses / Forbidden Remedies: Structural policy finding: visibility creep risk via public test hook.

### batpak/src/store/projection.rs
Applicable Parameters: Core-Source
Score: 76/100
Notes: Real cache backends are implemented and thoroughly tested, but this module contains two `unsafe` LMDB blocks and a default `prefetch()` no-op that weakens the “codebase must accuse itself” posture for predictive-cache behavior.
Named Offenses / Forbidden Remedies: None confirmed; monitor for Polite Downgrade on default no-op paths.

### batpak/src/store/reader.rs
Applicable Parameters: Core-Source
Score: 85/100
Notes: Good integrity checks, FD-cache discipline, and inline unit tests. Score reduced because this module mixes production logic and test support in one file and still uses an internal `expect()` in test-only code.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/store/segment.rs
Applicable Parameters: Core-Source
Score: 87/100
Notes: Strong frame/segment encoding surface with explicit CRC and typestate markers; golden and edge-case tests make this a well-proved storage boundary.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/store/subscription.rs
Applicable Parameters: Core-Source
Score: 87/100
Notes: Thin composition layer with a clear push/pull split and direct downstream coverage in `subscription_ops.rs` and larger store tests.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/store/writer.rs
Applicable Parameters: Core-Source
Score: 79/100
Notes: Strong single-writer commit path and panic-recovery design, but the intentional panic path and restart logic increase structural risk, and some invariants are still comment-enforced rather than type-enforced.
Named Offenses / Forbidden Remedies: Structural policy finding: test-only panic path exists in production code.

### batpak/src/typestate/mod.rs
Applicable Parameters: Core-Source
Score: 90/100
Notes: Strong macro-based typestate generation with compile-fail coverage against invalid states.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/src/typestate/transition.rs
Applicable Parameters: Core-Source
Score: 90/100
Notes: Small, honest transition wrapper with clear compile-time semantics and good proof via typestate tests.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/tests/chaos_testing.rs
Applicable Parameters: Test-Infrastructure
Score: 82/100
Notes: STRONG. Valuable adversarial load and corruption coverage, but the file is large and uses test-only panics/unwraps/prints that increase maintenance noise.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/tests/config_propagation.rs
Applicable Parameters: Test-Infrastructure
Score: 90/100
Notes: STRONG. Good proof that configuration fields are wired through real behavior rather than orphaned in the type surface.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/tests/event_api.rs
Applicable Parameters: Test-Infrastructure
Score: 91/100
Notes: STRONG. High-value direct API tests for coordinate, event, kind, and ID semantics with strong content assertions.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/tests/fuzz_chaos_feedback.rs
Applicable Parameters: Test-Infrastructure
Score: 82/100
Notes: STRONG. Good self-measuring feedback-loop coverage, but the harness uses ignores, prints, and panic-style assertions that make the gate less deterministic than the strongest tests.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/tests/fuzz_targets.rs
Applicable Parameters: Test-Infrastructure
Score: 84/100
Notes: STRONG. Excellent serializer/fuzzer breadth and real production imports; score reduced because this file deliberately relaxes several lint rules for harness convenience.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/tests/gate_pipeline.rs
Applicable Parameters: Test-Infrastructure
Score: 90/100
Notes: STRONG. Directly proves bypass, denial, receipt, and pipeline flow semantics with concrete assertions and compile-adjacent coverage.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/tests/hash_chain.rs
Applicable Parameters: Test-Infrastructure
Score: 90/100
Notes: STRONG. Good tamper-detection and chain-integrity proof without shadow types or fake assertions.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/tests/monad_laws.rs
Applicable Parameters: Test-Infrastructure
Score: 90/100
Notes: STRONG. Focused law-based proof file with real behavioral content rather than line-coverage padding.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/tests/outcome_combinators.rs
Applicable Parameters: Test-Infrastructure
Score: 88/100
Notes: STRONG. Broad behavioral coverage of algebraic branches. Score reduced only for file size and several panic-style expected-failure assertions.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/tests/perf_gates.rs
Applicable Parameters: Test-Infrastructure
Score: 93/100
Notes: STRONG. This is the best dogfooding file in the crate: the gate system evaluates its own performance claims and produces real negative cases.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/tests/projection_cache.rs
Applicable Parameters: Test-Infrastructure
Score: 91/100
Notes: STRONG. Real backend coverage for NoCache, Redb, LMDB, freshness, and metadata behavior closes several phantom/chimera risks.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/tests/store_advanced.rs
Applicable Parameters: Test-Infrastructure
Score: 87/100
Notes: STRONG. Excellent deep integration coverage for advanced store behaviors, but the file is very large and therefore harder to audit for gaps.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/tests/store_edge_cases.rs
Applicable Parameters: Test-Infrastructure
Score: 88/100
Notes: STRONG. Good hard-path and corruption coverage with real assertions. Score reduced slightly for local allow usage and panic-style mismatch checks.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/tests/store_integration.rs
Applicable Parameters: Test-Infrastructure
Score: 90/100
Notes: STRONG. Clear end-to-end production-path coverage for append, cold start, query, projection, CAS, and concurrency.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/tests/store_properties.rs
Applicable Parameters: Test-Infrastructure
Score: 92/100
Notes: STRONG. High-value law/property coverage for replay determinism, round-trip fidelity, idempotency, flow connectivity, and error propagation.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/tests/subscription_ops.rs
Applicable Parameters: Test-Infrastructure
Score: 62/100
Notes: WEAK-MEDIUM. Behavioral assertions are real, but this file currently breaks `cargo clippy --all-targets -- -D warnings` through a direct `panic!` and relies on crate-level allowances for `thread::spawn` and `unwrap_used`.
Named Offenses / Forbidden Remedies: Rogue Silence risk in test harness; current lint failure is active evidence.

### batpak/tests/typestate_safety.rs
Applicable Parameters: Test-Infrastructure
Score: 91/100
Notes: STRONG. Compile-fail and runtime tests together prove the typestate and receipt-forgery barriers are real.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/tests/ui/forge_receipt.rs
Applicable Parameters: Test-Infrastructure
Score: 88/100
Notes: STRONG. Small but high-value negative test that proves receipt construction is not publicly forgeable.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/tests/ui/invalid_transition.rs
Applicable Parameters: Test-Infrastructure
Score: 88/100
Notes: STRONG. Small but high-value compile-fail proof for illegal typestate transitions.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/tests/wire_format.rs
Applicable Parameters: Test-Infrastructure
Score: 91/100
Notes: STRONG. Golden-wire verification is exactly the kind of deterministic replay proof this protocol asks for.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/benches/cold_start.rs
Applicable Parameters: Examples-Benches
Score: 82/100
Notes: Honest benchmark with real store population and cold-start measurement. It is informative rather than a hard gate by itself.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/benches/compaction.rs
Applicable Parameters: Examples-Benches
Score: 80/100
Notes: Real compaction benchmark that exercises production behavior. Score reduced because it measures throughput/latency without directly proving correctness under concurrent cutover conditions.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/benches/projection_latency.rs
Applicable Parameters: Examples-Benches
Score: 86/100
Notes: Strong benchmark coverage for replay plus cache-hit/cache-miss paths across backends; `cargo bench --no-run --all-features` passed during audit.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/benches/subscription_fanout.rs
Applicable Parameters: Examples-Benches
Score: 81/100
Notes: Useful fan-out benchmark against the real store/writer path. It remains a measurement artifact rather than a policy gate.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/benches/write_throughput.rs
Applicable Parameters: Examples-Benches
Score: 84/100
Notes: Broad throughput benchmark with concurrency coverage and real append paths; stronger than a toy microbenchmark.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/examples/chat_room.rs
Applicable Parameters: Examples-Benches
Score: 70/100
Notes: Clear runnable example with real API usage, but examples are explanatory surfaces, not proving artifacts, and this one relies heavily on printed narration.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/examples/dungeon_typestate.rs
Applicable Parameters: Examples-Benches
Score: 72/100
Notes: Good pedagogical typestate example using real APIs. Score reduced because behavior is narrated through prints rather than checked as an invariant-bearing test.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/examples/event_sourced_counter.rs
Applicable Parameters: Examples-Benches
Score: 74/100
Notes: Good event-sourcing walkthrough with real projection/replay usage. Limited self-accusation because it is demonstrative, not assertive.
Named Offenses / Forbidden Remedies: None confirmed.

### batpak/examples/policy_gates.rs
Applicable Parameters: Examples-Benches
Score: 76/100
Notes: Strongest example of the set because it demonstrates real approval/denial paths, but it is still observational rather than test-enforced.
Named Offenses / Forbidden Remedies: None confirmed.

### Crate Rollup: batpak
Score: 83/100
Notes: The crate is materially strong on self-accusing tests, compile-fail proofs, golden wire checks, and build-time invariant enforcement. The main unresolved findings are operational: `batpak/.cargo/config.toml` hardcodes a local linker path, `batpak/deny.toml` does not parse under the local `cargo deny check`, `batpak/src/store/mod.rs` and `batpak/src/store/writer.rs` carry test-only/public-surface risk, and `batpak/tests/subscription_ops.rs` currently fails the stronger clippy gate that the repo claims to want.

## Repo / System / DevOps

### .editorconfig
Applicable Parameters: Build-Config
Score: 72/100
Notes: Useful formatting baseline, but it is a light hygiene control rather than a meaningful integrity detector.
Named Offenses / Forbidden Remedies: None confirmed.

### README.md
Applicable Parameters: Spec-Docs
Score: 74/100
Notes: Good top-level navigation and project framing, but it is index-like rather than traceability-rich.
Named Offenses / Forbidden Remedies: None confirmed.

### CONTRIBUTING.md
Applicable Parameters: Spec-Docs
Score: 70/100
Notes: Clear contributor workflow, but it overstates lint confidence by recommending clippy without the stronger all-targets form that exposes the current `subscription_ops.rs` failure.
Named Offenses / Forbidden Remedies: Coverage Mirage risk on contributor guidance.

### SPEC.md
Applicable Parameters: Spec-Docs
Score: 62/100
Notes: This is the strongest design document conceptually, but it has drifted: it still references `tests/self_benchmark.rs` and other historical paths, so bidirectional traceability is no longer trustworthy as written.
Named Offenses / Forbidden Remedies: Bidirectional traceability failure.

### SPEC_REGISTRY.md
Applicable Parameters: Spec-Docs
Score: 58/100
Notes: Similar to `SPEC.md`, this is rich in intended architecture but now contains stale file references and therefore undermines architecture freeze fidelity.
Named Offenses / Forbidden Remedies: Bidirectional traceability failure.

### scripts/verify-all.sh
Applicable Parameters: DevOps-Scripts
Score: 66/100
Notes: Helpful wrapper script for fmt/clippy/test/doc, but it does not run clippy on all targets, does not include the deny gate, and therefore does not fully “accuse the codebase” under the stronger standard.
Named Offenses / Forbidden Remedies: Coverage Mirage risk on local verification script.

### Repo / System / DevOps Rollup
Score: 67/100
Notes: The repo-level surfaces explain the system well, but they lag the implemented crate. The dominant issue is traceability drift in `SPEC.md` and `SPEC_REGISTRY.md`, followed by underpowered lint/dependency gates in `CONTRIBUTING.md` and `scripts/verify-all.sh`.

## Aggregate Score
82/100
