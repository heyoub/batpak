//! Anti-vacuous red/green fixtures for the agent-safety meta-gate (P1-4).
//!
//! Every detector has BOTH a planted weakening that MUST `Err` without approval
//! AND a green variant (no weakening, or weakening + correct approval) that MUST
//! pass — proving the gate is non-vacuous (it can fail) and non-paranoid (it does
//! not flag strengthening). Diffs are synthetic unified-diff strings, so no git
//! or filesystem is required.

use super::*;

/// A synthetic L4 manifest: `crates/core/src/store/write/writer.rs` is L4 (the
/// real writer-commit seam). Anything else resolves to the default (L1).
fn l4_manifest() -> Vec<AssuranceEntry> {
    vec![AssuranceEntry {
        level: AssuranceLevel::L4,
        seam: Some("writer-commit".to_string()),
        globs: vec!["crates/core/src/store/write/**/*.rs".to_string()],
    }]
}

/// A context with the human label + a trailer by `reviewer` for a PR authored by
/// `agent` — satisfies BOTH the standard rule and the L4 two-person rule.
fn fully_approved_l4() -> ApprovalContext {
    ApprovalContext {
        labels: vec![WEAKEN_APPROVED_LABEL.to_string()],
        pr_author: Some("agent".to_string()),
        weaken_ok_trailers: vec![WeakenTrailer {
            reason: "deliberate, reviewed relaxation".to_string(),
            author: Some("reviewer".to_string()),
        }],
    }
}

/// Label + trailer, but the trailer is by the SAME author as the PR — satisfies
/// the standard rule, FAILS the L4 two-person rule.
fn approved_same_author() -> ApprovalContext {
    ApprovalContext {
        labels: vec![WEAKEN_APPROVED_LABEL.to_string()],
        pr_author: Some("agent".to_string()),
        weaken_ok_trailers: vec![WeakenTrailer {
            reason: "self-approved".to_string(),
            author: Some("agent".to_string()),
        }],
    }
}

/// No approval at all.
fn no_approval() -> ApprovalContext {
    ApprovalContext::default()
}

// ---------------------------------------------------------------------------
// Threshold lowered (CRITICAL_SEAM_MIN_CATCH_PCT 85 -> 70)
// ---------------------------------------------------------------------------

const LOWER_CRITICAL_SEAM_DIFF: &str = "\
diff --git a/tools/xtask/src/commands/mutants/lanes.rs b/tools/xtask/src/commands/mutants/lanes.rs
--- a/tools/xtask/src/commands/mutants/lanes.rs
+++ b/tools/xtask/src/commands/mutants/lanes.rs
@@ -7,7 +7,7 @@
-pub(super) const CRITICAL_SEAM_MIN_CATCH_PCT: u32 = 85;
+pub(super) const CRITICAL_SEAM_MIN_CATCH_PCT: u32 = 70;
";

#[test]
fn lowering_critical_seam_threshold_without_approval_errs() {
    let findings = classify_weakening(LOWER_CRITICAL_SEAM_DIFF, &l4_manifest());
    assert_eq!(findings.len(), 1, "exactly one weakening, got {findings:?}");
    assert_eq!(findings[0].kind, WeakeningKind::ThresholdLowered);
    let err = evaluate(LOWER_CRITICAL_SEAM_DIFF, &l4_manifest(), &no_approval())
        .expect_err("a threshold drop with no approval must Err");
    assert!(err.to_string().contains("threshold-lowered"), "got: {err}");
}

#[test]
fn lowering_critical_seam_threshold_with_label_and_trailer_passes() {
    evaluate(
        LOWER_CRITICAL_SEAM_DIFF,
        &l4_manifest(),
        &fully_approved_l4(),
    )
    .expect("approved weakening (standard radius) must pass");
}

// ---------------------------------------------------------------------------
// L4 threshold: same diff but planted in an L4 file -> two-person rule applies
// ---------------------------------------------------------------------------

const LOWER_L4_THRESHOLD_DIFF: &str = "\
diff --git a/crates/core/src/store/write/writer.rs b/crates/core/src/store/write/writer.rs
--- a/crates/core/src/store/write/writer.rs
+++ b/crates/core/src/store/write/writer.rs
@@ -10,7 +10,7 @@
-const DEBT_BUDGET: u32 = 12;
+const DEBT_BUDGET: u32 = 4;
";

