//! Typed, expiring, owned waiver engine (P0-2).
//!
//! The owner's decision is **"kill silent allowlists"**: no silent permanent
//! exemption may survive. Genuine-indirect exemptions become LOUD, TYPED,
//! EXPIRING waivers, validated here on every `structural-check`.
//!
//! This replaces the deleted `traceability/pub_item_allowlist.yaml`. One schema
//! serves ALL gates (dual-ergonomic, not allowlist-specific): each waiver names
//! the gate family it waives via [`WaiverKind`], the target item/id, a real
//! human owner, an ISO-8601 expiry (the gate FAILS the day after), a
//! justification, the blast radius (an assurance level), a 1..=5 debt score, and
//! a resolvable `adr` anchor — generalizing the equivalent-mutant proof registry
//! (`lanes.rs`) and the citation-waiver discipline (`invariant_bridge.rs`).
//!
//! A waiver whose `blast_radius` is `L4` must additionally carry
//! `independent_signoff: true` (the two-person / "raccoon" rule) — an agent may
//! not self-assign an L4 exemption.
//!
//! `today` is injected ([`check_with_today`]) so fixtures stay deterministic and
//! never rot as the wall clock advances.

use crate::anchors::{extract_anchors, resolve_anchor};
use crate::assurance::AssuranceLevel;
use crate::repo_surface::{ensure, load_yaml};
use anyhow::Result;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Allowed `debt_score` range, inclusive.
pub(crate) const DEBT_SCORE_MIN: u8 = 1;
pub(crate) const DEBT_SCORE_MAX: u8 = 5;

/// The waiver file path, repo-root-relative.
pub(crate) const WAIVERS_REL: &str = "traceability/typed_waivers.yaml";

/// Which gate family a waiver exempts. Extend as new gates adopt the engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Deserialize)]
pub(crate) enum WaiverKind {
    /// Public-surface exemption: a `pub` item with no direct test reference.
    #[serde(rename = "pub-item")]
    PubItem,
    /// Invariant-citation exemption.
    #[serde(rename = "invariant-citation")]
    InvariantCitation,
    /// A sanctioned, expiring CAPABILITY DOWNGRADE (GAUNT-CAPSNAP): a backend
    /// `support_matrix()` cell whose enforcement was deliberately lowered, a
    /// (backend,kind) row removed, an evidence claim dropped, or a witness
    /// un-proved. The `target` is `backend:kind` (e.g. `linux:ExposePath`). This
    /// is the DURABLE form a downgrade approval takes; the meta-gate accepts a
    /// matching, in-date waiver added in the same diff as authorization for the
    /// downgrade. A downgrade to `Unsupported` (a fully-lost capability) is L4
    /// blast radius and so requires `independent_signoff: true`.
    #[serde(rename = "capability-downgrade")]
    CapabilityDowngrade,
}

/// One typed waiver as declared in `typed_waivers.yaml`. `serde(deny_unknown_fields)`
/// keeps a typo'd field from being silently dropped (anti-laundering).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Waiver {
    pub(crate) id: String,
    pub(crate) kind: WaiverKind,
    pub(crate) target: String,
    pub(crate) owner: String,
    /// ISO-8601 `YYYY-MM-DD`. The gate fails the day AFTER this date.
    pub(crate) expiry: String,
    pub(crate) justification: String,
    pub(crate) blast_radius: AssuranceLevel,
    pub(crate) debt_score: u8,
    /// Resolvable anchor (`INV-*` or `ADR-NNNN`).
    pub(crate) adr: String,
    /// Required to be `true` when `blast_radius` is `L4`.
    #[serde(default)]
    pub(crate) independent_signoff: bool,
}

/// A simple ISO-8601 calendar date used for expiry comparison. Avoids a chrono
/// dependency: lexicographic comparison of `(year, month, day)` is the calendar
/// order, and parsing rejects anything that is not a real `YYYY-MM-DD`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct IsoDate {
    pub(crate) year: u16,
    pub(crate) month: u8,
    pub(crate) day: u8,
}

