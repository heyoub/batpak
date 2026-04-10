#![allow(clippy::panic, clippy::wildcard_enum_match_arm, clippy::unwrap_used)] // test assertions use panic; exhaustive match not needed in tests
//! Tests for Outcome combinators not covered by monad_laws.rs.
//! Covers: inspect, inspect_err, map_err, or_else, and_then_if,
//! into_result, unwrap_or, unwrap_or_else, join_any, zip edge cases.
//! [SPEC:tests/outcome_combinators.rs]
//!
//! PROVES: LAW-006 (Algebraic Integrity — combinator contracts)
//! DEFENDS: FM-009 (Polite Downgrade — combinators preserve error semantics)
//! INVARIANTS: INV-TYPE (combinator type safety), INV-STATE (WaitCondition semantics)

use batpak::prelude::*;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

fn test_err() -> OutcomeError {
    OutcomeError {
        kind: ErrorKind::Internal,
        message: "test error".into(),
        compensation: None,
        retryable: false,
    }
}

// --- inspect ---
// NOTE: inspect/inspect_err require F: Clone (for Batch distribution).
// Use Arc<Atomic> to make closures Clone-compatible.

#[test]
fn inspect_calls_closure_on_ok() {
    let counter = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&counter);
    let result = Outcome::Ok(42).inspect(move |v| {
        assert_eq!(
            *v, 42,
            "INSPECT CLOSURE VALUE WRONG: expected 42, got {}.\n\
             Investigate: src/outcome/mod.rs inspect Ok branch.\n\
             Common causes: inspect modifying the value instead of just observing it.\n\
             Run: cargo test --test outcome_combinators",
            v
        );
        c.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "INSPECT CALL COUNT WRONG: expected closure to be called exactly once for Ok(42), got {}.\n\
         Investigate: src/outcome/mod.rs inspect Ok branch.\n\
         Common causes: inspect skips Ok variant, or calls closure multiple times.\n\
         Run: cargo test --test outcome_combinators",
        counter.load(Ordering::SeqCst)
    );
    assert_eq!(
        result,
        Outcome::Ok(42),
        "INSPECT MODIFIED VALUE: inspect on Ok(42) must return Ok(42) unchanged.\n\
         Investigate: src/outcome/mod.rs inspect Ok branch return value.\n\
         Common causes: inspect transforms value instead of only observing it.\n\
         Run: cargo test --test outcome_combinators"
    );
}

#[test]
fn inspect_skips_non_ok() {
    let err: Outcome<i32> = Outcome::Err(test_err());
    let result = err.inspect(|_| panic!("inspect should not call closure on Err"));
    assert!(
        result.is_err(),
        "INSPECT NON-OK VARIANT CHANGED: inspect on Err must return Err unchanged.\n\
         Investigate: src/outcome/mod.rs inspect non-Ok arm.\n\
         Common causes: inspect arm falls through to Ok branch or mutates variant.\n\
         Run: cargo test --test outcome_combinators"
    );

    let retry: Outcome<i32> = Outcome::retry(100, 1, 3, "backoff");
    let result = retry.inspect(|_| panic!("inspect should not call closure on Retry"));
    assert!(
        result.is_retry(),
        "INSPECT NON-OK VARIANT CHANGED: inspect on Retry must return Retry unchanged.\n\
         Investigate: src/outcome/mod.rs inspect non-Ok arm.\n\
         Common causes: inspect arm converts Retry to another variant.\n\
         Run: cargo test --test outcome_combinators"
    );

    let cancelled: Outcome<i32> = Outcome::cancelled("nope");
    let result = cancelled.inspect(|_| panic!("inspect should not call closure on Cancelled"));
    assert!(
        result.is_cancelled(),
        "INSPECT NON-OK VARIANT CHANGED: inspect on Cancelled must return Cancelled unchanged.\n\
         Investigate: src/outcome/mod.rs inspect non-Ok arm.\n\
         Common causes: inspect arm converts Cancelled to another variant.\n\
         Run: cargo test --test outcome_combinators"
    );
}

#[test]
fn inspect_distributes_over_batch() {
    let counter = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&counter);
    let batch = Outcome::Batch(vec![
        Outcome::Ok(1),
        Outcome::Err(test_err()),
        Outcome::Ok(3),
    ]);
    let _ = batch.inspect(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(
        counter.load(Ordering::SeqCst),
        2,
        "INSPECT BATCH CALL COUNT WRONG: expected closure called 2 times (once per Ok in Batch), got {}.\n\
         Investigate: src/outcome/mod.rs inspect Batch distribution.\n\
         Common causes: inspect does not recurse into Batch items, or counts Err items too.\n\
         Run: cargo test --test outcome_combinators",
        counter.load(Ordering::SeqCst)
    );
}

// --- inspect_err ---

#[test]
fn inspect_err_calls_closure_on_err() {
    let counter = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&counter);
    let result: Outcome<i32> = Outcome::Err(test_err()).inspect_err(move |e| {
        assert_eq!(
            e.kind,
            ErrorKind::Internal,
            "INSPECT_ERR CLOSURE KIND WRONG: expected ErrorKind::Internal, got {:?}.\n\
             Investigate: src/outcome/mod.rs inspect_err Err branch.\n\
             Common causes: error kind mutated before closure is called.\n\
             Run: cargo test --test outcome_combinators",
            e.kind
        );
        c.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "INSPECT_ERR CALL COUNT WRONG: expected closure called exactly once for Err, got {}.\n\
         Investigate: src/outcome/mod.rs inspect_err Err branch.\n\
         Common causes: inspect_err skips Err variant, or calls closure multiple times.\n\
         Run: cargo test --test outcome_combinators",
        counter.load(Ordering::SeqCst)
    );
    assert!(
        result.is_err(),
        "INSPECT_ERR CHANGED VARIANT: inspect_err on Err must return Err unchanged.\n\
         Investigate: src/outcome/mod.rs inspect_err Err arm.\n\
         Common causes: inspect_err arm replaces the Err variant with something else.\n\
         Run: cargo test --test outcome_combinators"
    );
}

#[test]
fn inspect_err_skips_ok() {
    let result = Outcome::Ok(42).inspect_err(|_| panic!("should not call on Ok"));
    assert_eq!(
        result,
        Outcome::Ok(42),
        "INSPECT_ERR CHANGED OK: inspect_err on Ok(42) must return Ok(42) unchanged.\n\
         Investigate: src/outcome/mod.rs inspect_err Ok branch pass-through.\n\
         Common causes: inspect_err accidentally modifies the Ok value.\n\
         Run: cargo test --test outcome_combinators"
    );
}

#[test]
fn inspect_err_distributes_over_batch() {
    let counter = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&counter);
    let batch = Outcome::Batch(vec![
        Outcome::Ok(1),
        Outcome::Err(test_err()),
        Outcome::Ok(3),
    ]);
    let _ = batch.inspect_err(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "INSPECT_ERR BATCH CALL COUNT WRONG: expected closure called 1 time (once per Err in Batch), got {}.\n\
         Investigate: src/outcome/mod.rs inspect_err Batch distribution.\n\
         Common causes: inspect_err does not recurse into Batch items, or also calls for Ok items.\n\
         Run: cargo test --test outcome_combinators",
        counter.load(Ordering::SeqCst)
    );
}

// --- map_err ---

