#![cfg(feature = "alloc-count")]
//! GAUNT-CPLX-EXP: perf-as-contract complexity-EXPONENT + WCET gate.
//!
//! Invariant: INV-COMPLEXITY-EXPONENT-BOUNDED.
//!
//! PROVES, HARDWARE-INDEPENDENTLY and DETERMINISTICALLY, that a real Store
//! read operation's COST does not silently regress into a worse asymptotic
//! class. The cost metric is the ALLOCATION COUNT measured by [`CountingAlloc`]
//! (process-wide atomics), NEVER wall-clock nanoseconds — wall-clock budgets
//! are flaky (a P0 in this repo) and the `no-wallclock-asserts` structural gate
//! rejects `Instant::now()` + elapsed assertions in correctness tests.
//!
//! Two halves, both count-based:
//!
//! 1. EXPONENT (asymptotic class). We measure a full-region REPLAY READ —
//!    `Store::query(Region::all())` to enumerate every visible entry, then
//!    `Store::get` to decode each event's payload — at geometrically increasing
//!    input sizes n (64,128,256,512,1024), fit a least-squares slope of
//!    log(cost) vs log(n), and assert the slope stays under a linear-ish budget.
//!    Decoding each event allocates once per event, so the cost is genuinely
//!    LINEAR in n (slope ~= 1); a regression to super-linear scan/copy behavior
//!    would push the slope past the budget. (Enumerating alone would only
//!    reallocate the result `Vec` — a LOGARITHMIC artifact that does not witness
//!    the O(n) read work, so we deliberately decode the payloads too.) The slope
//!    is a RATIO of logs, so it is hardware-independent: it does not matter how
//!    many allocations a single op costs on this machine, only how that count
//!    GROWS with n.
//!
//! 2. WCET / p100 (worst-case bound). Over the SAME declared input
//!    distribution (the largest n), the worst observed per-op allocation count
//!    must stay under a fixed budget — a COUNT, not a duration. This is the
//!    deterministic stand-in for a true WCET: we bound the worst-case counted
//!    work, which is exactly what a hardware-independent gate can assert.
//!
//! The RED fixture ([`super_linear_dataset_is_rejected`]) feeds the SAME gate
//! logic a deliberately QUADRATIC dataset and asserts [`check_complexity`]
//! returns `Err`, proving the gate actually bites a super-linear regression
//! rather than being a green-only tautology.
//!
//! This is a DEDICATED single-test binary because `#[global_allocator]` is
//! process-wide: installing [`CountingAlloc`] here keeps the counters free of
//! allocations from unrelated tests. Run with `--features alloc-count`.
//!
//! Slug: GAUNT-CPLX-EXP / complexity_exponent

use batpak::coordinate::{Coordinate, Region};
use batpak::event::EventKind;
use batpak::store::alloc::CountingAlloc;
use batpak::store::{Store, StoreConfig};
use tempfile::TempDir;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

const KIND: EventKind = EventKind::custom(0xC, 1);

/// Geometric input sizes for the log-log fit. Geometric spacing gives evenly
/// spaced log(n) abscissae so the least-squares slope is well-conditioned.
const SIZES: &[u64] = &[64, 128, 256, 512, 1024];

/// Linear-cost budget for the fitted log-log slope.
///
/// A full-region query is ~linear (slope ~= 1) in the number of visible
/// entries. The budget allows generous headroom for sub-linear bookkeeping
/// terms and small-n constant-factor noise while still rejecting a regression
/// to a super-linear class (slope ~= 2 for quadratic). If a change legitimately
/// raises the exponent, bump this with a one-line justification.
#[cfg(not(gauntlet_red_fixture))]
const SLOPE_BUDGET: f64 = 1.35;

/// RED fixture: under `--cfg gauntlet_red_fixture` the slope budget is flipped
/// below linear (0.5), so the real ~linear replay read's measured slope EXCEEDS
/// it and the live measurement assertion fails. This proves the exponent gate is
/// anti-vacuous (it reds on a real over-budget exponent) and is exercised by the
/// `gauntlet-red-fixtures-bite` lane / `cargo xtask prove-gates-bite`.
#[cfg(gauntlet_red_fixture)]
const SLOPE_BUDGET: f64 = 0.5;

/// Worst-case per-op allocation budget (a COUNT, the deterministic WCET stand-in).
///
/// The worst single full-region replay read over the largest declared input
/// size must allocate no more than this many times. Loose on purpose: the gate
/// exists and is deterministic, not tight.
#[cfg(not(gauntlet_red_fixture))]
const WCET_ALLOC_BUDGET: u64 = 200_000;

/// RED fixture: flip the WCET budget to 0 so a real replay read (which always
/// allocates while decoding payloads) exceeds it and the p100 assertion reds.
#[cfg(gauntlet_red_fixture)]
const WCET_ALLOC_BUDGET: u64 = 0;

// ---------------------------------------------------------------------------
// Pure, isolated gate logic (no I/O, no allocator) — unit-testable directly.
// ---------------------------------------------------------------------------