impl IsoDate {
    /// Parse a strict `YYYY-MM-DD` date. Returns `None` on any malformed input
    /// (wrong shape, non-numeric, out-of-range month/day).
    pub(crate) fn parse(s: &str) -> Option<IsoDate> {
        let mut parts = s.split('-');
        let year: u16 = parts.next()?.parse().ok()?;
        let month: u8 = parts.next()?.parse().ok()?;
        let day: u8 = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return None;
        }
        Some(IsoDate { year, month, day })
    }
}

pub(crate) fn waivers_path(repo_root: &Path) -> std::path::PathBuf {
    repo_root.join(WAIVERS_REL)
}

/// Load the waiver list. A missing file is an empty list (the desired end state
/// after triage: zero silent allowlist, zero live waivers).
pub(crate) fn load_waivers(repo_root: &Path) -> Result<Vec<Waiver>> {
    let path = waivers_path(repo_root);
    if !path.exists() {
        return Ok(Vec::new());
    }
    load_yaml(&path)
}

/// Targets exempted for a specific gate kind. The consuming gate calls this to
/// learn which items it may skip (e.g. `public_surface` for `pub-item`).
pub(crate) fn targets_for(waivers: &[Waiver], kind: WaiverKind) -> BTreeSet<String> {
    waivers
        .iter()
        .filter(|w| w.kind == kind)
        .map(|w| w.target.clone())
        .collect()
}

/// Validate every waiver against the reference date `today`. Fails RED on:
/// expired waiver, any blank required field, an unresolvable `adr`, a
/// `debt_score` outside `1..=5`, a duplicate `id`, or an `L4` blast radius
/// without `independent_signoff: true`.
pub(crate) fn validate(repo_root: &Path, waivers: &[Waiver], today: IsoDate) -> Result<()> {
    let mut seen_ids: BTreeMap<&str, ()> = BTreeMap::new();
    for w in waivers {
        ensure(
            !w.id.trim().is_empty(),
            "typed_waivers: a waiver has a blank `id`",
        )?;
        ensure(
            seen_ids.insert(w.id.as_str(), ()).is_none(),
            format!("typed_waivers: duplicate waiver id `{}`", w.id),
        )?;
        ensure(
            !w.target.trim().is_empty(),
            format!("typed_waivers: waiver `{}` has a blank `target`", w.id),
        )?;
        ensure(
            !w.owner.trim().is_empty(),
            format!(
                "typed_waivers: waiver `{}` has a blank `owner`; every waiver needs a real human owner",
                w.id
            ),
        )?;
        ensure(
            !w.justification.trim().is_empty(),
            format!(
                "typed_waivers: waiver `{}` has a blank `justification`",
                w.id
            ),
        )?;
        ensure(
            (DEBT_SCORE_MIN..=DEBT_SCORE_MAX).contains(&w.debt_score),
            format!(
                "typed_waivers: waiver `{}` debt_score {} is outside {DEBT_SCORE_MIN}..={DEBT_SCORE_MAX}",
                w.id, w.debt_score
            ),
        )?;

        // Expiry: must parse and must not be in the past relative to `today`.
        let expiry = IsoDate::parse(&w.expiry).ok_or_else(|| {
            anyhow::anyhow!(
                "typed_waivers: waiver `{}` expiry `{}` is not a valid ISO-8601 YYYY-MM-DD date",
                w.id,
                w.expiry
            )
        })?;
        ensure(
            expiry >= today,
            format!(
                "typed_waivers: waiver `{}` EXPIRED on {} (today {:04}-{:02}-{:02}); renew with a fresh expiry + justification or remove it",
                w.id, w.expiry, today.year, today.month, today.day
            ),
        )?;

        // adr must resolve to a real anchor (anti-laundering): no waiver without
        // a real ADR / INV anchor.
        let anchors = extract_anchors(&w.adr);
        ensure(
            !anchors.is_empty(),
            format!(
                "typed_waivers: waiver `{}` adr `{}` does not contain a resolvable anchor (expected INV-* or ADR-NNNN)",
                w.id, w.adr
            ),
        )?;
        let resolves = anchors
            .iter()
            .any(|anchor| resolve_anchor(anchor, repo_root, &BTreeSet::new()));
        ensure(
            resolves,
            format!(
                "typed_waivers: waiver `{}` adr `{}` does not resolve to a real ADR/invariant",
                w.id, w.adr
            ),
        )?;

        // L4 blast radius requires independent sign-off (two-person rule).
        if w.blast_radius == AssuranceLevel::L4 {
            ensure(
                w.independent_signoff,
                format!(
                    "typed_waivers: waiver `{}` has blast_radius L4 but lacks `independent_signoff: true`; an L4 exemption requires independent sign-off and may not be agent-self-assigned",
                    w.id
                ),
            )?;
        }
    }
    Ok(())
}