#[test]
fn map_err_transforms_error() {
    let result: Outcome<i32> = Outcome::Err(test_err()).map_err(|mut e| {
        e.message = "transformed".into();
        e
    });
    match result {
        Outcome::Err(e) => assert_eq!(
            e.message, "transformed",
            "MAP_ERR TRANSFORM NOT APPLIED: expected message \"transformed\", got {:?}.\n\
             Investigate: src/outcome/mod.rs map_err Err arm.\n\
             Common causes: closure not invoked, or return value discarded.\n\
             Run: cargo test --test outcome_combinators",
            e.message
        ),
        _ => panic!(
            "Expected Err after map_err on Err — variant changed unexpectedly.\n\
             Investigate: src/outcome/mod.rs map_err Err arm.\n\
             Common causes: map_err on Err returns wrong variant.\n\
             Run: cargo test --test outcome_combinators"
        ),
    }
}

#[test]
fn map_err_skips_ok() {
    let result = Outcome::Ok(42).map_err(|_| panic!("should not call on Ok"));
    assert_eq!(
        result,
        Outcome::Ok(42),
        "MAP_ERR CHANGED OK: map_err on Ok(42) must return Ok(42) unchanged.\n\
         Investigate: src/outcome/mod.rs map_err Ok branch pass-through.\n\
         Common causes: map_err Ok arm calls closure or alters value.\n\
         Run: cargo test --test outcome_combinators"
    );
}

#[test]
fn map_err_distributes_over_batch() {
    let batch: Outcome<i32> = Outcome::Batch(vec![Outcome::Ok(1), Outcome::Err(test_err())]);
    let result = batch.map_err(|mut e| {
        e.message = "mapped".into();
        e
    });
    match result {
        Outcome::Batch(items) => {
            assert_eq!(
                items[0],
                Outcome::Ok(1),
                "MAP_ERR BATCH ITEM 0 CHANGED: Ok(1) must pass through map_err unmodified.\n\
                 Investigate: src/outcome/mod.rs map_err Batch distribution.\n\
                 Common causes: map_err applies transformation to Ok items inside Batch.\n\
                 Run: cargo test --test outcome_combinators"
            );
            match &items[1] {
                Outcome::Err(e) => assert_eq!(
                    e.message, "mapped",
                    "MAP_ERR BATCH ERR MESSAGE WRONG: expected \"mapped\", got {:?}.\n\
                     Investigate: src/outcome/mod.rs map_err Err arm inside Batch.\n\
                     Common causes: closure not called, wrong item indexed.\n\
                     Run: cargo test --test outcome_combinators",
                    e.message
                ),
                _ => panic!("Expected Err in batch"),
            }
        }
        _ => panic!("Expected Batch"),
    }
}

// --- or_else ---

#[test]
fn or_else_recovers_from_err() {
    let result: Outcome<i32> = Outcome::Err(test_err()).or_else(|_| Outcome::Ok(99));
    assert_eq!(
        result,
        Outcome::Ok(99),
        "OR_ELSE RECOVERY FAILED: or_else on Err should produce Ok(99), got {:?}.\n\
         Investigate: src/outcome/mod.rs or_else Err arm.\n\
         Common causes: or_else ignores Err, or passes it through without calling closure.\n\
         Run: cargo test --test outcome_combinators",
        result
    );
}

#[test]
fn or_else_skips_ok() {
    let result = Outcome::Ok(42).or_else(|_| panic!("should not call on Ok"));
    assert_eq!(
        result,
        Outcome::Ok(42),
        "OR_ELSE CHANGED OK: or_else on Ok(42) must return Ok(42) unchanged.\n\
         Investigate: src/outcome/mod.rs or_else Ok pass-through.\n\
         Common causes: or_else applies closure to Ok, or changes Ok value.\n\
         Run: cargo test --test outcome_combinators"
    );
}

#[test]
fn or_else_skips_non_err_variants() {
    let retry: Outcome<i32> = Outcome::retry(100, 1, 3, "retry");
    let result = retry.or_else(|_| panic!("should not call on Retry"));
    assert!(
        result.is_retry(),
        "OR_ELSE CHANGED RETRY: or_else on Retry must return Retry unchanged.\n\
         Investigate: src/outcome/mod.rs or_else non-Err pass-through.\n\
         Common causes: or_else mistakenly treats Retry as Err and applies closure.\n\
         Run: cargo test --test outcome_combinators"
    );
}

#[test]
fn or_else_distributes_over_batch() {
    let batch: Outcome<i32> = Outcome::Batch(vec![Outcome::Err(test_err()), Outcome::Ok(1)]);
    let result = batch.or_else(|_| Outcome::Ok(0));
    match result {
        Outcome::Batch(items) => {
            assert_eq!(items[0], Outcome::Ok(0),
                "OR_ELSE BATCH ERR NOT RECOVERED: items[0] should be Ok(0) after recovery, got {:?}.\n\
                 Investigate: src/outcome/mod.rs or_else Batch distribution.\n\
                 Common causes: or_else does not recurse into Batch items.\n\
                 Run: cargo test --test outcome_combinators",
                items[0]);
            assert_eq!(items[1], Outcome::Ok(1),
                "OR_ELSE BATCH OK CHANGED: items[1] is Ok(1) and must pass through untouched, got {:?}.\n\
                 Investigate: src/outcome/mod.rs or_else Batch distribution.\n\
                 Common causes: or_else incorrectly applies closure to Ok items inside Batch.\n\
                 Run: cargo test --test outcome_combinators",
                items[1]);
        }
        _ => panic!(
            "OR_ELSE BATCH VARIANT WRONG: expected Batch result from or_else on Batch.\n\
             Investigate: src/outcome/mod.rs or_else Batch arm.\n\
             Common causes: or_else collapses Batch to a single variant instead of distributing.\n\
             Run: cargo test --test outcome_combinators"
        ),
    }
}

// --- and_then_if ---

#[test]
fn and_then_if_applies_when_predicate_true() {
    let result = Outcome::Ok(42).and_then_if(|v| *v > 10, |v| Outcome::Ok(v * 2));
    assert_eq!(
        result,
        Outcome::Ok(84),
        "AND_THEN_IF PREDICATE TRUE NOT APPLIED: expected Ok(84) when predicate holds, got {:?}.\n\
         Investigate: src/outcome/mod.rs and_then_if true branch.\n\
         Common causes: and_then_if ignores the closure when predicate returns true.\n\
         Run: cargo test --test outcome_combinators",
        result
    );
}

#[test]
fn and_then_if_skips_when_predicate_false() {
    let result = Outcome::Ok(5).and_then_if(
        |v| *v > 10,
        |_| panic!("should not call when predicate is false"),
    );
    assert_eq!(result, Outcome::Ok(5),
        "AND_THEN_IF PREDICATE FALSE CHANGED VALUE: expected Ok(5) when predicate is false, got {:?}.\n\
         Investigate: src/outcome/mod.rs and_then_if false branch pass-through.\n\
         Common causes: and_then_if applies closure even when predicate returns false.\n\
         Run: cargo test --test outcome_combinators",
        result);
}

#[test]
fn and_then_if_skips_non_ok() {
    let err: Outcome<i32> = Outcome::Err(test_err());
    let result = err.and_then_if(
        |_| panic!("should not check predicate on Err"),
        |_| panic!("should not apply on Err"),
    );
    assert!(
        result.is_err(),
        "AND_THEN_IF CHANGED ERR VARIANT: and_then_if on Err must return Err unchanged.\n\
         Investigate: src/outcome/mod.rs and_then_if non-Ok pass-through.\n\
         Common causes: and_then_if evaluates predicate or applies closure on Err variants.\n\
         Run: cargo test --test outcome_combinators"
    );
}

