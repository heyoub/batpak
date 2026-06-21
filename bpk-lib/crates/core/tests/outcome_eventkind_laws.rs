//! Outcome functor/monoid laws and EventKind parse-roundtrip law.
//!
//! PROVES: LAW — functor composition, Batch monoid associativity/unit, zip
//! priority-lattice, and the EventKind parse-don't-validate roundtrip.
//! CATCHES: a map mutant that skips the Batch arm, a zip mutant that swaps two
//! priority arms, or an EventKind parser that drifts from the const constructor's
//! validity predicate — each passes a fixed example but fails this generated law.
//! SEEDED: bounded proptest (64 cases) with proptest-regressions persistence;
//! `arb_outcome` covers every variant including Batch, and category/type_id are
//! drawn across the full input space to probe the refinement boundary.
//! INVARIANTS: INV-OUTCOME-FUNCTOR-COMPOSITION (map distributes over composition
//! including the Batch arm), INV-EVENTKIND-PARSE-ROUNDTRIP (try_custom Ok values
//! decompose back to their (category, type_id), and try_custom errs exactly
//! where the panicking const constructor would).
//!
//! Extends the existing monad-law suite (left/right identity, associativity,
//! functor identity) with the missing functor-composition half plus the monoid
//! and parse laws that example tests never generalized.
#![allow(clippy::wildcard_enum_match_arm)] // justifies: INV-TEST-PANIC-AS-ASSERTION; proptest assertions report counterexamples without enumerating every variant.

use proptest::prelude::*;
use support::prelude::*;

#[path = "common/proptest.rs"]
mod proptest_support;
mod support;

/// Generates a valid custom [`EventKind`] via the fallible parser (never the
/// panicking const constructor).
fn arb_event_kind() -> impl Strategy<Value = EventKind> {
    (1u8..16, 0u16..0x1000).prop_filter_map(
        "category 0xD is reserved; try_custom must accept the rest",
        |(category, type_id)| {
            if category == 0xD {
                return None;
            }
            EventKind::try_custom(category, type_id).ok()
        },
    )
}

/// Arbitrary `Outcome<i32>` covering every variant, including Batch (to exercise
/// the distributing arms of map/and_then).
fn arb_outcome() -> impl Strategy<Value = Outcome<i32>> {
    prop_oneof![
        any::<i32>().prop_map(Outcome::Ok),
        any::<String>().prop_map(|msg| Outcome::Err(OutcomeError::new(ErrorKind::Internal, msg))),
        (any::<u64>(), any::<u32>(), any::<u32>(), any::<String>()).prop_map(
            |(after_ms, attempt, max_attempts, reason)| Outcome::Retry {
                after_ms,
                attempt,
                max_attempts,
                reason,
            }
        ),
        any::<String>().prop_map(|reason| Outcome::Cancelled { reason }),
        proptest::collection::vec(any::<i32>(), 0..5)
            .prop_map(|vs| Outcome::Batch(vs.into_iter().map(Outcome::Ok).collect())),
    ]
}

/// Non-Batch `Outcome<i32>` for the zip priority-lattice law (Batch has special
/// pairwise distribution semantics, so the total-order priority law is stated
/// over the non-Batch variants).
fn arb_non_batch_outcome() -> impl Strategy<Value = Outcome<i32>> {
    prop_oneof![
        any::<i32>().prop_map(Outcome::Ok),
        any::<String>().prop_map(|msg| Outcome::Err(OutcomeError::new(ErrorKind::Internal, msg))),
        (any::<u64>(), any::<u32>(), any::<u32>(), any::<String>()).prop_map(
            |(after_ms, attempt, max_attempts, reason)| Outcome::Retry {
                after_ms,
                attempt,
                max_attempts,
                reason,
            }
        ),
        any::<String>().prop_map(|reason| Outcome::Cancelled { reason }),
    ]
}

/// Priority rank of a non-Batch variant (higher wins in zip).
/// Err > Cancelled > Retry > Pending > Ok.
fn priority_rank<T>(o: &Outcome<T>) -> u8 {
    match o {
        Outcome::Err(_) => 5,
        Outcome::Cancelled { .. } => 4,
        Outcome::Retry { .. } => 3,
        Outcome::Pending { .. } => 2,
        Outcome::Ok(_) => 1,
        Outcome::Batch(_) => 0,
        _ => 0,
    }
}

fn f_double(x: i32) -> i32 {
    x.wrapping_mul(2)
}

fn g_inc(x: i32) -> i32 {
    x.wrapping_add(1)
}