/// Today's date from the system clock as an [`IsoDate`] (UTC). Used by the
/// production gate; tests inject a fixed date via [`check_with_today`].
fn today_utc() -> IsoDate {
    // Days since the Unix epoch, converted to a proleptic-Gregorian calendar
    // date. No external clock crate; deterministic and dependency-free.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    civil_from_days(days)
}

/// Convert days-since-1970-01-01 to a calendar date (Howard Hinnant's
/// `civil_from_days`, public-domain algorithm). Correct across all Gregorian
/// dates; here only the near-future range matters.
fn civil_from_days(z: i64) -> IsoDate {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    // Components are bounded by construction (year in the realistic CE range,
    // m in 1..=12, d in 1..=31); `try_from` keeps the conversion lint-clean and
    // saturates defensively rather than wrapping.
    IsoDate {
        year: u16::try_from(year).unwrap_or(u16::MAX),
        month: u8::try_from(m).unwrap_or(0),
        day: u8::try_from(d).unwrap_or(0),
    }
}

/// Load and validate all waivers against an injected reference date.
pub(crate) fn check_with_today(repo_root: &Path, today: IsoDate) -> Result<()> {
    let waivers = load_waivers(repo_root)?;
    validate(repo_root, &waivers, today)?;
    let aggregate: u32 = waivers.iter().map(|w| u32::from(w.debt_score)).sum();
    outln!(
        "typed-waivers: ok ({} waiver(s), aggregate debt {})",
        waivers.len(),
        aggregate
    );
    Ok(())
}