// --- into_result ---

#[test]
fn into_result_ok() {
    let r: Result<i32, OutcomeError> = Outcome::Ok(42).into_result();
    assert_eq!(r.expect("into_result on Ok(42) should be Ok"), 42,
        "INTO_RESULT OK VALUE WRONG: expected 42, got a different value.\n\
         Investigate: src/outcome/mod.rs into_result Ok arm.\n\
         Common causes: into_result wraps or transforms the Ok value instead of passing it through.\n\
         Run: cargo test --test outcome_combinators");
}

#[test]
fn into_result_err() {
    let r: Result<i32, OutcomeError> = Outcome::Err(test_err()).into_result();
    assert!(
        r.is_err(),
        "INTO_RESULT ERR NOT ERR: into_result on Outcome::Err must return Err, got Ok.\n\
         Investigate: src/outcome/mod.rs into_result Err arm.\n\
         Common causes: into_result maps Err to Ok, or returns wrong variant.\n\
         Run: cargo test --test outcome_combinators"
    );
}

#[test]
fn into_result_cancelled() {
    let r: Result<i32, OutcomeError> = Outcome::cancelled("nope").into_result();
    let err = r.expect_err("cancelled outcome should convert to error");
    assert!(err.message.contains("cancelled"),
        "INTO_RESULT CANCELLED MESSAGE WRONG: error message should contain \"cancelled\", got {:?}.\n\
         Investigate: src/outcome/mod.rs into_result Cancelled arm.\n\
         Common causes: into_result uses wrong message template for Cancelled variant.\n\
         Run: cargo test --test outcome_combinators",
        err.message);
}

#[test]
fn into_result_non_terminal() {
    let r: Result<i32, OutcomeError> = Outcome::retry(100, 1, 3, "wait").into_result();
    let err = r.expect_err("non-terminal outcome should convert to error");
    assert!(err.message.contains("not terminal"),
        "INTO_RESULT NON_TERMINAL MESSAGE WRONG: error message should contain \"not terminal\", got {:?}.\n\
         Investigate: src/outcome/mod.rs into_result Retry/Pending arm.\n\
         Common causes: into_result uses wrong message template for non-terminal variants.\n\
         Run: cargo test --test outcome_combinators",
        err.message);
}

// --- unwrap_or / unwrap_or_else ---

#[test]
fn unwrap_or_returns_value_for_ok() {
    assert_eq!(
        Outcome::Ok(42).unwrap_or(0),
        42,
        "UNWRAP_OR OK VALUE WRONG: unwrap_or on Ok(42) should return 42, not the default.\n\
         Investigate: src/outcome/mod.rs unwrap_or Ok arm.\n\
         Common causes: unwrap_or returns default even for Ok, or extracts wrong inner value.\n\
         Run: cargo test --test outcome_combinators"
    );
}

#[test]
fn unwrap_or_returns_default_for_non_ok() {
    let err: Outcome<i32> = Outcome::Err(test_err());
    assert_eq!(err.unwrap_or(99), 99,
        "UNWRAP_OR DEFAULT NOT RETURNED: unwrap_or on Err should return 99 (the default), not another value.\n\
         Investigate: src/outcome/mod.rs unwrap_or non-Ok arm.\n\
         Common causes: unwrap_or panics or returns wrong fallback for Err variant.\n\
         Run: cargo test --test outcome_combinators");
}

#[test]
fn unwrap_or_else_returns_value_for_ok() {
    assert_eq!(Outcome::Ok(42).unwrap_or_else(|| 0), 42,
        "UNWRAP_OR_ELSE OK VALUE WRONG: unwrap_or_else on Ok(42) should return 42, not the closure result.\n\
         Investigate: src/outcome/mod.rs unwrap_or_else Ok arm.\n\
         Common causes: unwrap_or_else calls closure even for Ok, or extracts wrong inner value.\n\
         Run: cargo test --test outcome_combinators");
}

#[test]
fn unwrap_or_else_calls_closure_for_non_ok() {
    let err: Outcome<i32> = Outcome::Err(test_err());
    assert_eq!(err.unwrap_or_else(|| 99), 99,
        "UNWRAP_OR_ELSE CLOSURE NOT CALLED: unwrap_or_else on Err should return closure result 99.\n\
         Investigate: src/outcome/mod.rs unwrap_or_else non-Ok arm.\n\
         Common causes: unwrap_or_else does not invoke closure for Err, or returns wrong value.\n\
         Run: cargo test --test outcome_combinators");
}

// --- predicates ---