#[test]
fn l4_threshold_with_same_author_trailer_still_errs() {
    // The change lands on an L4 file -> blast radius L4 -> needs a second author.
    let findings = classify_weakening(LOWER_L4_THRESHOLD_DIFF, &l4_manifest());
    assert_eq!(findings.len(), 1, "got {findings:?}");
    assert_eq!(findings[0].blast_radius, BlastRadius::L4);
    let err = evaluate(
        LOWER_L4_THRESHOLD_DIFF,
        &l4_manifest(),
        &approved_same_author(),
    )
    .expect_err("L4 weakening with a same-author trailer must still Err");
    assert!(err.to_string().contains("two-person"), "got: {err}");
}

#[test]
fn l4_threshold_with_independent_trailer_passes() {
    evaluate(
        LOWER_L4_THRESHOLD_DIFF,
        &l4_manifest(),
        &fully_approved_l4(),
    )
    .expect("L4 weakening with label + independent trailer must pass");
}

// ---------------------------------------------------------------------------
// Budget raised (DEFAULT_LINE_BUDGET 850 -> 1200)
// ---------------------------------------------------------------------------

const RAISE_LINE_BUDGET_DIFF: &str = "\
diff --git a/tools/integrity/src/structural.rs b/tools/integrity/src/structural.rs
--- a/tools/integrity/src/structural.rs
+++ b/tools/integrity/src/structural.rs
@@ -120,3 +120,3 @@
-const DEFAULT_LINE_BUDGET: usize = 850;
+const DEFAULT_LINE_BUDGET: usize = 1200;
";

#[test]
fn raising_default_line_budget_without_approval_errs() {
    let findings = classify_weakening(RAISE_LINE_BUDGET_DIFF, &l4_manifest());
    assert_eq!(findings.len(), 1, "got {findings:?}");
    assert_eq!(findings[0].kind, WeakeningKind::BudgetRaised);
    evaluate(RAISE_LINE_BUDGET_DIFF, &l4_manifest(), &no_approval())
        .expect_err("raising the line budget without approval must Err");
}

#[test]
fn lowering_default_line_budget_is_not_a_weakening() {
    // Tightening (lowering) the budget is strengthening; it must NOT be flagged.
    let diff = "\
diff --git a/tools/integrity/src/structural.rs b/tools/integrity/src/structural.rs
--- a/tools/integrity/src/structural.rs
+++ b/tools/integrity/src/structural.rs
@@ -120,3 +120,3 @@
-const DEFAULT_LINE_BUDGET: usize = 850;
+const DEFAULT_LINE_BUDGET: usize = 700;
";
    assert!(classify_weakening(diff, &l4_manifest()).is_empty());
    evaluate(diff, &l4_manifest(), &no_approval()).expect("tightening a budget must pass");
}

// ---------------------------------------------------------------------------
// Typed-waiver L4 entry added
// ---------------------------------------------------------------------------

const ADD_L4_WAIVER_DIFF: &str = "\
diff --git a/traceability/typed_waivers.yaml b/traceability/typed_waivers.yaml
--- a/traceability/typed_waivers.yaml
+++ b/traceability/typed_waivers.yaml
@@ -34 +34,9 @@
-[]
+- id: WAIVER-PUBSURF-0001
+  kind: pub-item
+  target: SegmentHeader
+  owner: heyoub
+  expiry: 2026-12-16
+  justification: serialization shape proven via wire fuzz harness
+  blast_radius: L4
+  debt_score: 3
+  adr: ADR-0026
";

#[test]
fn adding_l4_typed_waiver_entry_without_approval_errs() {
    let findings = classify_weakening(ADD_L4_WAIVER_DIFF, &l4_manifest());
    assert!(
        findings
            .iter()
            .any(|w| w.kind == WeakeningKind::WaiverEntryAdded),
        "expected a waiver-entry-added finding, got {findings:?}"
    );
    assert!(
        findings.iter().any(|w| w.blast_radius == BlastRadius::L4),
        "blast_radius: L4 line must elevate the waiver-add to L4, got {findings:?}"
    );
    let err = evaluate(ADD_L4_WAIVER_DIFF, &l4_manifest(), &no_approval())
        .expect_err("adding an L4 waiver without approval must Err");
    assert!(err.to_string().contains("waiver-entry-added"), "got: {err}");
}

#[test]
fn adding_l4_waiver_with_independent_trailer_passes() {
    evaluate(ADD_L4_WAIVER_DIFF, &l4_manifest(), &fully_approved_l4())
        .expect("approved L4 waiver-add must pass");
}

