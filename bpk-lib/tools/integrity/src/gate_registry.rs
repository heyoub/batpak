//! Gate registry — the DO-178B tool-qualification law in data (P1-3).
//!
//! Every gauntlet gate is listed here with whether it has BLOCKING AUTHORITY
//! (its `Err` fails a real CI run on the default PR path) and, if so, the
//! anti-vacuous RED FIXTURE test that proves the gate actually flags a planted
//! violation. The law, enforced by `tests::no_blocking_gate_without_a_red_fixture`:
//!
//!   > No gate may carry `has_blocking_authority: true` without naming an
//!   > EXISTING, ANTI-VACUOUS `red_fixture_test`.
//!
//! "Anti-vacuous" is the part SQLite TH3 and DO-178B Tool Qualification insist
//! on: a test that *cannot fail* proves nothing, so naming a green-only tautology
//! as a "red fixture" launders authority. We classify each fixture by
//! [`RedFixtureKind`] and source-scan it so the registry rejects a fixture that
//! lacks a real failing path:
//!
//! - [`RedFixtureKind::GateNegativePath`]: a normal green test that plants a
//!   VIOLATING input and asserts the gate's `check(..)` returns `Err`. Its
//!   failing path is exercised in normal CI (it runs and passes by taking the
//!   Err branch); the registry additionally requires the test body to contain an
//!   explicit failure-expecting assertion (`is_err`/`expect_err`/`Err(`/
//!   `should_panic`/…), so a budget-less "consistent OR typed error" tautology
//!   cannot qualify.
//! - [`RedFixtureKind::ProductionFlip`]: a test gated by
//!   `#[cfg(gauntlet_red_fixture)]` — green on correct production, RED when the
//!   cfg flips production (or the test's expectation) to the broken variant. Its
//!   red half is PROVEN in automation by the `gauntlet-red-fixtures-bite` CI lane
//!   (and `cargo xtask prove-gates-bite`), which builds with the cfg and asserts
//!   the fixture FAILS. The registry requires the file to contain a
//!   `gauntlet_red_fixture` branch.
//!
//! A gate that genuinely blocks today but has no qualified RED fixture yet is
//! recorded with `has_blocking_authority: false` and listed in
//! [`UNQUALIFIED_BLOCKING_GATES`] as an explicit finding — we do NOT fabricate a
//! fixture to launder authority it has not earned.

use anyhow::{Context, Result};
use std::path::Path;

/// How a gate's red fixture proves it is anti-vacuous (not a green-only
/// tautology). See the module docs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RedFixtureKind {
    /// Green test that plants a violation and asserts the gate `Err`s. Must
    /// contain an explicit failure-expecting assertion in its body.
    GateNegativePath,
    /// `#[cfg(gauntlet_red_fixture)]`-gated test, proven to red by the
    /// `gauntlet-red-fixtures-bite` lane. Must contain a `gauntlet_red_fixture`
    /// branch.
    ProductionFlip,
}

/// One gauntlet gate's qualification record.
pub(crate) struct Gate {
    /// Stable slug (matches the receipt `gate` field where the gate emits one).
    pub slug: &'static str,
    /// `Some("<repo-relative file>::<test_fn_name>")` naming the anti-vacuous RED
    /// fixture; `None` when the gate has no qualified red fixture yet (then it
    /// must NOT be blocking).
    pub red_fixture_test: Option<&'static str>,
    /// How the red fixture proves it is anti-vacuous. `Some` exactly when
    /// `red_fixture_test` is `Some`.
    pub red_fixture_kind: Option<RedFixtureKind>,
    /// Whether the gate's `Err` fails a real default-path CI run.
    pub has_blocking_authority: bool,
}