#[test]
fn predicates_cover_all_variants() {
    let ok: Outcome<i32> = Outcome::Ok(1);
    assert!(
        ok.is_ok(),
        "PREDICATE is_ok FAILED: Ok(1).is_ok() must be true.\n\
         Investigate: src/outcome/mod.rs is_ok predicate.\n\
         Common causes: is_ok returns false for Ok variant or has wrong match arm.\n\
         Run: cargo test --test outcome_combinators"
    );
    assert!(
        ok.is_terminal(),
        "PREDICATE is_terminal FAILED: Ok(1).is_terminal() must be true.\n\
         Investigate: src/outcome/mod.rs is_terminal predicate.\n\
         Common causes: Ok not listed as terminal in is_terminal match.\n\
         Run: cargo test --test outcome_combinators"
    );
    assert!(
        !ok.is_err(),
        "PREDICATE is_err FALSE POSITIVE: Ok(1).is_err() must be false.\n\
         Investigate: src/outcome/mod.rs is_err predicate.\n\
         Common causes: is_err matches Ok variant by mistake.\n\
         Run: cargo test --test outcome_combinators"
    );
    assert!(
        !ok.is_retry(),
        "PREDICATE is_retry FALSE POSITIVE: Ok(1).is_retry() must be false.\n\
         Investigate: src/outcome/mod.rs is_retry predicate.\n\
         Common causes: is_retry matches Ok variant by mistake.\n\
         Run: cargo test --test outcome_combinators"
    );
    assert!(
        !ok.is_pending(),
        "PREDICATE is_pending FALSE POSITIVE: Ok(1).is_pending() must be false.\n\
         Investigate: src/outcome/mod.rs is_pending predicate.\n\
         Common causes: is_pending matches Ok variant by mistake.\n\
         Run: cargo test --test outcome_combinators"
    );
    assert!(
        !ok.is_cancelled(),
        "PREDICATE is_cancelled FALSE POSITIVE: Ok(1).is_cancelled() must be false.\n\
         Investigate: src/outcome/mod.rs is_cancelled predicate.\n\
         Common causes: is_cancelled matches Ok variant by mistake.\n\
         Run: cargo test --test outcome_combinators"
    );
    assert!(
        !ok.is_batch(),
        "PREDICATE is_batch FALSE POSITIVE: Ok(1).is_batch() must be false.\n\
         Investigate: src/outcome/mod.rs is_batch predicate.\n\
         Common causes: is_batch matches Ok variant by mistake.\n\
         Run: cargo test --test outcome_combinators"
    );

    let err: Outcome<i32> = Outcome::Err(test_err());
    assert!(
        err.is_err(),
        "PREDICATE is_err FAILED: Err(...).is_err() must be true.\n\
         Investigate: src/outcome/mod.rs is_err predicate.\n\
         Common causes: is_err does not match Err variant.\n\
         Run: cargo test --test outcome_combinators"
    );
    assert!(
        err.is_terminal(),
        "PREDICATE is_terminal FAILED: Err(...).is_terminal() must be true.\n\
         Investigate: src/outcome/mod.rs is_terminal predicate.\n\
         Common causes: Err not listed as terminal in is_terminal match.\n\
         Run: cargo test --test outcome_combinators"
    );

    let retry: Outcome<i32> = Outcome::retry(100, 1, 3, "r");
    assert!(
        retry.is_retry(),
        "PREDICATE is_retry FAILED: Retry(...).is_retry() must be true.\n\
         Investigate: src/outcome/mod.rs is_retry predicate.\n\
         Common causes: is_retry does not match Retry variant.\n\
         Run: cargo test --test outcome_combinators"
    );
    assert!(
        !retry.is_terminal(),
        "PREDICATE is_terminal FALSE POSITIVE: Retry(...).is_terminal() must be false.\n\
         Investigate: src/outcome/mod.rs is_terminal predicate.\n\
         Common causes: Retry incorrectly listed as terminal in is_terminal match.\n\
         Run: cargo test --test outcome_combinators"
    );

    let pending: Outcome<i32> =
        Outcome::pending(batpak::outcome::WaitCondition::Event { event_id: 123 }, 456);
    assert!(
        pending.is_pending(),
        "PREDICATE is_pending FAILED: Pending(...).is_pending() must be true.\n\
         Investigate: src/outcome/mod.rs is_pending predicate.\n\
         Common causes: is_pending does not match Pending variant.\n\
         Run: cargo test --test outcome_combinators"
    );
    assert!(
        !pending.is_terminal(),
        "PREDICATE is_terminal FALSE POSITIVE: Pending(...).is_terminal() must be false.\n\
         Investigate: src/outcome/mod.rs is_terminal predicate.\n\
         Common causes: Pending incorrectly listed as terminal in is_terminal match.\n\
         Run: cargo test --test outcome_combinators"
    );

    let cancelled: Outcome<i32> = Outcome::cancelled("c");
    assert!(
        cancelled.is_cancelled(),
        "PREDICATE is_cancelled FAILED: Cancelled(...).is_cancelled() must be true.\n\
         Investigate: src/outcome/mod.rs is_cancelled predicate.\n\
         Common causes: is_cancelled does not match Cancelled variant.\n\
         Run: cargo test --test outcome_combinators"
    );
    assert!(
        cancelled.is_terminal(),
        "PREDICATE is_terminal FAILED: Cancelled(...).is_terminal() must be true.\n\
         Investigate: src/outcome/mod.rs is_terminal predicate.\n\
         Common causes: Cancelled not listed as terminal in is_terminal match.\n\
         Run: cargo test --test outcome_combinators"
    );

    let batch: Outcome<i32> = Outcome::Batch(vec![Outcome::Ok(1)]);
    assert!(
        batch.is_batch(),
        "PREDICATE is_batch FAILED: Batch(...).is_batch() must be true.\n\
         Investigate: src/outcome/mod.rs is_batch predicate.\n\
         Common causes: is_batch does not match Batch variant.\n\
         Run: cargo test --test outcome_combinators"
    );
    assert!(
        !batch.is_terminal(),
        "PREDICATE is_terminal FALSE POSITIVE: Batch(...).is_terminal() must be false.\n\
         Investigate: src/outcome/mod.rs is_terminal predicate.\n\
         Common causes: Batch incorrectly listed as terminal in is_terminal match.\n\
         Run: cargo test --test outcome_combinators"
    );
}

// --- join_any ---

#[test]
fn join_any_first_ok_wins() {
    let outcomes = vec![
        Outcome::Err(test_err()),
        Outcome::Ok(42),
        Outcome::Ok(99), // should never be reached
    ];
    let result = batpak::outcome::join_any(outcomes);
    assert_eq!(
        result,
        Outcome::Ok(42),
        "JOIN_ANY FIRST OK NOT SELECTED: expected Ok(42) (first Ok encountered), got {:?}.\n\
         Investigate: src/outcome/mod.rs join_any Ok selection logic.\n\
         Common causes: join_any returns last Ok instead of first, or skips Ok after Err.\n\
         Run: cargo test --test outcome_combinators",
        result
    );
}

#[test]
fn join_any_all_err_returns_last() {
    let outcomes: Vec<Outcome<i32>> = vec![
        Outcome::Err(OutcomeError {
            kind: ErrorKind::Internal,
            message: "first".into(),
            compensation: None,
            retryable: false,
        }),
        Outcome::Err(OutcomeError {
            kind: ErrorKind::NotFound,
            message: "last".into(),
            compensation: None,
            retryable: false,
        }),
    ];
    let result = batpak::outcome::join_any(outcomes);
    match result {
        Outcome::Err(e) => assert_eq!(e.message, "last",
            "JOIN_ANY ALL_ERR WRONG ERROR RETURNED: expected last Err message \"last\", got {:?}.\n\
             Investigate: src/outcome/mod.rs join_any all-Err accumulation.\n\
             Common causes: join_any returns first Err instead of last when all are Err.\n\
             Run: cargo test --test outcome_combinators",
            e.message),
        _ => panic!(
            "JOIN_ANY ALL_ERR WRONG VARIANT: all Err inputs should yield Err, got non-Err.\n\
             Investigate: src/outcome/mod.rs join_any all-Err path.\n\
             Common causes: join_any returns Ok(default) or panics when all inputs are Err.\n\
             Run: cargo test --test outcome_combinators"
        ),
    }
}

#[test]
fn join_any_empty_returns_err() {
    let outcomes: Vec<Outcome<i32>> = vec![];
    let result = batpak::outcome::join_any(outcomes);
    assert!(
        result.is_err(),
        "JOIN_ANY EMPTY NOT ERR: join_any on empty vec must return Err, got non-Err.\n\
         Investigate: src/outcome/mod.rs join_any empty-input handling.\n\
         Common causes: join_any returns a default Ok or panics on empty input.\n\
         Run: cargo test --test outcome_combinators"
    );
}

#[test]
fn join_any_retry_propagates() {
    let outcomes: Vec<Outcome<i32>> = vec![
        Outcome::Err(test_err()),
        Outcome::retry(100, 1, 3, "try later"),
    ];
    let result = batpak::outcome::join_any(outcomes);
    assert!(
        result.is_retry(),
        "JOIN_ANY RETRY NOT PROPAGATED: join_any with a Retry should propagate Retry immediately.\n\
         Investigate: src/outcome/mod.rs join_any Retry short-circuit logic.\n\
         Common causes: join_any ignores Retry and continues past it, returning Err instead.\n\
         Run: cargo test --test outcome_combinators"
    );
}

// --- zip edge cases ---

#[test]
fn zip_err_plus_ok() {
    let result = batpak::outcome::zip(Outcome::<i32>::Err(test_err()), Outcome::Ok(42));
    assert!(
        result.is_err(),
        "ZIP ERR+OK NOT ERR: zip(Err, Ok) must return Err, got non-Err.\n\
         Investigate: src/outcome/mod.rs zip Err propagation.\n\
         Common causes: zip ignores Err on left side and returns Ok from right side.\n\
         Run: cargo test --test outcome_combinators"
    );
}