#[test]
fn adding_waiver_schema_comment_block_is_not_a_weakening() {
    // Adding only comments + the empty `[]` container is schema, not an entry.
    let diff = "\
diff --git a/traceability/typed_waivers.yaml b/traceability/typed_waivers.yaml
--- a/traceability/typed_waivers.yaml
+++ b/traceability/typed_waivers.yaml
@@ -0,0 +1,3 @@
+# New schema documentation for the typed waiver gate.
+# Every field below is required unless marked optional.
+[]
";
    assert!(
        classify_weakening(diff, &l4_manifest()).is_empty(),
        "adding waiver schema/comments must not be flagged"
    );
}

// ---------------------------------------------------------------------------
// Mutation enforcement weakened
// ---------------------------------------------------------------------------

#[test]
fn lowering_repo_mutation_phase_errs() {
    let diff = "\
diff --git a/tools/xtask/src/commands/mutants/policy.rs b/tools/xtask/src/commands/mutants/policy.rs
--- a/tools/xtask/src/commands/mutants/policy.rs
+++ b/tools/xtask/src/commands/mutants/policy.rs
@@ -13 +13 @@
-pub(super) const REPO_MUTATION_PHASE: RepoMutationPhase = RepoMutationPhase::Phase2;
+pub(super) const REPO_MUTATION_PHASE: RepoMutationPhase = RepoMutationPhase::Phase0;
";
    let findings = classify_weakening(diff, &l4_manifest());
    assert!(findings
        .iter()
        .any(|w| w.kind == WeakeningKind::MutationEnforcementWeakened));
    evaluate(diff, &l4_manifest(), &no_approval()).expect_err("phase regression must Err");
}

#[test]
fn threshold_to_record_only_errs() {
    let diff = "\
diff --git a/tools/xtask/src/commands/mutants/lanes.rs b/tools/xtask/src/commands/mutants/lanes.rs
--- a/tools/xtask/src/commands/mutants/lanes.rs
+++ b/tools/xtask/src/commands/mutants/lanes.rs
@@ -220,3 +220,1 @@
-            enforcement: MutationEnforcement::Threshold {
-                min_catch_pct: CRITICAL_SEAM_MIN_CATCH_PCT,
-            },
+            enforcement: MutationEnforcement::RecordOnly,
";
    let findings = classify_weakening(diff, &l4_manifest());
    assert!(findings
        .iter()
        .any(|w| w.kind == WeakeningKind::MutationEnforcementWeakened));
    evaluate(diff, &l4_manifest(), &no_approval()).expect_err("Threshold -> RecordOnly must Err");
}

// ---------------------------------------------------------------------------
// Assurance downgrade
// ---------------------------------------------------------------------------

#[test]
fn assurance_level_downgrade_errs() {
    let diff = "\
diff --git a/traceability/assurance_levels.yaml b/traceability/assurance_levels.yaml
--- a/traceability/assurance_levels.yaml
+++ b/traceability/assurance_levels.yaml
@@ -22,3 +22,3 @@
-- level: L4
+- level: L2
   seam: hash-chain-replay
   rationale: lowered for convenience
";
    let findings = classify_weakening(diff, &l4_manifest());
    assert!(findings
        .iter()
        .any(|w| w.kind == WeakeningKind::AssuranceDowngraded));
    let err = evaluate(diff, &l4_manifest(), &approved_same_author())
        .expect_err("an L4 downgrade with a same-author trailer must Err (two-person)");
    assert!(err.to_string().contains("two-person"), "got: {err}");
}

// ---------------------------------------------------------------------------
// Gate re-burial
// ---------------------------------------------------------------------------

#[test]
fn removing_ci_fast_gate_marker_errs() {
    let diff = "\
diff --git a/tools/xtask/src/commands/ci.rs b/tools/xtask/src/commands/ci.rs
--- a/tools/xtask/src/commands/ci.rs
+++ b/tools/xtask/src/commands/ci.rs
@@ -36,5 +36,0 @@
-    crate::public_api::public_api(PublicApiArgs {
-        strict: true,
-        check_baseline: true,
-        bless_baseline: false,
-    })?;
";
    let findings = classify_weakening(diff, &l4_manifest());
    assert!(findings
        .iter()
        .any(|w| w.kind == WeakeningKind::GateReburied));
    evaluate(diff, &l4_manifest(), &no_approval()).expect_err("re-burying a gate must Err");
}