/// The registry. Slugs that emit receipts use the same slug as their receipt.
///
/// NOTE: these are explicit struct literals (not constructor helpers) ON PURPOSE
/// — `meta_gate.rs` detects gate-weakening by text-scanning this file's diff for
/// `has_blocking_authority: true` and `red_fixture_test: Some(` lines. Keep the
/// literal form so a weakening edit stays visible to the agent-safety meta-gate.
pub(crate) const GATES: &[Gate] = &[
    // --- Graders with anti-vacuous self-tests (blocking, qualified). ---
    Gate {
        slug: "assurance-level-check",
        red_fixture_test: Some("tools/integrity/src/assurance.rs::missing_seam_glob_fails_lockstep"),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    Gate {
        slug: "typed-waivers",
        red_fixture_test: Some("tools/integrity/src/typed_waivers.rs::expired_waiver_fails"),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    Gate {
        slug: "capability-snapshot",
        red_fixture_test: Some(
            "tools/integrity/src/capability_snapshot_tests.rs::downgrade_enforced_to_mediated_fails",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    Gate {
        slug: "ci-parity",
        red_fixture_test: Some(
            "tools/integrity/src/ci_parity.rs::ci_parity_rejects_unknown_xtask_command",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    Gate {
        slug: "invariant-bridge",
        red_fixture_test: Some(
            "tools/integrity/src/invariant_bridge.rs::invariant_bridge_rejects_uncited_invariant",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    Gate {
        slug: "gauntlet-receipts-present",
        red_fixture_test: Some(
            "tools/integrity/src/receipts.rs::zero_files_pass_receipt_is_rejected",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    // --- Agent-safety meta-gate ("raccoon with commit access", P1-4). ---
    Gate {
        slug: "meta-gate",
        red_fixture_test: Some(
            "tools/integrity/src/meta_gate_tests.rs::lowering_critical_seam_threshold_without_approval_errs",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    // --- Harness structural lints (blocking, qualified). ---
    Gate {
        slug: "harness-line-caps",
        red_fixture_test: Some(
            "tools/integrity/src/harness_lints.rs::check_line_caps_is_non_overridable_at_the_absolute_cap",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    Gate {
        slug: "harness-ledger-structural",
        red_fixture_test: Some(
            "tools/integrity/src/harness_lints.rs::synthetic_malformed_ledger_entry_is_rejected",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    Gate {
        slug: "harness-module-headers",
        red_fixture_test: Some(
            "tools/integrity/src/harness_lints.rs::check_module_headers_requires_canonical_fields_or_allowlist",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    // --- Runtime durability sentinels (blocking, qualified ProductionFlip via
    //     `gauntlet_red_fixture`). Their red half is proven by the
    //     `gauntlet-red-fixtures-bite` lane. ---
    Gate {
        slug: "sentinel-s2-future-version-refusal",
        red_fixture_test: Some(
            "crates/core/tests/gauntlet_s2_future_version_refusal.rs::future_version_mmap_index_is_canonical_refusal_not_silent_rebuild",
        ),
        red_fixture_kind: Some(RedFixtureKind::ProductionFlip),
        has_blocking_authority: true,
    },
    Gate {
        slug: "sentinel-s3-recovery-oracle",
        red_fixture_test: Some(
            "crates/core/tests/gauntlet_s3_recovery_oracle.rs::post_fsync_committed_batch_recovers_committed_or_canonical_refusal",
        ),
        red_fixture_kind: Some(RedFixtureKind::ProductionFlip),
        has_blocking_authority: true,
    },
    // --- Phase-2 perf sentinel (blocking, qualified ProductionFlip). Under
    //     `gauntlet_red_fixture` the allocation budget is flipped to 0, so a real
    //     append exceeds it and the gate reds — proving the budget assertion bites. ---
    Gate {
        slug: "perf-alloc-count",
        red_fixture_test: Some(
            "crates/core/tests/gauntlet_perf_alloc_count.rs::single_append_stays_under_allocation_budget",
        ),
        red_fixture_kind: Some(RedFixtureKind::ProductionFlip),
        has_blocking_authority: true,
    },
    // --- Phase-2 semantic_diff family (blocking, qualified ProductionFlip). The
    //     same seeded op stream is driven through every equivalence-claiming config
    //     pair (mmap<->scan, checkpoint<->rebuilt, fused<->unfused, cached<->uncached,
    //     reopened<->fresh); divergence is a hard finding. Under the cfg one side is
    //     fed an extra op so the diff assertion fails. ---
    Gate {
        slug: "semantic-diff",
        red_fixture_test: Some(
            "crates/core/tests/semantic_diff.rs::semantic_diff_detects_planted_divergence",
        ),
        red_fixture_kind: Some(RedFixtureKind::ProductionFlip),
        has_blocking_authority: true,
    },
    // --- Phase-B2 deterministic-simulation crash-recovery (blocking, qualified
    //     ProductionFlip). A real Store is composed over the fault-injecting SimFs
    //     backend, driven through the real append/sync API, crashed at the
    //     durability boundary, and reopened; the oracle fails closed on any
    //     illegal recovered state (lost-after-sync commit, undead event, broken
    //     hash chain, non-canonical reopen) or nondeterminism. Under
    //     `gauntlet_red_fixture` the test asserts the (illegal) lost-after-sync
    //     outcome, so its red half fails — proving the oracle bites. ---
    Gate {
        slug: "dst-recovery",
        red_fixture_test: Some(
            "crates/core/tests/dst_recovery.rs::dst_recovery_is_legal_and_deterministic",
        ),
        red_fixture_kind: Some(RedFixtureKind::ProductionFlip),
        has_blocking_authority: true,
    },
    // --- Phase-B3 recovery-oracle matrix (blocking, qualified ProductionFlip).
    //     B2's dst-recovery ran the legality oracle under ONE fault profile
    //     (honest disk). B3 generalizes it across the FULL hostile-fs matrix SimFs
    //     can model over the real Store: honest-disk crash, lying-disk fsync-drop,
    //     and crash-before-fsync at each durability boundary (single-append frame,
    //     batch-commit marker, post-fsync-before-publish, segment-rotation create).
    //     Every cell must recover EXACTLY one of {CommittedPrefix | RolledBack |
    //     CanonicalRefusal} and LEGAL (prefix, no undead, intact hash chain;
    //     honest-disk no-loss rule; lying-disk relaxation). Under
    //     `gauntlet_red_fixture` the honest-disk cell is asserted to have lost an
    //     acked-durable commit, so its red half fails — proving the oracle bites.
    Gate {
        slug: "recovery-oracle",
        red_fixture_test: Some(
            "crates/core/tests/recovery_oracle.rs::recovery_oracle_matrix_is_legal_and_deterministic",
        ),
        red_fixture_kind: Some(RedFixtureKind::ProductionFlip),
        has_blocking_authority: true,
    },
    // --- Phase-B4 linearizability gate (blocking, qualified GateNegativePath).
    //     A seeded op stream against a REAL Store under a fixed clock must, after
    //     settling on the visibility watermark, expose a dense strictly-increasing
    //     global_sequence prefix (== single-writer linearization order), with
    //     monotonic reads, reader convergence, and no real-time/seq inversion. The
    //     pure `check_linearizable` checker makes the property testable in isolation;
    //     the red fixture feeds it inverted/gapped/duplicate histories and asserts
    //     each is rejected, proving the checker is not vacuous. ---
    Gate {
        slug: "linearizability",
        red_fixture_test: Some(
            "crates/core/tests/linearizability.rs::checker_rejects_inverted_gapped_and_duplicate_histories",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    // --- Phase-2 Kth-I/O cold-start fault sentinel (blocking, qualified
    //     ProductionFlip). A real Store is reopened with a Kth-recovery-I/O fault
    //     armed on the read/scan/cold-start path; the recovered state must be a
    //     legal terminal outcome (opens consistently with no invented events OR a
    //     typed StoreError refusal, never a panic/untyped failure). Under
    //     `gauntlet_red_fixture` the test asserts the illegal counterpart
    //     (invented events / untyped failure), so its red half fails — proving the
    //     gate bites. This injects on the REAL platform-fs read path, distinct from
    //     `recovery-oracle`'s SimFs fsync-interposition matrix, so both are kept. ---
    Gate {
        slug: "fault-kth-io",
        red_fixture_test: Some(
            "crates/core/tests/gauntlet_fault_kth_recovery_io.rs::kth_io_fault_on_scan_path_is_consistent_or_typed_error",
        ),
        red_fixture_kind: Some(RedFixtureKind::ProductionFlip),
        has_blocking_authority: true,
    },
    // NOTE: `gauntlet_fault_alloc_oom` is intentionally NOT a registered gate. It
    // is a unit test of the `FailingAlloc` shim (arms/disarms), not a gauntlet
    // property: Rust aborts on allocation failure and batpak does not claim
    // graceful-OOM (a real OOM gate would require pervasive fallible allocation,
    // out of scope), so there is no honest blocking property to assert. The test
    // stays as plain coverage; we do not pretend it is a gate.
    // --- Fuzz-replay gate (GAUNT-FUZZ-1, blocking, self-proving). The replay
    //     `#[test]` re-runs every committed corpus + regression input through the
    //     real `__fuzz::*` decode entry points and asserts none panics. Its
    //     anti-vacuous fixture is the meta-test that reds when any declared fuzz
    //     `[[bin]]` loses its regression dir or its dispatcher wiring. ---
    Gate {
        slug: "fuzz-replay",
        red_fixture_test: Some("crates/core/tests/fuzz_replay.rs::fuzz_replay_covers_every_target"),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    // --- Structural source lints (blocking, qualified). Each carries a dedicated
    //     end-to-end RED fixture: a green baseline temp tree plus a planted
    //     violation asserting the full `check(..)` returns `Err`. ---
    Gate {
        slug: "file-size-pressure",
        red_fixture_test: Some(
            "tools/integrity/src/structural_tests.rs::file_size_pressure_rejects_oversized_production_file",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    Gate {
        slug: "inline-test-island-pressure",
        red_fixture_test: Some(
            "tools/integrity/src/structural_tests.rs::inline_test_island_pressure_rejects_oversized_island",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    Gate {
        slug: "dead-code-silencers",
        red_fixture_test: Some(
            "tools/integrity/src/structural_tests.rs::zero_allow_ban_rejects_every_allow",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    Gate {
        slug: "allow-justifications",
        red_fixture_test: Some(
            "tools/integrity/src/structural_tests.rs::allow_ban_rejects_even_a_justified_allow",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    Gate {
        slug: "pub-items-have-tests",
        red_fixture_test: Some(
            "tools/integrity/src/public_surface.rs::pub_items_have_tests_rejects_unwitnessed_pub_item",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    Gate {
        slug: "store-pub-fn-coverage",
        red_fixture_test: Some(
            "tools/integrity/src/store_pub_fn_coverage.rs::store_pub_fn_coverage_rejects_uncovered_store_method",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    // --- Phase-2 structural gates (blocking, qualified). ---
    // Vacuous-glob killer (GAUNT-FAULT-3): a typo'd mutation-seam glob matches no
    // tracked file -> 0 mutants -> vacuous PASS in cloud. Reds on a planted
    // `crates/core/src/NONEXISTENT.rs` glob.
    Gate {
        slug: "mutation-glob-coverage",
        red_fixture_test: Some(
            "tools/integrity/src/structural_tests.rs::glob_coverage_rejects_nonexistent_mutation_glob",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    // Function-complexity gate (GAUNT-CPLX-6): syn-based per-fn line/nesting/
    // cyclomatic budgets, ratcheted by traceability/complexity_ratchet.yaml.
    Gate {
        slug: "function-complexity",
        red_fixture_test: Some(
            "tools/integrity/src/structural_tests.rs::function_complexity_rejects_over_budget_function",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    // Wall-clock-in-correctness-tests detector (GAUNT-FLAKE-7): Instant::now()
    // paired with an elapsed/Duration assert in a non-perf test, a flake source.
    Gate {
        slug: "no-wallclock-asserts",
        red_fixture_test: Some(
            "tools/integrity/src/structural_tests.rs::no_wallclock_asserts_rejects_elapsed_assertion",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    // --- Phase-B5 complexity-EXPONENT + WCET gate (GAUNT-CPLX-EXP, blocking,
    //     qualified GateNegativePath). The live measurement fits a log-log slope
    //     of a real `Store::query(Region::all())`'s ALLOCATION COUNT (not
    //     wall-clock) across geometric input sizes and asserts the asymptotic
    //     class stays ~linear, plus a count-based WCET/p100 budget. Its
    //     anti-vacuous red fixture plants a QUADRATIC dataset and asserts the
    //     pure `check_complexity` gate returns `Err`. ---
    Gate {
        slug: "complexity-exponent",
        red_fixture_test: Some(
            "crates/core/tests/complexity_exponent.rs::super_linear_dataset_is_rejected",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    // --- bvisor C1 boundary-supervisor gates (blocking, qualified ProductionFlip).
    //     bvisor's SimBackend "monster" drives `admit → plan → run` against a lying
    //     backend while the HARNESS-OWNED GroundTruth oracle classifies what ACTUALLY
    //     happened — the monster never grades itself, mirroring batpak's recovery
    //     matrix (reopen-and-classify-independently). These fixtures live in the
    //     `bvisor` package behind `--features dangerous-test-hooks`, so the bite lane
    //     (and `prove-gates-bite`) build them per-package from the path prefix.
    //
    //     `bvisor-grid`: the G1..G13 proof grid. Under `gauntlet_red_fixture` the red
    //     branch asserts the (illegal) "lie uncaught" outcome on G4 (no-spawn-when-
    //     denied); a biting oracle always catches the spawn-despite-deny lie, so the
    //     red half FAILS — proving the grid is anti-vacuous.
    Gate {
        slug: "bvisor-grid",
        red_fixture_test: Some(
            "crates/bvisor/tests/grid.rs::grid_red_fixture_lie_must_escape",
        ),
        red_fixture_kind: Some(RedFixtureKind::ProductionFlip),
        has_blocking_authority: true,
    },
    //     `bvisor-qualification-coupling` (S1): the §1 COUPLING LAW gate. For
    //     representative machine profiles, it asserts that EVERY production-ceiling
    //     cell advertised `Enforced` has a `Proven` row in the committed Linux
    //     qualification ledger whose ProfileFloor the profile satisfies AND whose
    //     MechanismDigest matches the backend's live mechanism. Under
    //     `gauntlet_red_fixture` the red branch plants an Enforced `NetworkDenyAll`
    //     cell with NO Proven ledger row (it is FailClosed) and asserts the coupling
    //     check PASSED; a biting gate returns NotProven, so the red half FAILS —
    //     proving the gate is anti-vacuous.
    Gate {
        slug: "bvisor-qualification-coupling",
        red_fixture_test: Some(
            "crates/bvisor/tests/coupling_proof.rs::coupling_red_fixture_enforced_without_proven_row_must_fail",
        ),
        red_fixture_kind: Some(RedFixtureKind::ProductionFlip),
        has_blocking_authority: true,
    },
    //     `bvisor-proof-receipt-resolution` (S1): the ANTI-FABRICATION half of the
    //     coupling law — `Proven ⟹ a real oracle exists`. It resolves EVERY proof
    //     receipt cited by a `Proven` ledger row to a real `#[test]` fn (file exists +
    //     declares the named test). Runs on the DEFAULT build (text resolution). Red
    //     fixture plants a ghost file/fn citation and asserts the resolver `Err`s.
    Gate {
        slug: "bvisor-proof-receipt-resolution",
        red_fixture_test: Some(
            "crates/bvisor/src/contract/qualification_tests.rs::ghost_receipt_is_rejected",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    //     `bvisor-reconciliation`: the startup-reconciliation oracle (G13). It sweeps
    //     `(crash_boundary × seed)` and classifies each in-flight boundary as EXACTLY
    //     one of {Completed | RolledBack | CanonicalRefusal}, reading ONLY the
    //     persisted 0xE crash state (never a backend self-report). Under
    //     `gauntlet_red_fixture` the red branch asserts the illegal UndeadBoundary
    //     (a committed artifact with no sealed report reconciling to Completed); the
    //     real reconciler returns CanonicalRefusal (the sacred window forbids a silent
    //     Completed), so the red half FAILS — proving the oracle bites.
    Gate {
        slug: "bvisor-reconciliation",
        red_fixture_test: Some(
            "crates/bvisor/tests/reconciliation_oracle.rs::reconciliation_red_fixture_undead_boundary_must_fail",
        ),
        red_fixture_kind: Some(RedFixtureKind::ProductionFlip),
        has_blocking_authority: true,
    },
    //     `bvisor-injective-collapse` (S3, P0-A): promotes S2's injectivity to a
    //     registered gate. For EVERY constructible CanonicalPolicy variant across
    //     all four families (Fd/Spawn/Env/Net), `RequirementKind::of` must be a
    //     well-defined function (equal canonical bytes -> equal key) AND injective
    //     on variants (equal key -> equal canonical variant), so no key fuses two
    //     semantically-distinct variants. Under `gauntlet_red_fixture` the red branch
    //     drives the SAME check with a POLICY-BLIND key map collapsing
    //     InheritedFds::None/::Only onto one key and asserts NO collapse is found; a
    //     biting check always catches it, so the red half FAILS — anti-vacuous.
    Gate {
        slug: "bvisor-injective-collapse",
        red_fixture_test: Some(
            "crates/bvisor/tests/collapse_gate.rs::injective_collapse_red_fixture_policy_blind_map_must_escape",
        ),
        red_fixture_kind: Some(RedFixtureKind::ProductionFlip),
        has_blocking_authority: true,
    },
    //     `bvisor-support-completeness` (S3, P0-A): every `RequirementKind::ALL` key
    //     must carry an EXPLICIT support claim (Enforced/Mediated/Unsupported — even
    //     Unsupported must be STATED) in EVERY backend's support_matrix(); a silent
    //     gap is a finding. Under `gauntlet_red_fixture` the red branch runs the SAME
    //     completeness check against a matrix with the `Kill` key DROPPED and asserts
    //     no gap is found; a biting check always catches the dropped key, so the red
    //     half FAILS — anti-vacuous.
    Gate {
        slug: "bvisor-support-completeness",
        red_fixture_test: Some(
            "crates/bvisor/tests/collapse_gate.rs::support_completeness_red_fixture_dropped_key_must_escape",
        ),
        red_fixture_kind: Some(RedFixtureKind::ProductionFlip),
        has_blocking_authority: true,
    },
    // --- Phase-B6 DST corpus currency (Thread #64-B, blocking, qualified
    //     ProductionFlip). The committed `traceability/dst_corpus.yaml` must be
    //     non-empty and every row's FNV-1a digest must replay through the real
    //     Store+SimFs recovery oracle. Under `gauntlet_red_fixture` the test
    //     asserts a zero digest — impossible for a graduated row — so the red
    //     half FAILS and proves the gate bites. ---
    Gate {
        slug: "dst-corpus-currency",
        red_fixture_test: Some(
            "crates/core/tests/dst_corpus_currency.rs::dst_corpus_currency_replays_committed_corpus",
        ),
        red_fixture_kind: Some(RedFixtureKind::ProductionFlip),
        has_blocking_authority: true,
    },
    // --- Phase-3 over-claim detector (GAUNTLET-OVERCLAIM, Thread #67). ---
    Gate {
        slug: "overclaim",
        red_fixture_test: Some(
            "tools/integrity/src/overclaim.rs::detector_rejects_planted_overclaim",
        ),
        red_fixture_kind: Some(RedFixtureKind::ProductionFlip),
        has_blocking_authority: true,
    },
    // --- D9 repo-IR fitness gate (GAUNTLET-REPO-IR, blocking, qualified
    //     GateNegativePath). The fitness runner is no longer advisory: `repo_ir::check`
    //     folds the blocking fitnesses over the live IR and asserts every seam glob
    //     PARSED from seam_registry.yaml resolves to a tracked file. The red fixture
    //     plants a synthetic IR with an unrecognized seam assurance level and asserts
    //     the seam fitness flags exactly it. ---
    Gate {
        slug: "repo-ir-fitness",
        red_fixture_test: Some(
            "tools/integrity/src/repo_ir_tests.rs::detector_rejects_planted_bad_seam_level",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    // --- D10 no-runtime dep-graph gate (blocking, qualified GateNegativePath).
    //     The shared scanner walks the RESOLVED Cargo production graph (cargo
    //     metadata, not a Cargo.toml grep) of every runtime crate for an async
    //     runtime (tokio/async-std/smol/async-executor), catching renamed/
    //     optional/target-specific/transitive forms. The SAME scanner is the
    //     build.rs early FAIL-CLOSED sentinel. The red fixture feeds synthetic
    //     resolved graphs with a renamed, a transitive, AND a target-specific
    //     planted runtime and asserts each is flagged (the gate Errs), proving it
    //     catches the evasions the old grep missed. flume is never flagged.
    Gate {
        slug: "no-runtime-dep-graph",
        red_fixture_test: Some(
            "tools/integrity/src/no_runtime_gate.rs::planted_runtime_dep_is_rejected",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
    // --- D11 STORE_SYNC_ONLY gate (blocking, qualified GateNegativePath). The
    //     STRUCTURAL (AST) half flags a public async Store API, an impl-Future or
    //     boxed-Future return, an #[async_trait] impl, and a stray .await/async
    //     block in production store code — every shape the old `async fn`
    //     substring grep missed — plus the dep-graph half (no async executor in
    //     the store's production graph). The red fixture plants all six evasions
    //     (4 AST + a renamed-tokio + a target-specific async runtime under store)
    //     and asserts each bites. flume's recv_async() is a sync call, not flagged.
    Gate {
        slug: "store-sync-only",
        red_fixture_test: Some(
            "tools/integrity/src/store_sync_gate_tests.rs::every_async_store_evasion_is_rejected",
        ),
        red_fixture_kind: Some(RedFixtureKind::GateNegativePath),
        has_blocking_authority: true,
    },
];

/// Gates that block a real run today but are recorded as `has_blocking_authority:
/// false` because they lack a qualified anti-vacuous RED fixture. This is a
/// surfaced finding, NOT a fabricated qualification.
///
/// This list is EMPTY: `fault-kth-io` was CURED into a real blocking ProductionFlip
/// gate (it now carries a `gauntlet_red_fixture` red branch asserting the illegal
/// recovery counterpart), and `fault-alloc-oom` was DE-REGISTERED (it is a shim
/// unit test, not a gauntlet property — Rust aborts on OOM and batpak does not
/// claim graceful-OOM). So no gate sits in advisory limbo: every registered
/// blocking gate is anti-vacuous and bite-proven. The list stays so the
/// honesty-ledger test below keeps it accurate if a gate is ever withheld again.
pub(crate) const UNQUALIFIED_BLOCKING_GATES: &[&str] = &[];

/// Slugs of gates that emit an execution receipt the `gauntlet-receipts-present`
/// check requires. Kept narrow to the gates whose receipts the integrity binary
/// (and build script) actually write on a normal `structural-check` run.
pub(crate) const RECEIPT_REQUIRED_GATES: &[&str] = &[
    "assurance-level-check",
    "typed-waivers",
    "capability-snapshot",
    "ci-parity",
    "invariant-bridge",
    "structural-source-lints",
    "overclaim",
    "repo-ir-fitness",
];

/// Tokens that signal a [`RedFixtureKind::GateNegativePath`] test body asserts a
/// rejection/error/emptiness condition — the signature of a real negative-path
/// test, not a green-only "consistent OR typed error" tautology. This is a CHEAP
/// heuristic, deliberately not a proof: the rigorous anti-vacuity proof is
/// reserved for [`RedFixtureKind::ProductionFlip`] gates, whose red half the
/// `gauntlet-red-fixtures-bite` lane actually exercises. The tokens are chosen to
/// match rejection assertions (`is_err`/`Err(`) and completeness/wiring assertions
/// (`is_empty` — "a required set must not be empty") while NOT matching the loose
/// `value <= bound` tautologies the audit flagged on the fault gates.
const FAILURE_ASSERTION_TOKENS: &[&str] = &[
    "is_err",
    "expect_err",
    "unwrap_err",
    "Err(",
    "should_panic",
    "panic!",
    "is_none",  // negative-path gates assert a rejected item resolves to None
    "is_empty", // completeness/wiring gates assert a required set is non-empty
    "assert_ne!",
];

/// Split `"<file>::<test_fn>"` into its parts.
fn split_reference(reference: &str) -> Option<(&str, &str)> {
    reference.split_once("::")
}

/// True when `repo_root/<file>` contains a `fn <test_fn>` definition. This is the
/// "the named red fixture EXISTS" resolution: it verifies both that the file is
/// present and that it declares the named test function. (A `#[cfg(...)]`-gated
/// sentinel still declares its `fn` unconditionally, so this resolves it.)
fn red_fixture_resolves(repo_root: &Path, reference: &str) -> Result<bool> {
    let Some((rel, test_fn)) = split_reference(reference) else {
        return Ok(false);
    };
    let path = repo_root.join(rel);
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(_) => return Ok(false),
    };
    let needle = format!("fn {test_fn}(");
    Ok(content.contains(&needle))
}

/// Extract the `{ .. }` body of `fn <test_fn>` by brace-matching from the first
/// `{` after the signature. Returns `None` if the fn or its body is not found.
fn extract_fn_body<'a>(content: &'a str, test_fn: &str) -> Option<&'a str> {
    let sig = format!("fn {test_fn}(");
    let start = content.find(&sig)?;
    let open_rel = content[start..].find('{')?;
    let open = start + open_rel;
    let bytes = content.as_bytes();
    let mut depth = 0usize;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&content[open..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Verify the named red fixture is ANTI-VACUOUS for its kind. This is what stops
/// a green-only tautology from laundering blocking authority.
fn red_fixture_is_antivacuous(
    repo_root: &Path,
    reference: &str,
    kind: RedFixtureKind,
) -> Result<std::result::Result<(), String>> {
    let Some((rel, test_fn)) = split_reference(reference) else {
        return Ok(Err(format!("malformed reference `{reference}`")));
    };
    let path = repo_root.join(rel);
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(_) => return Ok(Err(format!("file `{rel}` not found"))),
    };
    match kind {
        RedFixtureKind::ProductionFlip => {
            // The whole file must carry a gauntlet_red_fixture branch; the
            // `gauntlet-red-fixtures-bite` lane proves the branch actually reds.
            if content.contains("gauntlet_red_fixture") {
                Ok(Ok(()))
            } else {
                Ok(Err(format!(
                    "ProductionFlip fixture `{reference}` has no `gauntlet_red_fixture` branch — \
                     its red half can never be exercised by the bite lane"
                )))
            }
        }
        RedFixtureKind::GateNegativePath => {
            let Some(body) = extract_fn_body(&content, test_fn) else {
                return Ok(Err(format!(
                    "GateNegativePath fixture `{reference}`: could not extract test body"
                )));
            };
            if FAILURE_ASSERTION_TOKENS
                .iter()
                .any(|tok| body.contains(tok))
            {
                Ok(Ok(()))
            } else {
                Ok(Err(format!(
                    "GateNegativePath fixture `{reference}` has no failure-expecting assertion \
                     ({:?}) in its body — it may be a green-only tautology and cannot qualify a \
                     blocking gate",
                    FAILURE_ASSERTION_TOKENS
                )))
            }
        }
    }
}

/// Production entry: the registry law, checked against the live tree. Reusable by
/// the `gate-registry-check` subcommand and `cargo xtask`.
pub(crate) fn check(repo_root: &Path) -> Result<()> {
    for gate in GATES {
        if !gate.has_blocking_authority {
            continue;
        }
        let reference = gate.red_fixture_test.with_context(|| {
            format!(
                "gate_registry: gate `{}` claims blocking authority but names NO red_fixture_test. \
                 DO-178B TQL: no red fixture -> no blocking authority.",
                gate.slug
            )
        })?;
        let kind = gate.red_fixture_kind.with_context(|| {
            format!(
                "gate_registry: gate `{}` is blocking with a red fixture but no RedFixtureKind \
                 (cannot verify it is anti-vacuous).",
                gate.slug
            )
        })?;
        let resolves = red_fixture_resolves(repo_root, reference)
            .with_context(|| format!("resolve red fixture for `{}`", gate.slug))?;
        anyhow::ensure!(
            resolves,
            "gate_registry: gate `{}` names red_fixture_test `{}`, but no such test function \
             exists in the named file. A blocking gate must point at an EXISTING red fixture.",
            gate.slug,
            reference
        );
        let antivacuous = red_fixture_is_antivacuous(repo_root, reference, kind)
            .with_context(|| format!("anti-vacuity scan for `{}`", gate.slug))?;
        if let Err(why) = antivacuous {
            anyhow::bail!(
                "gate_registry: gate `{}` red fixture is not anti-vacuous: {}",
                gate.slug,
                why
            );
        }
    }
    Ok(())
}

/// Slugs + references of the [`RedFixtureKind::ProductionFlip`] fixtures the
/// `gauntlet-red-fixtures-bite` lane must build under `--cfg gauntlet_red_fixture`
/// and assert FAIL. Exposed so the lane and `cargo xtask prove-gates-bite` stay
/// in lockstep with the registry (no hand-maintained second list).
pub(crate) fn production_flip_fixtures() -> Vec<&'static str> {
    GATES
        .iter()
        .filter(|g| g.has_blocking_authority)
        .filter(|g| g.red_fixture_kind == Some(RedFixtureKind::ProductionFlip))
        .filter_map(|g| g.red_fixture_test)
        .collect()
}

/// Print the qualification ledger: each gate, whether it blocks, its red fixture
/// kind, and a resolved/MISSING marker. Diagnostic only — `check` is the gate.
pub(crate) fn report(repo_root: &Path) {
    outln!(
        "gate-registry-check: ok ({} gate(s) registered)",
        GATES.len()
    );
    for gate in GATES {
        let authority = if gate.has_blocking_authority {
            "BLOCKING"
        } else {
            "advisory"
        };
        match gate.red_fixture_test {
            Some(reference) => {
                let resolves = red_fixture_resolves(repo_root, reference).unwrap_or(false);
                let marker = if resolves { "resolved" } else { "MISSING" };
                let kind = match gate.red_fixture_kind {
                    Some(RedFixtureKind::GateNegativePath) => "neg-path",
                    Some(RedFixtureKind::ProductionFlip) => "prod-flip",
                    None => "?",
                };
                let (file, test_fn) = split_reference(reference).unwrap_or((reference, "?"));
                outln!(
                    "  - {} [{authority}] {kind} red fixture {file}::{test_fn} ({marker})",
                    gate.slug
                );
            }
            None => outln!("  - {} [{authority}] no red fixture", gate.slug),
        }
    }
    if !UNQUALIFIED_BLOCKING_GATES.is_empty() {
        outln!(
            "gate-registry-check: {} gate(s) run today but are NOT yet qualified (no anti-vacuous \
             red fixture); blocking authority withheld until each lands one:",
            UNQUALIFIED_BLOCKING_GATES.len()
        );
        for slug in UNQUALIFIED_BLOCKING_GATES {
            outln!("  - {slug}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo_surface::repo_root;

    fn repo() -> std::path::PathBuf {
        repo_root().expect("repo root resolves from tools/integrity")
    }

    /// THE LAW: every blocking gate names an existing, anti-vacuous red fixture.
    #[test]
    fn no_blocking_gate_without_a_red_fixture() {
        check(&repo()).expect("every blocking gate must name an existing anti-vacuous red fixture");
    }

    /// Slugs are unique (a duplicate slug would let one gate's qualification mask
    /// another's, defeating the law).
    #[test]
    fn gate_slugs_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for gate in GATES {
            assert!(
                seen.insert(gate.slug),
                "duplicate gate slug `{}`",
                gate.slug
            );
        }
    }

    /// `red_fixture_test` and `red_fixture_kind` are present together or not at all.
    #[test]
    fn fixture_reference_and_kind_are_paired() {
        for gate in GATES {
            assert_eq!(
                gate.red_fixture_test.is_some(),
                gate.red_fixture_kind.is_some(),
                "gate `{}`: red_fixture_test and red_fixture_kind must agree",
                gate.slug
            );
        }
    }

    /// Anti-vacuity for the law itself: a synthetic blocking gate with a
    /// non-existent red fixture MUST be rejected by `red_fixture_resolves`, so
    /// the law cannot pass by failing to look.
    #[test]
    fn nonexistent_red_fixture_does_not_resolve() {
        assert!(!red_fixture_resolves(
            &repo(),
            "tools/integrity/src/receipts.rs::this_test_does_not_exist_anywhere"
        )
        .expect("resolution must not error on a missing fn"));
        assert!(
            !red_fixture_resolves(&repo(), "tools/integrity/src/does_not_exist.rs::whatever")
                .expect("resolution must not error on a missing file")
        );
        // And a real one DOES resolve, proving the resolver isn't always-false.
        assert!(red_fixture_resolves(
            &repo(),
            "tools/integrity/src/receipts.rs::zero_files_pass_receipt_is_rejected"
        )
        .expect("a real test fn must resolve"));
    }

    /// Anti-vacuity for the anti-vacuity scan: a GateNegativePath fixture whose
    /// body has no failure-expecting assertion is rejected, and a ProductionFlip
    /// fixture with no `gauntlet_red_fixture` branch is rejected.
    #[test]
    fn antivacuity_scan_rejects_green_only_fixtures() {
        // The real S2 ProductionFlip fixture passes (it has a cfg branch).
        assert!(red_fixture_is_antivacuous(
            &repo(),
            "crates/core/tests/gauntlet_s2_future_version_refusal.rs::future_version_mmap_index_is_canonical_refusal_not_silent_rebuild",
            RedFixtureKind::ProductionFlip,
        )
        .expect("scan must not error")
        .is_ok());
        // ...but classifying a file WITHOUT a cfg branch as ProductionFlip fails.
        assert!(red_fixture_is_antivacuous(
            &repo(),
            "crates/core/tests/gauntlet_fault_alloc_oom.rs::failing_alloc_arms_and_disarms_deterministically",
            RedFixtureKind::ProductionFlip,
        )
        .expect("scan must not error")
        .is_err());
        // A real GateNegativePath fixture (asserts Err) passes.
        assert!(red_fixture_is_antivacuous(
            &repo(),
            "tools/integrity/src/typed_waivers.rs::expired_waiver_fails",
            RedFixtureKind::GateNegativePath,
        )
        .expect("scan must not error")
        .is_ok());
    }

    /// The honesty ledger: every slug in the unqualified list is recorded
    /// `has_blocking_authority: false` with NO red fixture, so we never quietly
    /// flip one to blocking without giving it a qualified red fixture and removing
    /// it from this list.
    #[test]
    fn unqualified_blocking_gates_are_recorded_nonblocking() {
        for slug in UNQUALIFIED_BLOCKING_GATES {
            let found = GATES.iter().find(|g| g.slug == *slug);
            assert!(
                found.is_some(),
                "unqualified gate `{slug}` missing from GATES"
            );
            let gate = found.expect("checked is_some directly above");
            assert!(
                !gate.has_blocking_authority,
                "gate `{slug}` is listed as unqualified but claims blocking authority"
            );
            assert!(
                gate.red_fixture_test.is_none(),
                "unqualified gate `{slug}` should not name a red fixture"
            );
        }
    }

    /// The bite lane's fixture list is derived from the registry and non-empty
    /// (S2/S3/perf-alloc are ProductionFlip), so the lane can never silently
    /// cover zero fixtures.
    #[test]
    fn production_flip_fixtures_are_derivable_and_nonempty() {
        let fixtures = production_flip_fixtures();
        assert!(
            fixtures.len() >= 3,
            "expected the S2/S3/perf-alloc ProductionFlip fixtures, got {fixtures:?}"
        );
        for reference in fixtures {
            assert!(
                red_fixture_resolves(&repo(), reference).expect("resolve"),
                "ProductionFlip fixture `{reference}` must resolve"
            );
        }
    }
}
