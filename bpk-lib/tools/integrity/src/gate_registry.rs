//! Gate registry — the DO-178B tool-qualification law in data (P1-3).
//!
//! Every gauntlet gate is listed here with whether it has BLOCKING AUTHORITY
//! (its `Err` fails a real CI run on the default PR path) and, if so, the
//! anti-vacuous RED FIXTURE test that proves the gate actually flags a planted
//! violation. The law, enforced by [`tests::no_blocking_gate_without_a_red_fixture`]:
//!
//!   > No gate may carry `has_blocking_authority: true` without naming an
//!   > EXISTING `red_fixture_test`.
//!
//! This is the Rust analog of DO-178B Tool Qualification (TQL): a tool's output
//! may be trusted only if the tool itself is verified — and SQLite TH3's
//! anti-vacuous rule that a test which cannot fail proves nothing. A gate that
//! blocks merges but has no test proving it can flag a bad input is exactly such
//! a vacuous trust.
//!
//! A gate that genuinely blocks today but has no dedicated RED fixture yet is
//! recorded with `has_blocking_authority: false` and listed in
//! [`UNQUALIFIED_BLOCKING_GATES`] as an explicit finding — we do NOT fabricate a
//! fixture to launder authority it has not earned.

use anyhow::{Context, Result};
use std::path::Path;

/// One gauntlet gate's qualification record.
pub(crate) struct Gate {
    /// Stable slug (matches the receipt `gate` field where the gate emits one).
    pub slug: &'static str,
    /// `Some("<repo-relative file>::<test_fn_name>")` naming the anti-vacuous RED
    /// fixture; `None` when the gate has no red fixture yet (then it must NOT be
    /// blocking).
    pub red_fixture_test: Option<&'static str>,
    /// Whether the gate's `Err` fails a real default-path CI run.
    pub has_blocking_authority: bool,
}

/// The registry. Slugs that emit receipts use the same slug as their receipt.
pub(crate) const GATES: &[Gate] = &[
    // --- Graders with anti-vacuous self-tests (blocking, qualified). ---
    Gate {
        slug: "assurance-level-check",
        red_fixture_test: Some(
            "tools/integrity/src/assurance.rs::missing_seam_glob_fails_lockstep",
        ),
        has_blocking_authority: true,
    },
    Gate {
        slug: "typed-waivers",
        red_fixture_test: Some("tools/integrity/src/typed_waivers.rs::expired_waiver_fails"),
        has_blocking_authority: true,
    },
    Gate {
        slug: "ci-parity",
        red_fixture_test: Some(
            "tools/integrity/src/ci_parity.rs::ci_parity_rejects_unknown_xtask_command",
        ),
        has_blocking_authority: true,
    },
    Gate {
        slug: "invariant-bridge",
        red_fixture_test: Some(
            "tools/integrity/src/invariant_bridge.rs::invariant_bridge_rejects_uncited_invariant",
        ),
        has_blocking_authority: true,
    },
    Gate {
        slug: "gauntlet-receipts-present",
        red_fixture_test: Some(
            "tools/integrity/src/receipts.rs::zero_files_pass_receipt_is_rejected",
        ),
        has_blocking_authority: true,
    },
    // --- Harness structural lints (blocking, qualified). ---
    Gate {
        slug: "harness-line-caps",
        red_fixture_test: Some(
            "tools/integrity/src/harness_lints.rs::check_line_caps_is_non_overridable_at_the_absolute_cap",
        ),
        has_blocking_authority: true,
    },
    Gate {
        slug: "harness-ledger-structural",
        red_fixture_test: Some(
            "tools/integrity/src/harness_lints.rs::synthetic_malformed_ledger_entry_is_rejected",
        ),
        has_blocking_authority: true,
    },
    Gate {
        slug: "harness-module-headers",
        red_fixture_test: Some(
            "tools/integrity/src/harness_lints.rs::check_module_headers_requires_canonical_fields_or_allowlist",
        ),
        has_blocking_authority: true,
    },
    // --- Runtime durability sentinels (blocking, qualified by `gauntlet_red_fixture`). ---
    Gate {
        slug: "sentinel-s2-future-version-refusal",
        red_fixture_test: Some(
            "crates/core/tests/gauntlet_s2_future_version_refusal.rs::future_version_mmap_index_is_canonical_refusal_not_silent_rebuild",
        ),
        has_blocking_authority: true,
    },
    Gate {
        slug: "sentinel-s3-recovery-oracle",
        red_fixture_test: Some(
            "crates/core/tests/gauntlet_s3_recovery_oracle.rs::post_fsync_committed_batch_recovers_committed_or_canonical_refusal",
        ),
        has_blocking_authority: true,
    },
    // --- Structural source lints (blocking, qualified). Each now carries a
    //     dedicated end-to-end RED fixture: a green baseline temp tree plus a
    //     planted violation asserting the full `check(..)` returns `Err`. ---
    Gate {
        slug: "file-size-pressure",
        red_fixture_test: Some(
            "tools/integrity/src/structural_tests.rs::file_size_pressure_rejects_oversized_production_file",
        ),
        has_blocking_authority: true,
    },
    Gate {
        slug: "inline-test-island-pressure",
        red_fixture_test: Some(
            "tools/integrity/src/structural_tests.rs::inline_test_island_pressure_rejects_oversized_island",
        ),
        has_blocking_authority: true,
    },
    Gate {
        slug: "dead-code-silencers",
        red_fixture_test: Some(
            "tools/integrity/src/structural_tests.rs::dead_code_silencers_reject_dead_code_allow",
        ),
        has_blocking_authority: true,
    },
    Gate {
        slug: "allow-justifications",
        red_fixture_test: Some(
            "tools/integrity/src/structural_tests.rs::allow_justifications_rejects_unanchored_allow",
        ),
        has_blocking_authority: true,
    },
    Gate {
        slug: "pub-items-have-tests",
        red_fixture_test: Some(
            "tools/integrity/src/public_surface.rs::pub_items_have_tests_rejects_unwitnessed_pub_item",
        ),
        has_blocking_authority: true,
    },
    Gate {
        slug: "store-pub-fn-coverage",
        red_fixture_test: Some(
            "tools/integrity/src/store_pub_fn_coverage.rs::store_pub_fn_coverage_rejects_uncovered_store_method",
        ),
        has_blocking_authority: true,
    },
];

