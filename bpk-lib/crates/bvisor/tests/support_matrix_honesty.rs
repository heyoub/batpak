//! GAUNTLET bvisor backend-ladder (#74 step a) — SupportMatrix TABLE-HONESTY.
//!
//! The cardinal anti-fake rule: a [`SupportMatrix`] that claims more enforcement
//! than the backend can deliver is a LIE the gauntlet must catch. Every
//! `Unsupported`/`Mediated` cell in SCOPE §4 is a LOAD-BEARING honest answer.
//! This fixture pins the load-bearing cells (macOS Mount, Wasm Spawn/Kill,
//! Windows Mount, all v1 AllowList-net) and proves the honesty CHECK ITSELF bites:
//! a `#[cfg(gauntlet_red_fixture)]` variant flips ONE cell to a lie and asserts
//! the honesty check would PASS — which is FALSE for a real check, so the red
//! half FAILS.
//!
//! SCOPE: this is the TABLE-honesty gate only. The full G1–G13 lie-detection
//! against a real `execute()` (live GroundTruth diff) lands in step (b); it is
//! NOT faked here.
//!
//! No `#![allow(..)]` of any kind; every assertion is `assert!`/`assert_eq!`.

use bvisor::{linux, macos, wasm, windows};
use bvisor::{Enforcement, RequirementKind, SupportMatrix};

/// One load-bearing honesty expectation: a `(kind, expected enforcement)` cell
/// that SCOPE §4 pins for a platform. The honesty predicate holds iff the
/// matrix's family best-case for `kind` matches `expected` EXACTLY.
struct HonestyCell {
    kind: RequirementKind,
    expected: Enforcement,
}

/// The honesty predicate under test: does `matrix` report EXACTLY the honest
/// enforcement SCOPE §4 demands for every pinned cell? A `false` here means the
/// table is LYING (over- or under-claiming) about a load-bearing cell.
///
/// This is the single function the GREEN test and the RED fixture both exercise,
/// so the red fixture proves the SAME check that guards production bites.
fn table_is_honest(matrix: &SupportMatrix, cells: &[HonestyCell]) -> bool {
    cells
        .iter()
        .all(|cell| matrix.best_case_for(cell.kind).enforcement == cell.expected)
}

/// The load-bearing honest cells per platform (SCOPE §4). These are the cells the
/// gauntlet must NEVER let drift to a stronger (lying) claim.
fn linux_cells() -> Vec<HonestyCell> {
    vec![HonestyCell {
        kind: RequirementKind::NetworkAllowList,
        expected: Enforcement::Unsupported, // v1: no broker.
    }]
}

fn wasm_cells() -> Vec<HonestyCell> {
    vec![
        HonestyCell {
            kind: RequirementKind::ChildSpawn,
            expected: Enforcement::Unsupported, // structural: no fork.
        },
        HonestyCell {
            kind: RequirementKind::Kill,
            expected: Enforcement::Unsupported, // structural: no kill.
        },
        HonestyCell {
            kind: RequirementKind::ExposePath,
            expected: Enforcement::Unsupported, // structural: no mount.
        },
        HonestyCell {
            kind: RequirementKind::NetworkAllowList,
            expected: Enforcement::Unsupported,
        },
    ]
}

fn windows_cells() -> Vec<HonestyCell> {
    vec![
        HonestyCell {
            kind: RequirementKind::ExposePath,
            expected: Enforcement::Mediated, // no first-class bind mount.
        },
        HonestyCell {
            kind: RequirementKind::NetworkAllowList,
            expected: Enforcement::Mediated, // WFP mediation.
        },
    ]
}

fn macos_cells() -> Vec<HonestyCell> {
    vec![
        HonestyCell {
            kind: RequirementKind::ExposePath,
            expected: Enforcement::Unsupported, // no per-boundary bind mount.
        },
        HonestyCell {
            kind: RequirementKind::NetworkAllowList,
            expected: Enforcement::Unsupported,
        },
        HonestyCell {
            kind: RequirementKind::Filesystem,
            expected: Enforcement::Mediated, // deprecated Seatbelt.
        },
        HonestyCell {
            kind: RequirementKind::Kill,
            expected: Enforcement::Mediated, // pgid only, no atomic subtree.
        },
    ]
}

/// GREEN: every platform's real `support_matrix()` is HONEST about its
/// load-bearing cells. If a future edit inflates (say) macOS Mount to Enforced,
/// this fails — the production guard.
#[test]
fn every_platform_table_is_honest_about_load_bearing_cells() {
    assert!(
        table_is_honest(&linux::support_matrix(), &linux_cells()),
        "linux NetworkAllowList must stay Unsupported (v1, no broker)"
    );
    assert!(
        table_is_honest(&wasm::support_matrix(), &wasm_cells()),
        "wasm Spawn/Kill/Mount/AllowList must stay structurally Unsupported"
    );
    assert!(
        table_is_honest(&windows::support_matrix(), &windows_cells()),
        "windows Mount/AllowList must stay Mediated"
    );
    assert!(
        table_is_honest(&macos::support_matrix(), &macos_cells()),
        "macos Mount/AllowList Unsupported + FS/Kill Mediated must hold"
    );
}

/// GREEN (anti-vacuity): the honesty predicate ACTUALLY BITES. We hand it a
/// deliberately-lying matrix (macOS Mount claimed Enforced) and assert the
/// predicate returns FALSE. Without this, a predicate that always returned `true`
/// would pass the green test above vacuously.
#[test]
fn honesty_predicate_rejects_a_lying_table() {
    let lying = lying_macos_mount_matrix();
    assert!(
        !table_is_honest(&lying, &macos_cells()),
        "honesty predicate MUST reject a table that claims macOS Mount is Enforced"
    );
}

/// Construct a LYING macOS matrix: identical to the honest one EXCEPT
/// `ExposePath` (Mount) is inflated from the honest `Unsupported` to a fabricated
/// `Enforced`. This is the exact class of lie the gauntlet exists to catch.
///
/// Built from the public `RequirementKind` surface so it cannot accidentally
/// reuse the real (honest) cell. macOS has no per-boundary bind mount, so
/// claiming `Enforced` here would be fake confinement.
fn lying_macos_mount_matrix() -> SupportMatrix {
    use bvisor::{EvidenceClaim, SupportVerdict};
    use std::collections::BTreeMap;

    let mut best = BTreeMap::new();
    // The single fabricated cell — the lie.
    best.insert(
        RequirementKind::ExposePath,
        SupportVerdict::new(
            Enforcement::Enforced,
            [EvidenceClaim::MechanismAttestation].into_iter().collect(),
        ),
    );
    SupportMatrix::from_best_case(best)
}

/// RED FIXTURE (`--cfg gauntlet_red_fixture`): asserts the ILLEGAL outcome — that
/// the honesty predicate PASSES on the lying macOS-Mount table (i.e. the lie
/// escapes). A real, biting predicate ALWAYS rejects that lie, so this assertion
/// is FALSE and the red half FAILS — proving the table-honesty gate is not
/// vacuous. (Full G1–G13 lie-detection against `execute()` is step (b).)
#[cfg(gauntlet_red_fixture)]
#[test]
fn red_fixture_lying_mount_must_escape() {
    let lying = lying_macos_mount_matrix();
    assert!(
        table_is_honest(&lying, &macos_cells()),
        "RED FIXTURE: asserts the (illegal) lie-escapes outcome; MUST fail because a biting \
         honesty check rejects a macOS-Mount-claimed-Enforced table"
    );
}