#[test]
fn adding_label_gate_to_ci_yml_step_errs() {
    let diff = "\
diff --git a/.github/workflows/ci.yml b/.github/workflows/ci.yml
--- a/.github/workflows/ci.yml
+++ b/.github/workflows/ci.yml
@@ -100,0 +101 @@
+    if: contains(github.event.pull_request.labels.*.name, 'run-heavy-ci')
";
    let findings = classify_weakening(diff, &l4_manifest());
    assert!(findings
        .iter()
        .any(|w| w.kind == WeakeningKind::GateReburied));
    evaluate(diff, &l4_manifest(), &no_approval())
        .expect_err("label-gating a default-on step must Err");
}

// ---------------------------------------------------------------------------
// Blocking authority removed
// ---------------------------------------------------------------------------

#[test]
fn flipping_blocking_authority_false_errs() {
    let diff = "\
diff --git a/tools/integrity/src/gate_registry.rs b/tools/integrity/src/gate_registry.rs
--- a/tools/integrity/src/gate_registry.rs
+++ b/tools/integrity/src/gate_registry.rs
@@ -45 +45 @@
-        has_blocking_authority: true,
+        has_blocking_authority: false,
";
    let findings = classify_weakening(diff, &l4_manifest());
    assert!(findings
        .iter()
        .any(|w| w.kind == WeakeningKind::BlockingAuthorityRemoved));
    assert!(findings.iter().any(|w| w.blast_radius == BlastRadius::L4));
    evaluate(diff, &l4_manifest(), &no_approval())
        .expect_err("removing blocking authority must Err");
}

#[test]
fn deleting_red_fixture_errs() {
    let diff = "\
diff --git a/tools/integrity/src/gate_registry.rs b/tools/integrity/src/gate_registry.rs
--- a/tools/integrity/src/gate_registry.rs
+++ b/tools/integrity/src/gate_registry.rs
@@ -42,3 +42,1 @@
-        red_fixture_test: Some(
-            \"tools/integrity/src/assurance.rs::missing_seam_glob_fails_lockstep\",
-        ),
+        red_fixture_test: None,
";
    let findings = classify_weakening(diff, &l4_manifest());
    assert!(findings
        .iter()
        .any(|w| w.kind == WeakeningKind::BlockingAuthorityRemoved));
    evaluate(diff, &l4_manifest(), &no_approval()).expect_err("deleting a red fixture must Err");
}

// ---------------------------------------------------------------------------
// Non-weakening cases: strengthening + neutral diffs MUST pass
// ---------------------------------------------------------------------------

#[test]
fn adding_a_new_threshold_constant_is_not_a_weakening() {
    // A brand-new const (only `+` lines) is strengthening, not a decrease.
    let diff = "\
diff --git a/tools/xtask/src/commands/mutants/lanes.rs b/tools/xtask/src/commands/mutants/lanes.rs
--- a/tools/xtask/src/commands/mutants/lanes.rs
+++ b/tools/xtask/src/commands/mutants/lanes.rs
@@ -9,0 +10 @@
+pub(super) const L4_SEAM_MIN_CATCH_PCT: u32 = 90;
";
    assert!(
        classify_weakening(diff, &l4_manifest()).is_empty(),
        "adding a new threshold const must not be a weakening"
    );
    evaluate(diff, &l4_manifest(), &no_approval()).expect("adding a const must pass");
}

#[test]
fn raising_a_threshold_is_not_a_weakening() {
    let diff = "\
diff --git a/tools/xtask/src/commands/mutants/lanes.rs b/tools/xtask/src/commands/mutants/lanes.rs
--- a/tools/xtask/src/commands/mutants/lanes.rs
+++ b/tools/xtask/src/commands/mutants/lanes.rs
@@ -9 +9 @@
-pub(super) const CRITICAL_SEAM_MIN_CATCH_PCT: u32 = 85;
+pub(super) const CRITICAL_SEAM_MIN_CATCH_PCT: u32 = 90;
";
    assert!(
        classify_weakening(diff, &l4_manifest()).is_empty(),
        "raising a catch threshold is strengthening, not a weakening"
    );
}