#[test]
fn zip_ok_plus_cancelled() {
    let result = batpak::outcome::zip(Outcome::Ok(1), Outcome::<i32>::cancelled("no"));
    assert!(result.is_cancelled(),
        "ZIP OK+CANCELLED NOT CANCELLED: zip(Ok, Cancelled) must return Cancelled, got non-Cancelled.\n\
         Investigate: src/outcome/mod.rs zip Cancelled propagation.\n\
         Common causes: zip ignores Cancelled on right side and returns Ok from left side.\n\
         Run: cargo test --test outcome_combinators");
}

#[test]
fn zip_batch_plus_ok() {
    let batch = Outcome::Batch(vec![Outcome::Ok(1), Outcome::Ok(2)]);
    let result = batpak::outcome::zip(batch, Outcome::Ok(10));
    match result {
        Outcome::Batch(items) => {
            assert_eq!(
                items.len(),
                2,
                "ZIP BATCH+OK LENGTH WRONG: expected 2 items in result Batch, got {}.\n\
                 Investigate: src/outcome/mod.rs zip Batch distribution.\n\
                 Common causes: zip drops items or duplicates them when distributing over Batch.\n\
                 Run: cargo test --test outcome_combinators",
                items.len()
            );
            assert_eq!(
                items[0],
                Outcome::Ok((1, 10)),
                "ZIP BATCH+OK ITEM 0 WRONG: expected Ok((1, 10)), got {:?}.\n\
                 Investigate: src/outcome/mod.rs zip Batch left-distribution.\n\
                 Common causes: zip pairs wrong elements or loses the right-side value.\n\
                 Run: cargo test --test outcome_combinators",
                items[0]
            );
            assert_eq!(
                items[1],
                Outcome::Ok((2, 10)),
                "ZIP BATCH+OK ITEM 1 WRONG: expected Ok((2, 10)), got {:?}.\n\
                 Investigate: src/outcome/mod.rs zip Batch left-distribution.\n\
                 Common causes: zip pairs wrong elements or loses the right-side value.\n\
                 Run: cargo test --test outcome_combinators",
                items[1]
            );
        }
        _ => panic!(
            "ZIP BATCH+OK WRONG VARIANT: expected Batch result when left is Batch.\n\
             Investigate: src/outcome/mod.rs zip Batch arm.\n\
             Common causes: zip collapses Batch instead of distributing over it.\n\
             Run: cargo test --test outcome_combinators"
        ),
    }
}

#[test]
fn zip_ok_plus_batch() {
    let batch = Outcome::Batch(vec![Outcome::Ok(10), Outcome::Ok(20)]);
    let result = batpak::outcome::zip(Outcome::Ok(1), batch);
    match result {
        Outcome::Batch(items) => {
            assert_eq!(items.len(), 2,
                "ZIP OK+BATCH LENGTH WRONG: expected 2 items in result Batch, got {}.\n\
                 Investigate: src/outcome/mod.rs zip Batch right-distribution.\n\
                 Common causes: zip drops items or duplicates them when distributing over right Batch.\n\
                 Run: cargo test --test outcome_combinators",
                items.len());
            assert_eq!(
                items[0],
                Outcome::Ok((1, 10)),
                "ZIP OK+BATCH ITEM 0 WRONG: expected Ok((1, 10)), got {:?}.\n\
                 Investigate: src/outcome/mod.rs zip Batch right-distribution.\n\
                 Common causes: zip pairs wrong elements or loses the left-side value.\n\
                 Run: cargo test --test outcome_combinators",
                items[0]
            );
            assert_eq!(
                items[1],
                Outcome::Ok((1, 20)),
                "ZIP OK+BATCH ITEM 1 WRONG: expected Ok((1, 20)), got {:?}.\n\
                 Investigate: src/outcome/mod.rs zip Batch right-distribution.\n\
                 Common causes: zip pairs wrong elements or loses the left-side value.\n\
                 Run: cargo test --test outcome_combinators",
                items[1]
            );
        }
        _ => panic!(
            "ZIP OK+BATCH WRONG VARIANT: expected Batch result when right is Batch.\n\
             Investigate: src/outcome/mod.rs zip Batch arm.\n\
             Common causes: zip collapses right Batch instead of distributing over it.\n\
             Run: cargo test --test outcome_combinators"
        ),
    }
}

// --- map distributes over Batch ---

#[test]
fn map_distributes_over_batch() {
    let batch = Outcome::Batch(vec![
        Outcome::Ok(1),
        Outcome::Ok(2),
        Outcome::Err(test_err()),
    ]);
    let result = batch.map(|x| x * 10);
    match result {
        Outcome::Batch(items) => {
            assert_eq!(
                items[0],
                Outcome::Ok(10),
                "MAP BATCH ITEM 0 WRONG: expected Ok(10) after map(*10) on Ok(1), got {:?}.\n\
                 Investigate: src/outcome/mod.rs map Batch distribution.\n\
                 Common causes: map does not apply function to Ok items inside Batch.\n\
                 Run: cargo test --test outcome_combinators",
                items[0]
            );
            assert_eq!(
                items[1],
                Outcome::Ok(20),
                "MAP BATCH ITEM 1 WRONG: expected Ok(20) after map(*10) on Ok(2), got {:?}.\n\
                 Investigate: src/outcome/mod.rs map Batch distribution.\n\
                 Common causes: map applies function only to first item, skipping rest.\n\
                 Run: cargo test --test outcome_combinators",
                items[1]
            );
            assert!(
                items[2].is_err(),
                "MAP BATCH ERR ITEM CHANGED: Err in Batch must pass through map unchanged.\n\
                 Investigate: src/outcome/mod.rs map Batch Err pass-through.\n\
                 Common causes: map attempts to apply function to Err items and changes variant.\n\
                 Run: cargo test --test outcome_combinators"
            );
        }
        _ => panic!(
            "MAP BATCH WRONG VARIANT: expected Batch result from map on Batch.\n\
             Investigate: src/outcome/mod.rs map Batch arm.\n\
             Common causes: map collapses Batch instead of distributing over items.\n\
             Run: cargo test --test outcome_combinators"
        ),
    }
}

// --- and_then distributes over Batch ---

#[test]
fn and_then_distributes_over_batch() {
    let batch = Outcome::Batch(vec![Outcome::Ok(1), Outcome::Ok(2)]);
    let result = batch.and_then(|x| {
        if x > 1 {
            Outcome::Ok(x * 10)
        } else {
            Outcome::Err(test_err())
        }
    });
    match result {
        Outcome::Batch(items) => {
            assert!(items[0].is_err(),
                "AND_THEN BATCH ITEM 0 NOT ERR: Ok(1) with x<=1 branch should produce Err, got {:?}.\n\
                 Investigate: src/outcome/mod.rs and_then Batch distribution.\n\
                 Common causes: and_then does not apply closure to Ok items in Batch.\n\
                 Run: cargo test --test outcome_combinators",
                items[0]);
            assert_eq!(items[1], Outcome::Ok(20),
                "AND_THEN BATCH ITEM 1 WRONG: Ok(2) with x>1 branch should produce Ok(20), got {:?}.\n\
                 Investigate: src/outcome/mod.rs and_then Batch distribution.\n\
                 Common causes: and_then applies closure only to first item, or uses wrong index.\n\
                 Run: cargo test --test outcome_combinators",
                items[1]);
        }
        _ => panic!(
            "AND_THEN BATCH WRONG VARIANT: expected Batch result from and_then on Batch.\n\
             Investigate: src/outcome/mod.rs and_then Batch arm.\n\
             Common causes: and_then collapses Batch instead of distributing over items.\n\
             Run: cargo test --test outcome_combinators"
        ),
    }
}

