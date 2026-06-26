//! GAUNT-CAPSNAP red+green fixtures. The mirror gate (`assert_mirror`) bites on
//! drift; the per-shape downgrade fixtures (enforcement weakened, evidence/row
//! removed, witness un-proved) are the canonical CAPABILITY DOWNGRADE shapes the
//! meta-gate must also catch in a diff (see `meta_gate_tests.rs`).

use super::*;
use crate::repo_surface::repo_root;

fn repo() -> std::path::PathBuf {
    repo_root().expect("repo root resolves from tools/integrity")
}

fn derived() -> Snapshot {
    derive_snapshot(&repo())
        .expect("derive snapshot from source")
        .sorted()
}

// ── GREEN: the committed floor mirrors source ───────────────────────────────

#[test]
fn committed_file_is_current() {
    check(&repo()).expect("committed capability_snapshot.yaml must mirror source");
}

#[test]
fn render_parse_round_trips() {
    let snapshot = derived();
    let parsed = parse(&render(&snapshot))
        .expect("rendered snapshot must parse")
        .sorted();
    assert_eq!(parsed, snapshot, "render -> parse must round-trip exactly");
}

#[test]
fn split_manifest_validates_replacement_cells() {
    let mut snapshot = derived();
    let replacement = snapshot
        .aspiration_floor
        .iter()
        .find(|cell| cell.backend == "linux")
        .expect("linux floor row exists")
        .kind
        .clone();
    snapshot.split_manifest.push(SplitManifestRow {
        backend: "linux".to_string(),
        from_kind: "AggregateCapability".to_string(),
        to_kinds: vec![replacement],
        reason: "split-for-test".to_string(),
    });
    assert_mirror(&render(&snapshot), &derived())
        .expect("valid split manifest row is preserved and mirror-compatible");

    let mut invalid = snapshot.clone();
    invalid
        .split_manifest
        .first_mut()
        .expect("split row exists")
        .to_kinds = vec!["MissingReplacement".to_string()];
    let err = assert_mirror(&render(&invalid), &derived())
        .expect_err("split manifest pointing at a missing replacement must fail");
    assert!(
        err.to_string().contains("split_manifest")
            && err.to_string().contains("MissingReplacement"),
        "got: {err}"
    );
}

#[test]
fn derived_floor_is_non_empty_and_covers_every_backend() {
    let snapshot = derived();
    assert!(
        snapshot.aspiration_floor.len() >= 40,
        "four backends each advertise 17 RequirementKind cells; got {}",
        snapshot.aspiration_floor.len()
    );
    for backend in BACKENDS {
        assert!(
            snapshot
                .aspiration_floor
                .iter()
                .any(|c| c.backend == *backend),
            "backend `{backend}` missing from the derived floor"
        );
    }
    // The load-bearing honest fail-closed cell: linux NetworkAllowList Unsupported.
    assert!(
        snapshot
            .aspiration_floor
            .iter()
            .any(|c| c.backend == "linux"
                && c.kind == "NetworkAllowList"
                && c.enforcement == "Unsupported"),
        "linux NetworkAllowList must be captured as Unsupported (v1, no broker)"
    );
}

// ── The pure extractor + its anti-empty hardening guard ─────────────────────

const SUPPORT_MATRIX_NO_INSERTS: &str = r#"
use std::collections::BTreeMap;
pub fn support_matrix() -> SupportMatrix {
    let best = BTreeMap::new();
    SupportMatrix::from_best_case(best)
}
"#;

const SUPPORT_MATRIX_ONE_INSERT: &str = r#"
pub fn support_matrix() -> SupportMatrix {
    let mut best = BTreeMap::new();
    insert(
        &mut best,
        RequirementKind::Filesystem,
        Enforcement::Enforced,
        &[EvidenceClaim::DeniedAttempts, EvidenceClaim::AllowedActions],
    );
    SupportMatrix::from_best_case(best)
}
"#;