#[test]
fn ordinary_feature_diff_passes() {
    let diff = "\
diff --git a/crates/core/src/store/read_api.rs b/crates/core/src/store/read_api.rs
--- a/crates/core/src/store/read_api.rs
+++ b/crates/core/src/store/read_api.rs
@@ -10,0 +11,3 @@
+    pub fn new_helper(&self) -> usize {
+        self.count + 1
+    }
";
    assert!(classify_weakening(diff, &l4_manifest()).is_empty());
    evaluate(diff, &l4_manifest(), &no_approval()).expect("a normal feature diff must pass");
}

#[test]
fn flipping_blocking_authority_to_true_is_not_a_weakening() {
    // This Phase-1 PR flips 6 gates TO blocking; that direction is strengthening.
    let diff = "\
diff --git a/tools/integrity/src/gate_registry.rs b/tools/integrity/src/gate_registry.rs
--- a/tools/integrity/src/gate_registry.rs
+++ b/tools/integrity/src/gate_registry.rs
@@ -45 +45 @@
-        has_blocking_authority: false,
+        has_blocking_authority: true,
";
    assert!(
        classify_weakening(diff, &l4_manifest()).is_empty(),
        "flipping a gate TO blocking is strengthening"
    );
}

#[test]
fn adding_a_red_fixture_is_not_a_weakening() {
    let diff = "\
diff --git a/tools/integrity/src/gate_registry.rs b/tools/integrity/src/gate_registry.rs
--- a/tools/integrity/src/gate_registry.rs
+++ b/tools/integrity/src/gate_registry.rs
@@ -42,0 +43,3 @@
+        red_fixture_test: Some(
+            \"tools/integrity/src/meta_gate_tests.rs::lowering_critical_seam_threshold_without_approval_errs\",
+        ),
";
    assert!(
        classify_weakening(diff, &l4_manifest()).is_empty(),
        "adding a red fixture is strengthening"
    );
}

// ---------------------------------------------------------------------------
// Approval-logic unit coverage
// ---------------------------------------------------------------------------

#[test]
fn standard_weakening_needs_both_label_and_trailer() {
    // Label only -> still Err.
    let label_only = ApprovalContext {
        labels: vec![WEAKEN_APPROVED_LABEL.to_string()],
        ..ApprovalContext::default()
    };
    evaluate(LOWER_CRITICAL_SEAM_DIFF, &l4_manifest(), &label_only)
        .expect_err("label without trailer must Err");
    // Trailer only -> still Err.
    let trailer_only = ApprovalContext {
        weaken_ok_trailers: vec![WeakenTrailer {
            reason: "x".into(),
            author: Some("reviewer".into()),
        }],
        ..ApprovalContext::default()
    };
    evaluate(LOWER_CRITICAL_SEAM_DIFF, &l4_manifest(), &trailer_only)
        .expect_err("trailer without label must Err");
}

#[test]
fn empty_l4_manifest_treats_threshold_drop_as_standard() {
    // With no manifest, the writer.rs threshold drop is Standard, so a
    // same-author label+trailer suffices.
    evaluate(LOWER_L4_THRESHOLD_DIFF, &[], &approved_same_author())
        .expect("with no L4 manifest, same-author approval suffices for a standard weakening");
}

// ---------------------------------------------------------------------------
// GAUNT-CAPSNAP: capability-floor downgrades
// ---------------------------------------------------------------------------

const SNAP: &str = "traceability/capability_snapshot.yaml";

fn cap_diff(removed: &str, added: &str) -> String {
    let mut diff =
        format!("diff --git a/{SNAP} b/{SNAP}\n--- a/{SNAP}\n+++ b/{SNAP}\n@@ -9,1 +9,1 @@\n");
    if !removed.is_empty() {
        diff.push_str(&format!("-{removed}\n"));
    }
    if !added.is_empty() {
        diff.push_str(&format!("+{added}\n"));
    }
    diff
}

#[test]
fn enforcement_weakened_errs() {
    let diff = cap_diff(
        "  - { backend: linux, kind: Kill, enforcement: Enforced, evidence: [ProcessTree, TerminalOutcome] }",
        "  - { backend: linux, kind: Kill, enforcement: Mediated, evidence: [ProcessTree, TerminalOutcome] }",
    );
    let findings = classify_weakening(&diff, &l4_manifest());
    assert!(
        findings
            .iter()
            .any(|w| w.kind == WeakeningKind::CapabilityDowngraded
                && w.detail.contains("Enforced -> Mediated")),
        "Enforced->Mediated must be a capability downgrade; got {findings:?}"
    );
    evaluate(&diff, &l4_manifest(), &no_approval())
        .expect_err("an unapproved capability downgrade must Err");
}

