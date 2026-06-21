//! HLC join-semilattice laws as properties.
//!
//! PROVES: LAW — CRDT join-semilattice (Shapiro et al.): the HLC merge is a
//! least-upper-bound on the `(wall_ms, global_sequence)`-ordered lattice and is
//! therefore commutative, associative, and idempotent, with `ORIGIN` as bottom.
//! CATCHES: a merge mutant that picks `self` unconditionally on the equal
//! coordinate branch, or breaks commutativity/associativity/idempotency, passes
//! a fixed example but fails this generated law (e.g. equal-wall_ms tiebreak).
//! SEEDED: bounded proptest (64 cases) with proptest-regressions persistence;
//! `arb_hlc` oversamples ORIGIN and shared-coordinate pairs for the tiebreak.
//! INVARIANTS: INV-HLC-JOIN-SEMILATTICE (HlcPoint merge forms a bounded
//! join-semilattice; the dual meet forms the bounded meet-semilattice).
//!
//! The merge under test is the `Ord`-derived join (`a.max(b)`) defined by
//! `HlcPoint`'s lexicographic `Ord` (`wall_ms` then `global_sequence`,
//! `src/store/stats.rs`). The meet is `a.min(b)`. The tiebreak matters: at equal
//! `wall_ms` the order falls through to `global_sequence`, so the generator MUST
//! produce equal-`wall_ms`/distinct-`global_sequence` pairs (and vice versa) to
//! kill a mutant that picks `self` unconditionally on the equal branch.

use proptest::prelude::*;
use support::prelude::*;

#[path = "common/proptest.rs"]
mod proptest_support;
mod support;

/// Generates an `HlcPoint`, deliberately oversampling values that share one
/// coordinate so the tiebreak path is exercised: `ORIGIN` (the bottom), fully
/// random points, equal-`wall_ms` pairs, and equal-`global_sequence` pairs.
fn arb_hlc() -> impl Strategy<Value = HlcPoint> {
    prop_oneof![
        1 => Just(HlcPoint::ORIGIN),
        4 => (any::<u64>(), any::<u64>())
            .prop_map(|(wall_ms, global_sequence)| HlcPoint { wall_ms, global_sequence }),
        // Equal wall_ms, distinct sequence — forces the global_sequence tiebreak.
        2 => (0u64..8, any::<u64>())
            .prop_map(|(wall_ms, global_sequence)| HlcPoint { wall_ms, global_sequence }),
        // Equal global_sequence, distinct wall_ms — forces the wall_ms primary.
        2 => (any::<u64>(), 0u64..8)
            .prop_map(|(wall_ms, global_sequence)| HlcPoint { wall_ms, global_sequence }),
    ]
}

/// The join (least upper bound) on the HLC lattice.
fn join(a: HlcPoint, b: HlcPoint) -> HlcPoint {
    a.max(b)
}

/// The meet (greatest lower bound) — the dual semilattice.
fn meet(a: HlcPoint, b: HlcPoint) -> HlcPoint {
    a.min(b)
}

proptest! {
    #![proptest_config(proptest_support::cfg(64))]

    // --- JOIN (LUB) semilattice laws ---

    #[test]
    fn join_commutative(a in arb_hlc(), b in arb_hlc()) {
        prop_assert_eq!(join(a, b), join(b, a),
            "JOIN COMMUTATIVITY VIOLATED: a⊔b != b⊔a for {:?}, {:?}", a, b);
    }

    #[test]
    fn join_associative(a in arb_hlc(), b in arb_hlc(), c in arb_hlc()) {
        prop_assert_eq!(join(join(a, b), c), join(a, join(b, c)),
            "JOIN ASSOCIATIVITY VIOLATED for {:?}, {:?}, {:?}", a, b, c);
    }

    #[test]
    fn join_idempotent(a in arb_hlc()) {
        prop_assert_eq!(join(a, a), a,
            "JOIN IDEMPOTENCY VIOLATED: a⊔a != a for {:?}", a);
    }

    #[test]
    fn join_origin_is_bottom(a in arb_hlc()) {
        prop_assert_eq!(join(a, HlcPoint::ORIGIN), a,
            "JOIN IDENTITY VIOLATED: a⊔ORIGIN != a for {:?}", a);
    }

    #[test]
    fn join_is_extensive(a in arb_hlc(), b in arb_hlc()) {
        // The LUB covers (is >=) both inputs.
        let j = join(a, b);
        prop_assert!(j >= a && j >= b,
            "JOIN MONOTONICITY VIOLATED: {:?} does not cover {:?} and {:?}", j, a, b);
    }

    // --- MEET (GLB) dual semilattice laws ---

    #[test]
    fn meet_commutative(a in arb_hlc(), b in arb_hlc()) {
        prop_assert_eq!(meet(a, b), meet(b, a),
            "MEET COMMUTATIVITY VIOLATED for {:?}, {:?}", a, b);
    }

    #[test]
    fn meet_associative(a in arb_hlc(), b in arb_hlc(), c in arb_hlc()) {
        prop_assert_eq!(meet(meet(a, b), c), meet(a, meet(b, c)),
            "MEET ASSOCIATIVITY VIOLATED for {:?}, {:?}, {:?}", a, b, c);
    }

    #[test]
    fn meet_idempotent(a in arb_hlc()) {
        prop_assert_eq!(meet(a, a), a,
            "MEET IDEMPOTENCY VIOLATED for {:?}", a);
    }

    #[test]
    fn meet_origin_is_absorbing(a in arb_hlc()) {
        // ORIGIN is the bottom, so it is the absorbing element of meet.
        prop_assert_eq!(meet(a, HlcPoint::ORIGIN), HlcPoint::ORIGIN,
            "MEET ABSORPTION VIOLATED: a⊓ORIGIN != ORIGIN for {:?}", a);
    }

    // --- ABSORPTION (ties the two operations into a lattice) ---

    #[test]
    fn absorption_join_over_meet(a in arb_hlc(), b in arb_hlc()) {
        prop_assert_eq!(join(a, meet(a, b)), a,
            "ABSORPTION VIOLATED: a⊔(a⊓b) != a for {:?}, {:?}", a, b);
    }

    #[test]
    fn absorption_meet_over_join(a in arb_hlc(), b in arb_hlc()) {
        prop_assert_eq!(meet(a, join(a, b)), a,
            "ABSORPTION VIOLATED: a⊓(a⊔b) != a for {:?}, {:?}", a, b);
    }

    // --- TIEBREAK COVERAGE (kills the equal-branch "pick self" mutant) ---

    #[test]
    fn join_respects_wall_ms_tiebreak(
        wall_ms in any::<u64>(),
        s1 in any::<u64>(),
        s2 in any::<u64>(),
    ) {
        // Equal wall_ms: the join must pick the larger global_sequence, NOT `self`.
        prop_assume!(s1 != s2);
        let a = HlcPoint { wall_ms, global_sequence: s1 };
        let b = HlcPoint { wall_ms, global_sequence: s2 };
        let expected = if s1 > s2 { a } else { b };
        prop_assert_eq!(join(a, b), expected,
            "JOIN TIEBREAK VIOLATED at equal wall_ms: did not pick larger sequence");
        prop_assert_eq!(join(a, b), join(b, a),
            "JOIN TIEBREAK is not commutative at equal wall_ms");
    }
}