// --- Retry/Pending/Cancelled pass through map/and_then ---

#[test]
fn map_passes_through_non_ok() {
    let retry: Outcome<i32> = Outcome::retry(100, 1, 3, "wait");
    let result = retry.map(|x| x * 2);
    assert!(
        result.is_retry(),
        "MAP CHANGED RETRY: map on Retry must return Retry unchanged.\n\
         Investigate: src/outcome/mod.rs map non-Ok pass-through.\n\
         Common causes: map applies function to Retry or changes variant.\n\
         Run: cargo test --test outcome_combinators"
    );

    let pending: Outcome<i32> = Outcome::pending(
        batpak::outcome::WaitCondition::Timeout { resume_at_ms: 1000 },
        42,
    );
    let result = pending.map(|x| x * 2);
    assert!(
        result.is_pending(),
        "MAP CHANGED PENDING: map on Pending must return Pending unchanged.\n\
         Investigate: src/outcome/mod.rs map non-Ok pass-through.\n\
         Common causes: map applies function to Pending or changes variant.\n\
         Run: cargo test --test outcome_combinators"
    );

    let cancelled: Outcome<i32> = Outcome::cancelled("no");
    let result = cancelled.map(|x| x * 2);
    assert!(
        result.is_cancelled(),
        "MAP CHANGED CANCELLED: map on Cancelled must return Cancelled unchanged.\n\
         Investigate: src/outcome/mod.rs map non-Ok pass-through.\n\
         Common causes: map applies function to Cancelled or changes variant.\n\
         Run: cargo test --test outcome_combinators"
    );
}

// --- WaitCondition / CompensationAction coverage ---

#[test]
fn wait_condition_variants() {
    use batpak::outcome::WaitCondition;

    let timeout = WaitCondition::Timeout { resume_at_ms: 5000 };
    let event = WaitCondition::Event { event_id: 123 };
    let all = WaitCondition::All(vec![timeout.clone(), event.clone()]);
    let any = WaitCondition::Any(vec![timeout, event]);
    let custom = WaitCondition::Custom {
        tag: 42,
        data: vec![1, 2, 3],
    };

    // Verify serde round-trip
    for condition in [all, any, custom] {
        let json = serde_json::to_string(&condition).expect("serialize");
        let _: WaitCondition = serde_json::from_str(&json).expect("deserialize");
    }
}

#[test]
fn compensation_action_variants() {
    use batpak::outcome::CompensationAction;

    let rollback = CompensationAction::Rollback {
        event_ids: vec![1, 2, 3],
    };
    let notify = CompensationAction::Notify {
        target_id: 42,
        message: "oops".into(),
    };
    let release = CompensationAction::Release {
        resource_ids: vec![100],
    };
    let custom = CompensationAction::Custom {
        action_type: "refund".into(),
        data: vec![0xFF],
    };

    // Verify serde round-trip
    for action in [rollback, notify, release, custom] {
        let json = serde_json::to_string(&action).expect("serialize");
        let _: CompensationAction = serde_json::from_str(&json).expect("deserialize");
    }
}

#[test]
fn outcome_error_with_compensation() {
    use batpak::outcome::CompensationAction;

    let err = OutcomeError {
        kind: ErrorKind::Conflict,
        message: "double booking".into(),
        compensation: Some(CompensationAction::Rollback { event_ids: vec![1] }),
        retryable: true,
    };

    assert!(
        err.retryable,
        "OUTCOME_ERROR RETRYABLE WRONG: expected retryable=true for this error.\n\
         Investigate: src/outcome/mod.rs OutcomeError retryable field.\n\
         Common causes: retryable field not set correctly during construction.\n\
         Run: cargo test --test outcome_combinators"
    );
    assert!(
        err.compensation.is_some(),
        "OUTCOME_ERROR COMPENSATION MISSING: expected compensation to be Some.\n\
         Investigate: src/outcome/mod.rs OutcomeError compensation field.\n\
         Common causes: compensation field dropped or not propagated through constructors.\n\
         Run: cargo test --test outcome_combinators"
    );
    assert_eq!(
        err.kind,
        ErrorKind::Conflict,
        "OUTCOME_ERROR KIND WRONG: expected ErrorKind::Conflict, got {:?}.\n\
         Investigate: src/outcome/mod.rs OutcomeError kind field.\n\
         Common causes: kind field overwritten or defaulted incorrectly.\n\
         Run: cargo test --test outcome_combinators",
        err.kind
    );

    // Direct construction with compensation
    let err2 = OutcomeError {
        kind: ErrorKind::Internal,
        message: "fail".into(),
        compensation: Some(CompensationAction::Notify {
            target_id: 99,
            message: "cleanup".into(),
        }),
        retryable: false,
    };
    assert!(err2.compensation.is_some(),
        "OUTCOME_ERROR COMPENSATION MISSING: expected err2.compensation to be Some after direct construction.\n\
         Investigate: src/outcome/mod.rs OutcomeError compensation field.\n\
         Common causes: compensation field dropped or not preserved in OutcomeError struct.\n\
         Run: cargo test --test outcome_combinators");
}

/// Regression test: join_all and flatten must not require T: Clone.
/// Both previously had spurious Clone bounds that rejected valid non-Clone payloads.
/// [FILE:src/outcome/combine.rs — join_all]
/// [FILE:src/outcome/mod.rs — flatten]
#[test]
fn join_all_and_flatten_accept_non_clone_types() {
    use batpak::outcome::combine::{join_all, join_any};

    // A type that is deliberately NOT Clone or Copy.
    struct Unique(u64);

    // join_all: should compile and work with non-Clone T
    let outcomes = vec![Outcome::Ok(Unique(1)), Outcome::Ok(Unique(2))];
    let joined = join_all(outcomes);
    assert!(
        matches!(&joined, Outcome::Ok(v) if v.len() == 2),
        "JOIN_ALL NON-CLONE REGRESSION: join_all should accept non-Clone types.\n\
         Investigate: src/outcome/combine.rs join_all generic bounds.\n\
         Run: cargo test --test outcome_combinators join_all_and_flatten"
    );

    // join_any: already accepted non-Clone (control check)
    let outcomes2 = vec![Outcome::Ok(Unique(10)), Outcome::Ok(Unique(20))];
    let any = join_any(outcomes2);
    assert!(matches!(any, Outcome::Ok(Unique(10))));

    // flatten: should compile and work with non-Clone T
    let nested: Outcome<Outcome<Unique>> = Outcome::Ok(Outcome::Ok(Unique(42)));
    let flat = nested.flatten();
    assert!(
        matches!(flat, Outcome::Ok(Unique(42))),
        "FLATTEN NON-CLONE REGRESSION: flatten should accept non-Clone types.\n\
         Investigate: src/outcome/mod.rs impl Outcome<Outcome<T>> flatten.\n\
         Run: cargo test --test outcome_combinators join_all_and_flatten"
    );
}

// ============================================================
// Retry / Pending / Cancelled variant coverage for combinators
// DEFENDS: FM-013 (Coverage Mirage — variant coverage, not just happy path)
// ============================================================