/// Least-squares slope of a line fit to `points` interpreted as `(x, y)` pairs.
///
/// For complexity fitting the caller passes `(log(n), log(cost))` pairs; the
/// returned slope is then the estimated complexity EXPONENT. Pure: same input
/// always yields the same output, so it is hardware-independent and trivially
/// unit-testable in isolation. Returns `0.0` for fewer than two points or a
/// degenerate (zero-variance) x column.
#[must_use]
pub fn loglog_slope(points: &[(f64, f64)]) -> f64 {
    let n = points.len();
    if n < 2 {
        return 0.0;
    }
    let count = n as f64;
    let sum_x: f64 = points.iter().map(|p| p.0).sum();
    let sum_y: f64 = points.iter().map(|p| p.1).sum();
    let mean_x = sum_x / count;
    let mean_y = sum_y / count;
    let mut cov = 0.0;
    let mut var_x = 0.0;
    for &(x, y) in points {
        let dx = x - mean_x;
        cov += dx * (y - mean_y);
        var_x += dx * dx;
    }
    if var_x == 0.0 {
        return 0.0;
    }
    cov / var_x
}

/// The exponent gate, expressed as a pure `Result` over already-measured
/// `(n, cost)` samples so the same logic can be exercised by both the live
/// measurement (green) and the RED fixture (an injected quadratic dataset).
///
/// `samples` are `(n, cost)` pairs with `n > 0` and `cost > 0`. We fit the slope
/// of `log(cost)` vs `log(n)` and reject when it exceeds `budget`.
///
/// # Errors
/// Returns `Err` when fewer than two usable samples remain, or when the fitted
/// log-log slope exceeds `budget` (a super-linear regression).
pub fn check_complexity(samples: &[(u64, u64)], budget: f64) -> Result<f64, String> {
    let points: Vec<(f64, f64)> = samples
        .iter()
        .filter(|&&(n, cost)| n > 0 && cost > 0)
        .map(|&(n, cost)| ((n as f64).ln(), (cost as f64).ln()))
        .collect();
    if points.len() < 2 {
        return Err(format!(
            "complexity gate needs >= 2 usable (n>0, cost>0) samples, got {} from {samples:?}",
            points.len()
        ));
    }
    let slope = loglog_slope(&points);
    if slope > budget {
        return Err(format!(
            "complexity exponent regression: fitted log-log slope {slope:.4} exceeds budget \
             {budget:.4} over samples {samples:?}"
        ));
    }
    Ok(slope)
}

// ---------------------------------------------------------------------------
// Live measurement against a REAL Store operation (the green property).
// ---------------------------------------------------------------------------

/// Build a store holding `n` visible events under one coordinate, then return
/// the allocation count of a single full-region REPLAY READ over it: enumerate
/// every visible entry with `Store::query(Region::all())` and decode each one's
/// payload with `Store::get`.
///
/// We deliberately decode the payloads (not just enumerate the index entries):
/// `query` alone returns `IndexEntry`s whose `Coordinate` is `Arc<str>`-backed,
/// so enumeration only reallocates the result `Vec` (a LOGARITHMIC count) and
/// would NOT witness the O(n) scan/decode work a real replay performs. Decoding
/// each event into an owned `serde_json::Value` allocates once per event, so the
/// measured cost is genuinely LINEAR in n — the metric this gate must bound is
/// the cost of the real read path an audit/replay drives, not a Vec-growth
/// artifact. The before/after snapshot isolates the read from store
/// construction and append cost.
fn replay_read_alloc_cost(n: u64) -> u64 {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let coord = Coordinate::new("entity:cplx", "scope:exp").expect("valid coordinate");
    for i in 0..n {
        store
            .append(&coord, KIND, &serde_json::json!({ "i": i }))
            .expect("append");
    }
    let region = Region::all();
    let (read, delta) = CountingAlloc::scope(|| {
        let entries = store.query(&region);
        let mut decoded = 0u64;
        for entry in &entries {
            let stored = store
                .get(entry.event_id())
                .expect("read back appended event");
            // Touch the decoded payload so the decode cannot be optimized away.
            if stored.event.payload.is_object() {
                decoded += 1;
            }
        }
        (entries.len() as u64, decoded)
    });
    let (entry_count, decoded) = read;
    // `Region::all()` returns every visible entry; the store may carry a small
    // fixed number of bookkeeping entries on top of our n appends, so we assert
    // the result grows with n (>= n) rather than an exact count. The log-log
    // slope is asymptotically invariant to that additive constant.
    assert!(
        entry_count >= n && decoded >= n,
        "replay must enumerate and decode at least the {n} appended events \
         (enumerated {entry_count}, decoded {decoded})"
    );
    delta.allocs.saturating_add(delta.reallocs)
}