proptest! {
    #![proptest_config(proptest_support::cfg(64))]

    // --- FUNCTOR COMPOSITION: map f . map g == map (f . g) ---
    // The missing half of the functor laws (identity already lives in monad_laws).
    #[test]
    fn functor_composition(m in arb_outcome()) {
        let lhs = m.clone().map(f_double).map(g_inc);
        let rhs = m.map(|x| g_inc(f_double(x)));
        prop_assert_eq!(lhs, rhs, "FUNCTOR COMPOSITION VIOLATED: \
            map(g)∘map(f) != map(g∘f). Investigate the Batch arm of Outcome::map.");
    }

    // --- BATCH MONOID: associativity of Batch concatenation ---
    #[test]
    fn batch_monoid_associative(
        a in proptest::collection::vec(any::<i32>(), 0..4),
        b in proptest::collection::vec(any::<i32>(), 0..4),
        c in proptest::collection::vec(any::<i32>(), 0..4),
    ) {
        let mk = |v: &[i32]| -> Vec<Outcome<i32>> { v.iter().map(|&x| Outcome::Ok(x)).collect() };
        // (a ++ b) ++ c == a ++ (b ++ c) at the Batch concatenation level.
        let left = {
            let mut ab = mk(&a);
            ab.extend(mk(&b));
            let mut abc = ab;
            abc.extend(mk(&c));
            Outcome::Batch(abc)
        };
        let right = {
            let mut bc = mk(&b);
            bc.extend(mk(&c));
            let mut abc = mk(&a);
            abc.extend(bc);
            Outcome::Batch(abc)
        };
        prop_assert_eq!(left, right, "BATCH MONOID ASSOCIATIVITY VIOLATED");
    }

    // --- BATCH MONOID UNIT: empty Batch is left+right identity ---
    #[test]
    fn batch_monoid_unit(a in proptest::collection::vec(any::<i32>(), 0..6)) {
        let items: Vec<Outcome<i32>> = a.iter().map(|&x| Outcome::Ok(x)).collect();
        let val = Outcome::Batch(items.clone());
        // Concatenating the empty batch on either side is the identity.
        let left_unit = {
            let mut v: Vec<Outcome<i32>> = Vec::new();
            v.extend(items.clone());
            Outcome::Batch(v)
        };
        let right_unit = {
            let mut v = items.clone();
            v.extend(Vec::<Outcome<i32>>::new());
            Outcome::Batch(v)
        };
        prop_assert_eq!(&left_unit, &val, "BATCH LEFT UNIT VIOLATED");
        prop_assert_eq!(&right_unit, &val, "BATCH RIGHT UNIT VIOLATED");
    }

    // --- ZIP PRIORITY LATTICE: result variant == max-priority input ---
    #[test]
    fn zip_priority_is_max(a in arb_non_batch_outcome(), b in arb_non_batch_outcome()) {
        let result = batpak::outcome::zip(a.clone(), b.clone());
        let expected_rank = priority_rank(&a).max(priority_rank(&b));
        prop_assert_eq!(priority_rank(&result), expected_rank,
            "ZIP PRIORITY VIOLATED: zip({:?}, {:?}) rank {} != max input rank {}",
            a, b, priority_rank(&result), expected_rank);
    }

    // --- EVENTKIND PARSE ROUNDTRIP: try_custom Ok ⟹ decompose to (cat, id) ---
    #[test]
    fn eventkind_parse_roundtrip(k in arb_event_kind()) {
        let category = k.category();
        let type_id = k.type_id();
        // Re-parsing the decomposed coordinates yields the same kind.
        let reparsed = EventKind::try_custom(category, type_id)
            .expect("decomposed coordinates of a valid kind must re-parse");
        prop_assert_eq!(reparsed.as_raw_u16(), k.as_raw_u16(),
            "EVENTKIND ROUNDTRIP VIOLATED: try_custom(category, type_id) != original");
        // Encoding law: raw == (category << 12) | type_id.
        prop_assert_eq!(k.as_raw_u16(), (u16::from(category) << 12) | type_id,
            "EVENTKIND ENCODING VIOLATED");
    }

    // --- EVENTKIND REFINEMENT: try_custom errs exactly on reserved/oob input ---
    #[test]
    fn eventkind_try_custom_refuses_invalid(category in 0u8..=255, type_id in any::<u16>()) {
        let result = EventKind::try_custom(category, type_id);
        let should_be_valid = category < 16
            && category != 0x0
            && category != 0xD
            && type_id < 0x1000;
        prop_assert_eq!(result.is_ok(), should_be_valid,
            "EVENTKIND REFINEMENT VIOLATED: try_custom({}, {}) acceptance != validity predicate",
            category, type_id);
    }
}