/// Production entry point: validate the committed waivers against today's date.
/// Called from `structural::run`.
pub(crate) fn check(repo_root: &Path) -> Result<()> {
    check_with_today(repo_root, today_utc())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo_surface::repo_root;

    fn repo() -> std::path::PathBuf {
        repo_root().expect("repo root resolves from tools/integrity")
    }

    // A fixed reference date so every fixture is deterministic and never rots.
    const TODAY: IsoDate = IsoDate {
        year: 2026,
        month: 6,
        day: 19,
    };

    fn base_waiver() -> Waiver {
        Waiver {
            id: "WAIVER-PUBSURF-0001".into(),
            kind: WaiverKind::PubItem,
            target: "SegmentHeader".into(),
            owner: "heyoub".into(),
            expiry: "2026-12-16".into(),
            justification: "serialization shape proven via wire and fuzz coverage".into(),
            blast_radius: AssuranceLevel::L2,
            debt_score: 3,
            adr: "ADR-0026".into(),
            independent_signoff: false,
        }
    }

    #[test]
    fn iso_date_parse_and_order() {
        assert!(IsoDate::parse("2026-13-01").is_none(), "month 13 rejected");
        assert!(IsoDate::parse("2026-01-32").is_none(), "day 32 rejected");
        assert!(IsoDate::parse("2026-01").is_none(), "missing day rejected");
        assert!(IsoDate::parse("not-a-date").is_none());
        let earlier = IsoDate::parse("2026-06-18").expect("valid");
        let later = IsoDate::parse("2026-06-19").expect("valid");
        assert!(earlier < later);
    }

    // GREEN: the committed (empty) waiver set validates.
    #[test]
    fn committed_waivers_are_green() {
        check_with_today(&repo(), TODAY).expect("committed typed_waivers.yaml must validate green");
    }

    // GREEN: an in-date, fully-typed, anchored waiver passes.
    #[test]
    fn well_formed_in_date_waiver_passes() {
        let waivers = vec![base_waiver()];
        validate(&repo(), &waivers, TODAY).expect("a well-formed in-date waiver must pass");
    }

    // RED: an expired waiver fails.
    #[test]
    fn expired_waiver_fails() {
        let mut w = base_waiver();
        w.expiry = "2026-06-18".into(); // one day before TODAY
        let err = validate(&repo(), &[w], TODAY).expect_err("expired waiver must fail");
        assert!(
            err.to_string().contains("EXPIRED"),
            "error must say EXPIRED, got: {err}"
        );
    }

    // RED: a missing required field (owner) fails.
    #[test]
    fn missing_owner_fails() {
        let mut w = base_waiver();
        w.owner = "   ".into();
        let err = validate(&repo(), &[w], TODAY).expect_err("blank owner must fail");
        assert!(
            err.to_string().contains("owner"),
            "error must name the owner field, got: {err}"
        );
    }

    // RED: an unresolvable adr (untraced) fails.
    #[test]
    fn unresolvable_adr_fails() {
        let mut w = base_waiver();
        w.adr = "ADR-9999".into(); // no such ADR file
        let err = validate(&repo(), &[w], TODAY).expect_err("unresolvable adr must fail");
        assert!(
            err.to_string().contains("does not resolve"),
            "error must say the adr does not resolve, got: {err}"
        );
    }

    // RED: an adr with no anchor token at all fails.
    #[test]
    fn adr_without_anchor_token_fails() {
        let mut w = base_waiver();
        w.adr = "see the design doc".into();
        let err = validate(&repo(), &[w], TODAY).expect_err("anchorless adr must fail");
        assert!(
            err.to_string().contains("resolvable anchor"),
            "error must demand a resolvable anchor, got: {err}"
        );
    }

    // RED: a debt_score out of range fails.
    #[test]
    fn debt_score_out_of_range_fails() {
        let mut w = base_waiver();
        w.debt_score = 9;
        let err = validate(&repo(), &[w], TODAY).expect_err("debt_score 9 must fail");
        assert!(
            err.to_string().contains("debt_score"),
            "error must name debt_score, got: {err}"
        );
    }

    // RED: an L4-blast-radius waiver WITHOUT independent_signoff fails.
    #[test]
    fn l4_waiver_without_signoff_fails() {
        let mut w = base_waiver();
        w.blast_radius = AssuranceLevel::L4;
        w.adr = "ADR-0014".into(); // durable-frontier ADR, resolves
        w.independent_signoff = false;
        let err = validate(&repo(), &[w], TODAY)
            .expect_err("an L4 waiver without independent sign-off must fail");
        assert!(
            err.to_string().contains("independent_signoff"),
            "error must demand independent_signoff, got: {err}"
        );
    }

    // GREEN: the same L4 waiver WITH independent_signoff passes.
    #[test]
    fn l4_waiver_with_signoff_passes() {
        let mut w = base_waiver();
        w.blast_radius = AssuranceLevel::L4;
        w.adr = "ADR-0014".into();
        w.independent_signoff = true;
        validate(&repo(), &[w], TODAY).expect("L4 waiver with sign-off must pass");
    }

    // RED: duplicate ids fail.
    #[test]
    fn duplicate_id_fails() {
        let a = base_waiver();
        let b = base_waiver();
        let err = validate(&repo(), &[a, b], TODAY).expect_err("duplicate ids must fail");
        assert!(
            err.to_string().contains("duplicate waiver id"),
            "error must report the duplicate id, got: {err}"
        );
    }

    #[test]
    fn targets_for_filters_by_kind() {
        let mut pub_item = base_waiver();
        pub_item.target = "Foo".into();
        let mut citation = base_waiver();
        citation.id = "WAIVER-CITE-0001".into();
        citation.kind = WaiverKind::InvariantCitation;
        citation.target = "INV-BAR".into();
        let waivers = vec![pub_item, citation];
        let pub_targets = targets_for(&waivers, WaiverKind::PubItem);
        assert!(pub_targets.contains("Foo"));
        assert!(!pub_targets.contains("INV-BAR"));
    }
}