fn test_retry() -> Outcome<i32> {
    Outcome::Retry {
        after_ms: 500,
        attempt: 1,
        max_attempts: 3,
        reason: "test retry".into(),
    }
}

fn test_pending() -> Outcome<i32> {
    Outcome::Pending {
        condition: batpak::outcome::wait::WaitCondition::Timeout { resume_at_ms: 9999 },
        resume_token: 42,
    }
}

fn test_cancelled() -> Outcome<i32> {
    Outcome::Cancelled {
        reason: "test cancel".into(),
    }
}

#[test]
fn zip_retry_propagates() {
    let result = batpak::outcome::zip(test_retry(), Outcome::Ok(1));
    assert!(
        result.is_retry(),
        "ZIP RETRY PROPAGATION: zip(Retry, Ok) must yield Retry.\n\
         Investigate: src/outcome/combine.rs zip Retry arm.\n\
         Run: cargo test --test outcome_combinators zip_retry_propagates"
    );
    // Also test Ok first, Retry second
    let result2 = batpak::outcome::zip(Outcome::Ok(1), test_retry());
    assert!(
        result2.is_retry(),
        "ZIP RETRY PROPAGATION (reversed): zip(Ok, Retry) must yield Retry.\n\
         Investigate: src/outcome/combine.rs zip Retry arm.\n\
         Run: cargo test --test outcome_combinators zip_retry_propagates"
    );
}

#[test]
fn zip_pending_propagates() {
    let result = batpak::outcome::zip(Outcome::Ok(1), test_pending());
    assert!(
        result.is_pending(),
        "ZIP PENDING PROPAGATION: zip(Ok, Pending) must yield Pending.\n\
         Investigate: src/outcome/combine.rs zip Pending arm.\n\
         Run: cargo test --test outcome_combinators zip_pending_propagates"
    );
}

#[test]
fn zip_retry_beats_pending() {
    // Priority: Err > Cancelled > Retry > Pending
    let result = batpak::outcome::zip(test_retry(), test_pending());
    assert!(
        result.is_retry(),
        "ZIP PRIORITY: zip(Retry, Pending) must yield Retry (Retry > Pending).\n\
         Investigate: src/outcome/combine.rs zip priority order.\n\
         Run: cargo test --test outcome_combinators zip_retry_beats_pending"
    );
}

#[test]
fn zip_cancelled_beats_retry() {
    // Priority: Err > Cancelled > Retry > Pending
    let result = batpak::outcome::zip(test_cancelled(), test_retry());
    assert!(
        result.is_cancelled(),
        "ZIP PRIORITY: zip(Cancelled, Retry) must yield Cancelled (Cancelled > Retry).\n\
         Investigate: src/outcome/combine.rs zip priority order.\n\
         Run: cargo test --test outcome_combinators zip_cancelled_beats_retry"
    );
}

#[test]
fn join_all_retry_short_circuits() {
    let outcomes = vec![Outcome::Ok(1), test_retry(), Outcome::Ok(3)];
    let result = batpak::outcome::join_all(outcomes);
    assert!(
        result.is_retry(),
        "JOIN_ALL RETRY SHORT-CIRCUIT: join_all with a Retry element must yield Retry.\n\
         Investigate: src/outcome/combine.rs join_all Retry arm.\n\
         Run: cargo test --test outcome_combinators join_all_retry_short_circuits"
    );
}

#[test]
fn join_all_pending_short_circuits() {
    let outcomes = vec![Outcome::Ok(1), test_pending()];
    let result = batpak::outcome::join_all(outcomes);
    assert!(
        result.is_pending(),
        "JOIN_ALL PENDING SHORT-CIRCUIT: join_all with a Pending element must yield Pending.\n\
         Investigate: src/outcome/combine.rs join_all Pending arm.\n\
         Run: cargo test --test outcome_combinators join_all_pending_short_circuits"
    );
}

#[test]
fn join_all_cancelled_short_circuits() {
    let outcomes = vec![Outcome::Ok(1), test_cancelled()];
    let result = batpak::outcome::join_all(outcomes);
    assert!(
        result.is_cancelled(),
        "JOIN_ALL CANCELLED SHORT-CIRCUIT: join_all with Cancelled element must yield Cancelled.\n\
         Investigate: src/outcome/combine.rs join_all Cancelled arm.\n\
         Run: cargo test --test outcome_combinators join_all_cancelled_short_circuits"
    );
}

#[test]
fn join_any_pending_propagates() {
    let err = OutcomeError {
        kind: ErrorKind::Internal,
        message: "fail".into(),
        compensation: None,
        retryable: false,
    };
    let outcomes: Vec<Outcome<i32>> = vec![Outcome::Err(err), test_pending()];
    let result = batpak::outcome::join_any(outcomes);
    assert!(
        result.is_pending(),
        "JOIN_ANY PENDING PROPAGATION: join_any([Err, Pending]) must yield Pending.\n\
         Investigate: src/outcome/combine.rs join_any 'other' arm.\n\
         Run: cargo test --test outcome_combinators join_any_pending_propagates"
    );
}

// ===== Wave 2A: WaitCondition + CompensationAction behavioral tests =====
// These types had only wire-format serde tests. Now we verify semantic construction
// and variant differentiation. DEFENDS: FM-009 (Polite Downgrade — types must carry
// real data, not just pass serde round-trips).

#[test]
fn wait_condition_timeout_carries_resume_time() {
    use batpak::outcome::WaitCondition;
    let wc = WaitCondition::Timeout {
        resume_at_ms: 1_700_000_000_000,
    };
    match &wc {
        WaitCondition::Timeout { resume_at_ms } => {
            assert_eq!(
                *resume_at_ms, 1_700_000_000_000,
                "PROPERTY: WaitCondition::Timeout must carry exact resume_at_ms.\n\
                 Investigate: src/outcome/wait.rs WaitCondition::Timeout variant.\n\
                 Common causes: field silently defaulted or truncated."
            );
        }
        _ => panic!("Expected Timeout variant"),
    }
}

#[test]
fn wait_condition_event_carries_id() {
    use batpak::outcome::WaitCondition;
    let id: u128 = 0xDEAD_BEEF_CAFE_BABE_1234_5678_9ABC_DEF0;
    let wc = WaitCondition::Event { event_id: id };
    match &wc {
        WaitCondition::Event { event_id } => {
            assert_eq!(
                *event_id, id,
                "PROPERTY: WaitCondition::Event must preserve full u128 event_id.\n\
                 Investigate: src/outcome/wait.rs WaitCondition::Event, wire::u128_bytes.\n\
                 Common causes: byte-order swap in serde, truncation to u64."
            );
        }
        _ => panic!("Expected Event variant"),
    }
}

#[test]
fn wait_condition_all_composes_multiple() {
    use batpak::outcome::WaitCondition;
    let wc = WaitCondition::All(vec![
        WaitCondition::Timeout { resume_at_ms: 100 },
        WaitCondition::Timeout { resume_at_ms: 200 },
    ]);
    match &wc {
        WaitCondition::All(conditions) => {
            assert_eq!(
                conditions.len(),
                2,
                "PROPERTY: WaitCondition::All must preserve all inner conditions.\n\
                 Investigate: src/outcome/wait.rs WaitCondition::All variant."
            );
        }
        _ => panic!("Expected All variant"),
    }
}