#[test]
fn empty_support_matrix_extraction_is_red() {
    let file = syn::parse_file(SUPPORT_MATRIX_NO_INSERTS).expect("parse synthetic source");
    let err = extract_aspiration_floor("synthetic", &file)
        .expect_err("a support_matrix() with zero inserts must be RED, never silently empty");
    assert!(
        err.to_string().contains("ZERO best-case cells"),
        "guard must name the empty-extraction failure, got: {err}"
    );
}

#[test]
fn extractor_reads_kind_enforcement_and_sorted_evidence() {
    let file = syn::parse_file(SUPPORT_MATRIX_ONE_INSERT).expect("parse synthetic source");
    let cells = extract_aspiration_floor("synthetic", &file).expect("one insert extracts one cell");
    assert_eq!(cells.len(), 1);
    let cell = &cells[0];
    assert_eq!(cell.kind, "Filesystem");
    assert_eq!(cell.enforcement, "Enforced");
    // Evidence is sorted + deduplicated for a canonical, stable on-disk form.
    assert_eq!(cell.evidence, vec!["AllowedActions", "DeniedAttempts"]);
}

// ── RED: every CAPABILITY DOWNGRADE shape fails the mirror ───────────────────

/// The registered gate red fixture: a stale committed snapshot (here a weakened
/// cell) fails `assert_mirror`. Anti-vacuous — it asserts an `Err`.
#[test]
fn downgrade_enforced_to_mediated_fails() {
    let snapshot = derived();
    let mut tampered = snapshot.clone();
    let cell = tampered
        .aspiration_floor
        .iter_mut()
        .find(|c| c.enforcement == "Enforced")
        .expect("some Enforced cell exists to weaken");
    cell.enforcement = "Mediated".to_string();
    let err = assert_mirror(&render(&tampered), &snapshot)
        .expect_err("an enforcement weakened to Mediated must fail the mirror");
    assert!(
        err.to_string().contains("STALE"),
        "error must flag the stale/downgraded floor, got: {err}"
    );
}

#[test]
fn removed_evidence_claim_fails() {
    let snapshot = derived();
    let mut tampered = snapshot.clone();
    let cell = tampered
        .aspiration_floor
        .iter_mut()
        .find(|c| !c.evidence.is_empty())
        .expect("some cell carries evidence");
    cell.evidence.pop();
    let err = assert_mirror(&render(&tampered), &snapshot)
        .expect_err("a removed evidence claim must fail the mirror");
    assert!(err.to_string().contains("STALE"), "got: {err}");
}

#[test]
fn removed_row_fails() {
    let snapshot = derived();
    let mut tampered = snapshot.clone();
    tampered.aspiration_floor.pop();
    let err = assert_mirror(&render(&tampered), &snapshot)
        .expect_err("a removed (backend,kind) row must fail the mirror");
    assert!(err.to_string().contains("STALE"), "got: {err}");
}

#[test]
fn witness_true_to_false_fails() {
    let snapshot = derived();
    let mut tampered = snapshot.clone();
    let row = tampered
        .witnesses
        .iter_mut()
        .find(|w| w.witnessed)
        .expect("some invariant is witnessed");
    row.witnessed = false;
    let err = assert_mirror(&render(&tampered), &snapshot)
        .expect_err("a witnessed invariant un-proved must fail the mirror");
    assert!(err.to_string().contains("STALE"), "got: {err}");
}

// ── The enforcement security order the meta-gate ranks downgrades by ─────────

#[test]
fn enforcement_rank_orders_by_security_strength() {
    assert!(enforcement_rank("Enforced") > enforcement_rank("Mediated"));
    assert!(enforcement_rank("Mediated") > enforcement_rank("Unsupported"));
    // An unknown/garbage value ranks below every real grade so it never reads as
    // "stronger" in a diff comparison.
    assert!(enforcement_rank("Unsupported") > enforcement_rank("Bogus"));
}