#[test]
fn downgrade_to_unsupported_is_l4_two_person() {
    // A drop to Unsupported (capability fully lost) is L4: a same-author trailer
    // is insufficient (two-person rule).
    let diff = cap_diff(
        "  - { backend: linux, kind: NetworkDenyAll, enforcement: Enforced, evidence: [DeniedAttempts] }",
        "  - { backend: linux, kind: NetworkDenyAll, enforcement: Unsupported, evidence: [] }",
    );
    let findings = classify_weakening(&diff, &l4_manifest());
    assert!(
        findings
            .iter()
            .any(|w| w.kind == WeakeningKind::CapabilityDowngraded
                && w.blast_radius == BlastRadius::L4),
        "a downgrade to Unsupported must be L4; got {findings:?}"
    );
    let err = evaluate(&diff, &l4_manifest(), &approved_same_author())
        .expect_err("an L4 capability downgrade with a same-author trailer must Err");
    assert!(err.to_string().contains("two-person"), "got: {err}");
}

#[test]
fn removed_capability_row_is_l4() {
    let diff = cap_diff(
        "  - { backend: linux, kind: ExposePath, enforcement: Enforced, evidence: [MechanismAttestation] }",
        "",
    );
    let findings = classify_weakening(&diff, &l4_manifest());
    assert!(
        findings
            .iter()
            .any(|w| w.kind == WeakeningKind::CapabilityDowngraded
                && w.blast_radius == BlastRadius::L4
                && w.detail.contains("row removed")),
        "a removed (backend,kind) row must be an L4 downgrade; got {findings:?}"
    );
}

#[test]
fn removed_evidence_claim_errs() {
    let diff = cap_diff(
        "  - { backend: linux, kind: Filesystem, enforcement: Enforced, evidence: [AllowedActions, DeniedAttempts, FilesystemDelta, MechanismAttestation] }",
        "  - { backend: linux, kind: Filesystem, enforcement: Enforced, evidence: [AllowedActions, DeniedAttempts, FilesystemDelta] }",
    );
    let findings = classify_weakening(&diff, &l4_manifest());
    assert!(
        findings
            .iter()
            .any(|w| w.kind == WeakeningKind::CapabilityDowngraded
                && w.detail.contains("dropped MechanismAttestation")),
        "a dropped evidence claim must be a downgrade; got {findings:?}"
    );
}

#[test]
fn witness_unproved_errs() {
    let diff = cap_diff(
        "  - { id: INV-HASH-CHAIN-INTEGRITY, witnessed: true }",
        "  - { id: INV-HASH-CHAIN-INTEGRITY, witnessed: false }",
    );
    let findings = classify_weakening(&diff, &l4_manifest());
    assert!(
        findings
            .iter()
            .any(|w| w.kind == WeakeningKind::CapabilityDowngraded
                && w.detail.contains("witness un-proved")),
        "un-proving a witnessed invariant must be a downgrade; got {findings:?}"
    );
}

#[test]
fn strengthening_a_capability_is_not_a_weakening() {
    // Mediated -> Enforced (an upgrade) and a brand-new added cell are NOT flagged.
    let upgrade = cap_diff(
        "  - { backend: macos, kind: Kill, enforcement: Mediated, evidence: [TerminalOutcome] }",
        "  - { backend: macos, kind: Kill, enforcement: Enforced, evidence: [TerminalOutcome] }",
    );
    assert!(
        classify_weakening(&upgrade, &l4_manifest()).is_empty(),
        "Mediated->Enforced is a strengthening, not a weakening"
    );
    let added_only = cap_diff(
        "",
        "  - { backend: linux, kind: NetworkAllowList, enforcement: Enforced, evidence: [NetworkActivity] }",
    );
    assert!(
        classify_weakening(&added_only, &l4_manifest()).is_empty(),
        "a newly-advertised capability cell is a strengthening, not a weakening"
    );
}

#[test]
fn capability_downgrade_with_full_two_person_approval_passes() {
    let diff = cap_diff(
        "  - { backend: linux, kind: NetworkDenyAll, enforcement: Enforced, evidence: [DeniedAttempts] }",
        "  - { backend: linux, kind: NetworkDenyAll, enforcement: Unsupported, evidence: [] }",
    );
    evaluate(&diff, &l4_manifest(), &fully_approved_l4())
        .expect("a fully two-person-approved capability downgrade must pass");
}