#[test]
fn wait_condition_custom_carries_tag_and_data() {
    use batpak::outcome::WaitCondition;
    let wc = WaitCondition::Custom {
        tag: 42,
        data: vec![1, 2, 3, 4],
    };
    match &wc {
        WaitCondition::Custom { tag, data } => {
            assert_eq!(*tag, 42);
            assert_eq!(data, &[1, 2, 3, 4]);
        }
        _ => panic!("Expected Custom variant"),
    }
}

#[test]
fn wait_condition_variants_are_distinguishable() {
    use batpak::outcome::WaitCondition;
    // Variance assertion: different variants produce different Debug output
    let variants: Vec<WaitCondition> = vec![
        WaitCondition::Timeout { resume_at_ms: 100 },
        WaitCondition::Event { event_id: 1 },
        WaitCondition::All(vec![]),
        WaitCondition::Any(vec![]),
        WaitCondition::Custom {
            tag: 0,
            data: vec![],
        },
    ];
    let debug_strings: Vec<String> = variants.iter().map(|v| format!("{v:?}")).collect();
    for i in 0..debug_strings.len() {
        for j in (i + 1)..debug_strings.len() {
            assert_ne!(
                debug_strings[i], debug_strings[j],
                "VARIANCE: WaitCondition variants must produce distinct Debug output.\n\
                 Variant {i} and {j} both produce: {}\n\
                 Investigate: src/outcome/wait.rs derive(Debug).",
                debug_strings[i]
            );
        }
    }
}

#[test]
fn compensation_action_rollback_carries_event_ids() {
    use batpak::outcome::CompensationAction;
    let ids = vec![111u128, 222, 333];
    let action = CompensationAction::Rollback {
        event_ids: ids.clone(),
    };
    match &action {
        CompensationAction::Rollback { event_ids } => {
            assert_eq!(
                event_ids, &ids,
                "PROPERTY: CompensationAction::Rollback must preserve all event_ids.\n\
                 Investigate: src/outcome/wait.rs CompensationAction::Rollback, wire::vec_u128_bytes."
            );
        }
        _ => panic!("Expected Rollback variant"),
    }
}

#[test]
fn compensation_action_notify_carries_message() {
    use batpak::outcome::CompensationAction;
    let action = CompensationAction::Notify {
        target_id: 42,
        message: "something went wrong".into(),
    };
    match &action {
        CompensationAction::Notify { target_id, message } => {
            assert_eq!(*target_id, 42);
            assert_eq!(message, "something went wrong");
        }
        _ => panic!("Expected Notify variant"),
    }
}

#[test]
fn compensation_action_variants_are_distinguishable() {
    use batpak::outcome::CompensationAction;
    let variants: Vec<CompensationAction> = vec![
        CompensationAction::Rollback { event_ids: vec![1] },
        CompensationAction::Notify {
            target_id: 1,
            message: "x".into(),
        },
        CompensationAction::Release {
            resource_ids: vec![1],
        },
        CompensationAction::Custom {
            action_type: "x".into(),
            data: vec![],
        },
    ];
    let debug_strings: Vec<String> = variants.iter().map(|v| format!("{v:?}")).collect();
    for i in 0..debug_strings.len() {
        for j in (i + 1)..debug_strings.len() {
            assert_ne!(
                debug_strings[i], debug_strings[j],
                "VARIANCE: CompensationAction variants must produce distinct Debug output."
            );
        }
    }
}

// ================================================================
// Flatten combinators + OutcomeError display
// ================================================================

#[test]
fn flatten_unwraps_nested_ok() {
    let nested: Outcome<Outcome<i32>> = Outcome::Ok(Outcome::Ok(42));
    let flat = nested.flatten();
    assert_eq!(
        flat,
        Outcome::Ok(42),
        "PROPERTY: Outcome::flatten on Ok(Ok(42)) must produce Ok(42).\n\
         Investigate: src/outcome/mod.rs impl Outcome<Outcome<T>> flatten().\n\
         Common causes: flatten() returning the outer Ok without unwrapping the inner, \
         or not handling the doubly-nested case.\n\
         Run: cargo test --test outcome_combinators flatten_unwraps_nested_ok"
    );
}

#[test]
fn flatten_propagates_inner_err() {
    let err = OutcomeError {
        kind: ErrorKind::Internal,
        message: "inner".into(),
        compensation: None,
        retryable: false,
    };
    let nested: Outcome<Outcome<i32>> = Outcome::Ok(Outcome::Err(err));
    let flat = nested.flatten();
    assert!(
        flat.is_err(),
        "PROPERTY: Outcome::flatten on Ok(Err) must propagate the inner error.\n\
         Investigate: src/outcome/mod.rs impl Outcome<Outcome<T>> flatten().\n\
         Common causes: flatten() treating Ok(Err) as Ok(default) by ignoring the \
         inner variant, or only handling the outer layer.\n\
         Run: cargo test --test outcome_combinators flatten_propagates_inner_err"
    );
}

#[test]
fn flatten_propagates_outer_err() {
    let err = OutcomeError {
        kind: ErrorKind::Internal,
        message: "outer".into(),
        compensation: None,
        retryable: false,
    };
    let nested: Outcome<Outcome<i32>> = Outcome::Err(err);
    let flat = nested.flatten();
    assert!(
        flat.is_err(),
        "PROPERTY: Outcome::flatten on Err must propagate the outer error unchanged.\n\
         Investigate: src/outcome/mod.rs impl Outcome<Outcome<T>> flatten().\n\
         Common causes: flatten() converting outer Err to Ok(default), or returning \
         Outcome::Pending instead of the error.\n\
         Run: cargo test --test outcome_combinators flatten_propagates_outer_err"
    );
}

#[test]
fn flatten_distributes_over_batch() {
    let nested: Outcome<Outcome<i32>> = Outcome::Batch(vec![
        Outcome::Ok(Outcome::Ok(1)),
        Outcome::Ok(Outcome::Ok(2)),
    ]);
    let flat = nested.flatten();
    assert_eq!(
        flat,
        Outcome::Batch(vec![Outcome::Ok(1), Outcome::Ok(2)]),
        "PROPERTY: Outcome::flatten on Batch must flatten each inner Outcome element.\n\
         Investigate: src/outcome/mod.rs impl Outcome<Outcome<T>> flatten() Batch arm.\n\
         Common causes: flatten() not recursing into Batch items, or collecting the \
         batch as Outcome<Vec<Outcome<T>>> instead of Outcome::Batch(Vec<Outcome<T>>).\n\
         Run: cargo test --test outcome_combinators flatten_distributes_over_batch"
    );
}

#[test]
fn outcome_error_display() {
    let err = OutcomeError {
        kind: ErrorKind::Conflict,
        message: "double booking".into(),
        compensation: None,
        retryable: false,
    };
    let s = format!("{err}");
    assert!(
        s.contains("Conflict"),
        "PROPERTY: OutcomeError Display must include the ErrorKind name.\n\
         Investigate: src/outcome/error.rs OutcomeError Display impl.\n\
         Common causes: Display formatting only the message field without including \
         the kind, or kind printed as a raw discriminant number instead of its name.\n\
         Run: cargo test --test outcome_combinators outcome_error_display"
    );
    assert!(
        s.contains("double booking"),
        "PROPERTY: OutcomeError Display must include the error message string.\n\
         Investigate: src/outcome/error.rs OutcomeError Display impl.\n\
         Common causes: Display printing only the kind and omitting the message, \
         or message field not being formatted into the output string.\n\
         Run: cargo test --test outcome_combinators outcome_error_display"
    );
}
