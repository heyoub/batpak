#![allow(clippy::wildcard_enum_match_arm)] // proptest assertions use wildcard match
//! Proptest verification of Outcome<T> monad laws.
//! Left identity, right identity, associativity, and Batch distribution.
//! [SPEC:tests/monad_laws.rs]
//!
//! PROVES: LAW-006 (Algebraic Integrity — monad laws hold for Outcome)
//! DEFENDS: FM-009 (Polite Downgrade — combinators must not silently drop errors)
//! INVARIANTS: INV-TYPE (type-level guarantees on combinator composition)
//!
//! Anti-almost-correctness: This test would have caught the missing T: Clone
//! bound on join_all (Phase 1.4) — proptest generates Batch(vec![Ok(x)]) cases
//! that exercise the .map() path requiring Clone.

use batpak::prelude::*;
use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;

/// Project-wide proptest config: env-driven cases + persistent failure seeds.
fn proptest_cfg() -> ProptestConfig {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(256);
    ProptestConfig {
        cases,
        failure_persistence: Some(Box::new(FileFailurePersistence::SourceParallel(
            "proptest-regressions",
        ))),
        ..ProptestConfig::default()
    }
}

// --- Arbitrary Outcome generator ---

fn arb_outcome() -> impl Strategy<Value = Outcome<i32>> {
    prop_oneof![
        any::<i32>().prop_map(Outcome::Ok),
        any::<String>().prop_map(|msg| Outcome::Err(OutcomeError {
            kind: ErrorKind::Internal,
            message: msg,
            compensation: None,
            retryable: false,
        })),
        (any::<u64>(), any::<u32>(), any::<u32>(), any::<String>()).prop_map(
            |(after, attempt, max, reason)| Outcome::Retry {
                after_ms: after,
                attempt,
                max_attempts: max,
                reason,
            }
        ),
        any::<String>().prop_map(|reason| Outcome::Cancelled { reason }),
        // Batch of Ok values (exercises Clone path in combinators)
        proptest::collection::vec(any::<i32>(), 0..5)
            .prop_map(|vs| Outcome::Batch(vs.into_iter().map(Outcome::Ok).collect())),
    ]
}

// --- Monad law helpers ---

// A pure function: i32 -> Outcome<i32>
fn f_double(x: i32) -> Outcome<i32> {
    Outcome::Ok(x.wrapping_mul(2))
}

fn f_add_one(x: i32) -> Outcome<i32> {
    Outcome::Ok(x.wrapping_add(1))
}

// --- LEFT IDENTITY: return a >>= f  ≡  f a ---
proptest! {
    #![proptest_config(proptest_cfg())]

    #[test]
    fn left_identity(a in any::<i32>()) {
        // Outcome::Ok(a).and_then(f) == f(a)
        let lhs = Outcome::Ok(a).and_then(f_double);
        let rhs = f_double(a);
        prop_assert_eq!(lhs, rhs, "LEFT IDENTITY VIOLATED: \
            Outcome::Ok({}).and_then(f) != f({}). \
            Investigate: src/outcome/mod.rs and_then implementation.", a, a);
    }

    // --- RIGHT IDENTITY: m >>= return  ≡  m ---
    #[test]
    fn right_identity(m in arb_outcome()) {
        let original = m.clone();
        let lhs = m.and_then(Outcome::Ok);
        prop_assert_eq!(lhs, original, "RIGHT IDENTITY VIOLATED: \
            m.and_then(Outcome::Ok) != m. \
            Investigate: src/outcome/mod.rs and_then for non-Ok variants.");
    }

    // --- ASSOCIATIVITY: (m >>= f) >>= g  ≡  m >>= (λx → f x >>= g) ---
    #[test]
    fn associativity(m in arb_outcome()) {
        let lhs = m.clone().and_then(f_double).and_then(f_add_one);
        let rhs = m.and_then(|x| f_double(x).and_then(f_add_one));
        prop_assert_eq!(lhs, rhs, "ASSOCIATIVITY VIOLATED: \
            (m >>= f) >>= g != m >>= (x -> f x >>= g). \
            Investigate: src/outcome/mod.rs Batch distribution in and_then.");
    }

    // --- MAP PRESERVES STRUCTURE ---
    #[test]
    fn map_preserves_structure(m in arb_outcome()) {
        // map id == id (functor identity law)
        let original = m.clone();
        let mapped = m.map(|x| x);
        prop_assert_eq!(mapped, original, "FUNCTOR IDENTITY VIOLATED: \
            m.map(|x| x) != m. \
            Investigate: src/outcome/mod.rs map implementation.");
    }

    // --- ZIP COMMUTATIVITY for Ok ---
    #[test]
    fn zip_both_ok(a in any::<i32>(), b in any::<i32>()) {
        let result = batpak::outcome::zip(Outcome::Ok(a), Outcome::Ok(b));
        prop_assert_eq!(result, Outcome::Ok((a, b)),
            "ZIP COMMUTATIVITY: zip(Ok({}), Ok({})) should produce Ok(({}, {})).\n\
             Investigate: src/outcome/combine.rs zip.\n\
             Common causes: zip not pairing Ok values, returning wrong variant.\n\
             Run: cargo test --test monad_laws zip_both_ok",
            a, b, a, b);
    }

    // --- JOIN_ALL: all Ok gives Ok(Vec) ---
    #[test]
    fn join_all_all_ok(values in proptest::collection::vec(any::<i32>(), 0..10)) {
        let outcomes: Vec<Outcome<i32>> = values.iter().map(|&v| Outcome::Ok(v)).collect();
        let result = batpak::outcome::join_all(outcomes);
        prop_assert_eq!(result, Outcome::Ok(values),
            "JOIN_ALL VIOLATED: all Ok inputs should produce Ok(vec). \
             Investigate: src/outcome/combine.rs join_all.");
    }

    // --- JOIN_ALL: first Err short-circuits ---
    #[test]
    fn join_all_first_err_wins(values in proptest::collection::vec(any::<i32>(), 1..5)) {
        let err = OutcomeError {
            kind: ErrorKind::Internal,
            message: "test error".into(),
            compensation: None,
            retryable: false,
        };
        let mut outcomes: Vec<Outcome<i32>> = values.iter().map(|&v| Outcome::Ok(v)).collect();
        outcomes.push(Outcome::Err(err.clone()));
        outcomes.push(Outcome::Ok(42)); // should never be reached
        let result = batpak::outcome::join_all(outcomes);
        match result {
            Outcome::Err(e) => prop_assert_eq!(e.message, "test error",
                "JOIN_ALL SHORT-CIRCUIT: join_all should propagate the Err message unchanged.\n\
                 Investigate: src/outcome/combine.rs join_all.\n\
                 Common causes: error message overwritten or not forwarded from Err variant.\n\
                 Run: cargo test --test monad_laws join_all_first_err_wins"),
            other => prop_assert!(false, "Expected Err, got {:?}", other),
        }
    }
}