/// Gates that block a real run today but are recorded as `has_blocking_authority:
/// false` because they lack a dedicated anti-vacuous RED fixture. This is a
/// surfaced finding, NOT a fabricated qualification: each needs an end-to-end red
/// fixture (plant a violating tree, assert the gate's `check(..)` returns `Err`)
/// before it may flip to blocking in the registry.
///
/// This list is now EMPTY: every previously-unqualified structural source lint
/// earned its blocking authority by landing a dedicated end-to-end RED fixture
/// (see the `structural-source-lints` family in [`GATES`]). The list stays so the
/// honesty-ledger test below keeps it permanently empty — a regression that
/// withholds authority again must re-list the gate here.
pub(crate) const UNQUALIFIED_BLOCKING_GATES: &[&str] = &[];

/// Slugs of gates that emit an execution receipt the `gauntlet-receipts-present`
/// check requires. Kept narrow to the gates whose receipts the integrity binary
/// (and build script) actually write on a normal `structural-check` run.
pub(crate) const RECEIPT_REQUIRED_GATES: &[&str] = &[
    "assurance-level-check",
    "typed-waivers",
    "ci-parity",
    "invariant-bridge",
    "structural-source-lints",
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

/// Production entry: the registry law, checked against the live tree. Reusable by
/// a future `gate-registry-check` subcommand; today it backs the `#[cfg(test)]`
/// law below.
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
        let resolves = red_fixture_resolves(repo_root, reference)
            .with_context(|| format!("resolve red fixture for `{}`", gate.slug))?;
        anyhow::ensure!(
            resolves,
            "gate_registry: gate `{}` names red_fixture_test `{}`, but no such test function \
             exists in the named file. A blocking gate must point at an EXISTING red fixture.",
            gate.slug,
            reference
        );
    }
    Ok(())
}

/// Print the qualification ledger: each gate, whether it blocks, and its red
/// fixture (with a resolved/MISSING marker). Lists the unqualified blocking
/// gates as an explicit finding. Diagnostic only — `check` is the gate.
pub(crate) fn report(repo_root: &Path) {
    println!(
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
                let (file, test_fn) = split_reference(reference).unwrap_or((reference, "?"));
                println!(
                    "  - {} [{authority}] red fixture {file}::{test_fn} ({marker})",
                    gate.slug
                );
            }
            None => println!("  - {} [{authority}] no red fixture", gate.slug),
        }
    }
    if !UNQUALIFIED_BLOCKING_GATES.is_empty() {
        println!(
            "gate-registry-check: {} gate(s) block a real run but are NOT yet qualified (no red \
             fixture); blocking authority withheld until each lands one:",
            UNQUALIFIED_BLOCKING_GATES.len()
        );
        for slug in UNQUALIFIED_BLOCKING_GATES {
            println!("  - {slug}");
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

    /// THE LAW: every blocking gate names an existing red fixture test.
    #[test]
    fn no_blocking_gate_without_a_red_fixture() {
        check(&repo()).expect("every blocking gate must name an existing red fixture test");
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

    /// The honesty ledger: the unqualified-blocking list is now EMPTY — every
    /// structural source lint earned a red fixture and flipped to blocking. Any
    /// gate re-added to the list must still be recorded `has_blocking_authority:
    /// false` with no red fixture (so we never quietly flip one to blocking
    /// without giving it a red fixture and removing it from this list).
    #[test]
    fn unqualified_blocking_gates_are_recorded_nonblocking() {
        assert!(
            UNQUALIFIED_BLOCKING_GATES.is_empty(),
            "every structural source lint now has a red fixture; the unqualified list must be empty"
        );
        for slug in UNQUALIFIED_BLOCKING_GATES {
            let gate = GATES
                .iter()
                .find(|g| g.slug == *slug)
                .unwrap_or_else(|| panic!("unqualified gate `{slug}` missing from GATES"));
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
}