/// GREEN: a real full-region replay read (`query(Region::all())` + per-event
/// `get`) scales no worse than ~linear in the number of visible entries, AND its
/// worst-case per-op allocation count over the largest input stays under the
/// WCET budget.
///
/// justifies: INV-COMPLEXITY-EXPONENT-BOUNDED — measured, hardware-independent,
/// deterministic (allocation COUNTS, not wall-clock).
#[test]
fn query_complexity_exponent_and_wcet_are_bounded() {
    let samples: Vec<(u64, u64)> = SIZES
        .iter()
        .map(|&n| (n, replay_read_alloc_cost(n)))
        .collect();

    // EXPONENT half: the fitted log-log slope must stay under the linear budget.
    let slope = check_complexity(&samples, SLOPE_BUDGET)
        .expect("PROPERTY: query complexity exponent must be bounded");
    assert!(
        slope <= SLOPE_BUDGET,
        "PROPERTY: query log-log slope {slope:.4} must be <= budget {SLOPE_BUDGET:.4} \
         (samples {samples:?})"
    );

    // WCET / p100 half: over the SAME declared distribution, the worst observed
    // per-op allocation COUNT (here the largest n's cost, the worst case of a
    // monotone metric) must stay under a fixed budget. Counts, not nanoseconds.
    let worst = samples
        .iter()
        .map(|&(_, cost)| cost)
        .max()
        .expect("at least one sample");
    assert!(
        worst <= WCET_ALLOC_BUDGET,
        "PROPERTY (WCET/p100): worst per-op query allocation count {worst} must be <= budget \
         {WCET_ALLOC_BUDGET} over distribution {SIZES:?}"
    );
}

// ---------------------------------------------------------------------------
// RED fixture (GateNegativePath): the gate REJECTS a super-linear dataset.
// ---------------------------------------------------------------------------

/// RED: feed the SAME gate logic a deliberately QUADRATIC cost dataset
/// (`cost = n * n`) and assert it is REJECTED. Anti-vacuity: this proves
/// `check_complexity` actually bites a super-linear regression rather than
/// passing everything. The live `query_complexity_exponent_and_wcet_are_bounded`
/// test is the green half; this is the failing-path proof.
#[test]
fn super_linear_dataset_is_rejected() {
    // Quadratic: cost grows as n^2, so the true log-log slope is ~2.0, well over
    // a linear budget.
    let quadratic: Vec<(u64, u64)> = SIZES.iter().map(|&n| (n, n.saturating_mul(n))).collect();
    let linear_budget = 1.35_f64;

    let rejected = check_complexity(&quadratic, linear_budget);
    assert!(
        rejected.is_err(),
        "the complexity gate MUST reject a quadratic dataset {quadratic:?} under a linear budget \
         {linear_budget}, got {rejected:?}"
    );
    if let Err(why) = &rejected {
        assert!(
            why.contains("regression"),
            "rejection must explain the exponent regression, got: {why}"
        );
    }

    // Anti-tautology cross-check: the SAME gate ACCEPTS a genuinely linear
    // dataset (`cost = c * n`) under the same budget, so it is not always-Err.
    let linear: Vec<(u64, u64)> = SIZES.iter().map(|&n| (n, n.saturating_mul(7))).collect();
    let accepted = check_complexity(&linear, linear_budget);
    assert!(
        accepted.is_ok(),
        "the gate must ACCEPT a linear dataset {linear:?} under budget {linear_budget}, got \
         {accepted:?}"
    );
}

/// Pure-logic unit test for [`loglog_slope`] in isolation (no Store, no
/// allocator): a perfect quadratic `y = x^2` in log-log space has slope exactly
/// 2, and a perfect linear `y = c*x` has slope exactly 1. This pins the fit
/// math independent of any measurement.
#[test]
fn loglog_slope_recovers_known_exponents() {
    // log(n^2) = 2*log(n): slope 2.
    let quad: Vec<(f64, f64)> = SIZES
        .iter()
        .map(|&n| {
            let x = (n as f64).ln();
            (x, (n.saturating_mul(n) as f64).ln())
        })
        .collect();
    let s2 = loglog_slope(&quad);
    assert!(
        (s2 - 2.0).abs() < 1e-9,
        "quadratic slope must be ~2, got {s2}"
    );

    // log(5*n) = log(5) + log(n): slope 1 (the additive constant shifts the
    // intercept, not the slope).
    let lin: Vec<(f64, f64)> = SIZES
        .iter()
        .map(|&n| {
            let x = (n as f64).ln();
            (x, (n.saturating_mul(5) as f64).ln())
        })
        .collect();
    let s1 = loglog_slope(&lin);
    assert!((s1 - 1.0).abs() < 1e-9, "linear slope must be ~1, got {s1}");

    // Degenerate guards: < 2 points and zero-variance x both return 0.0.
    assert_eq!(loglog_slope(&[]), 0.0);
    assert_eq!(loglog_slope(&[(1.0, 5.0)]), 0.0);
    assert_eq!(loglog_slope(&[(3.0, 1.0), (3.0, 9.0)]), 0.0);
}
