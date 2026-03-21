//! Tests for Outcome combinators not covered by monad_laws.rs.
//! Covers: inspect, inspect_err, map_err, or_else, and_then_if,
//! into_result, unwrap_or, unwrap_or_else, join_any, zip edge cases.
//! [SPEC:tests/outcome_combinators.rs]

use free_batteries::prelude::*;
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
        assert_eq!(*v, 42);
        c.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(counter.load(Ordering::SeqCst), 1, "inspect should call closure for Ok");
    assert_eq!(result, Outcome::Ok(42), "inspect should not modify the value");
}

#[test]
fn inspect_skips_non_ok() {
    let err: Outcome<i32> = Outcome::Err(test_err());
    let result = err.inspect(|_| panic!("inspect should not call closure on Err"));
    assert!(result.is_err());

    let retry: Outcome<i32> = Outcome::retry(100, 1, 3, "backoff");
    let result = retry.inspect(|_| panic!("inspect should not call closure on Retry"));
    assert!(result.is_retry());

    let cancelled: Outcome<i32> = Outcome::cancelled("nope");
    let result = cancelled.inspect(|_| panic!("inspect should not call closure on Cancelled"));
    assert!(result.is_cancelled());
}

#[test]
fn inspect_distributes_over_batch() {
    let counter = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&counter);
    let batch = Outcome::Batch(vec![Outcome::Ok(1), Outcome::Err(test_err()), Outcome::Ok(3)]);
    let _ = batch.inspect(move |_| { c.fetch_add(1, Ordering::SeqCst); });
    assert_eq!(counter.load(Ordering::SeqCst), 2, "inspect should be called for each Ok in Batch");
}

// --- inspect_err ---

#[test]
fn inspect_err_calls_closure_on_err() {
    let counter = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&counter);
    let result: Outcome<i32> = Outcome::Err(test_err()).inspect_err(move |e| {
        assert_eq!(e.kind, ErrorKind::Internal);
        c.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(counter.load(Ordering::SeqCst), 1, "inspect_err should call closure for Err");
    assert!(result.is_err());
}

#[test]
fn inspect_err_skips_ok() {
    let result = Outcome::Ok(42).inspect_err(|_| panic!("should not call on Ok"));
    assert_eq!(result, Outcome::Ok(42));
}

#[test]
fn inspect_err_distributes_over_batch() {
    let counter = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&counter);
    let batch = Outcome::Batch(vec![Outcome::Ok(1), Outcome::Err(test_err()), Outcome::Ok(3)]);
    let _ = batch.inspect_err(move |_| { c.fetch_add(1, Ordering::SeqCst); });
    assert_eq!(counter.load(Ordering::SeqCst), 1, "inspect_err should be called for each Err in Batch");
}

// --- map_err ---

#[test]
fn map_err_transforms_error() {
    let result: Outcome<i32> = Outcome::Err(test_err()).map_err(|mut e| {
        e.message = "transformed".into();
        e
    });
    match result {
        Outcome::Err(e) => assert_eq!(e.message, "transformed"),
        _ => panic!("Expected Err"),
    }
}

#[test]
fn map_err_skips_ok() {
    let result = Outcome::Ok(42).map_err(|_| panic!("should not call on Ok"));
    assert_eq!(result, Outcome::Ok(42));
}

#[test]
fn map_err_distributes_over_batch() {
    let batch: Outcome<i32> = Outcome::Batch(vec![
        Outcome::Ok(1),
        Outcome::Err(test_err()),
    ]);
    let result = batch.map_err(|mut e| {
        e.message = "mapped".into();
        e
    });
    match result {
        Outcome::Batch(items) => {
            assert_eq!(items[0], Outcome::Ok(1));
            match &items[1] {
                Outcome::Err(e) => assert_eq!(e.message, "mapped"),
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
    assert_eq!(result, Outcome::Ok(99));
}

#[test]
fn or_else_skips_ok() {
    let result = Outcome::Ok(42).or_else(|_| panic!("should not call on Ok"));
    assert_eq!(result, Outcome::Ok(42));
}

#[test]
fn or_else_skips_non_err_variants() {
    let retry: Outcome<i32> = Outcome::retry(100, 1, 3, "retry");
    let result = retry.or_else(|_| panic!("should not call on Retry"));
    assert!(result.is_retry());
}

#[test]
fn or_else_distributes_over_batch() {
    let batch: Outcome<i32> = Outcome::Batch(vec![
        Outcome::Err(test_err()),
        Outcome::Ok(1),
    ]);
    let result = batch.or_else(|_| Outcome::Ok(0));
    match result {
        Outcome::Batch(items) => {
            assert_eq!(items[0], Outcome::Ok(0)); // recovered
            assert_eq!(items[1], Outcome::Ok(1)); // untouched
        }
        _ => panic!("Expected Batch"),
    }
}

// --- and_then_if ---

#[test]
fn and_then_if_applies_when_predicate_true() {
    let result = Outcome::Ok(42).and_then_if(
        |v| *v > 10,
        |v| Outcome::Ok(v * 2),
    );
    assert_eq!(result, Outcome::Ok(84));
}

#[test]
fn and_then_if_skips_when_predicate_false() {
    let result = Outcome::Ok(5).and_then_if(
        |v| *v > 10,
        |_| panic!("should not call when predicate is false"),
    );
    assert_eq!(result, Outcome::Ok(5));
}

#[test]
fn and_then_if_skips_non_ok() {
    let err: Outcome<i32> = Outcome::Err(test_err());
    let result = err.and_then_if(
        |_| panic!("should not check predicate on Err"),
        |_| panic!("should not apply on Err"),
    );
    assert!(result.is_err());
}

// --- into_result ---

#[test]
fn into_result_ok() {
    let r: Result<i32, OutcomeError> = Outcome::Ok(42).into_result();
    assert_eq!(r.expect("should be Ok"), 42);
}

#[test]
fn into_result_err() {
    let r: Result<i32, OutcomeError> = Outcome::Err(test_err()).into_result();
    assert!(r.is_err());
}

#[test]
fn into_result_cancelled() {
    let r: Result<i32, OutcomeError> = Outcome::cancelled("nope").into_result();
    let err = r.unwrap_err();
    assert!(err.message.contains("cancelled"));
}

#[test]
fn into_result_non_terminal() {
    let r: Result<i32, OutcomeError> = Outcome::retry(100, 1, 3, "wait").into_result();
    let err = r.unwrap_err();
    assert!(err.message.contains("not terminal"));
}

// --- unwrap_or / unwrap_or_else ---

#[test]
fn unwrap_or_returns_value_for_ok() {
    assert_eq!(Outcome::Ok(42).unwrap_or(0), 42);
}

#[test]
fn unwrap_or_returns_default_for_non_ok() {
    let err: Outcome<i32> = Outcome::Err(test_err());
    assert_eq!(err.unwrap_or(99), 99);
}

#[test]
fn unwrap_or_else_returns_value_for_ok() {
    assert_eq!(Outcome::Ok(42).unwrap_or_else(|| 0), 42);
}

#[test]
fn unwrap_or_else_calls_closure_for_non_ok() {
    let err: Outcome<i32> = Outcome::Err(test_err());
    assert_eq!(err.unwrap_or_else(|| 99), 99);
}

// --- predicates ---

#[test]
fn predicates_cover_all_variants() {
    let ok: Outcome<i32> = Outcome::Ok(1);
    assert!(ok.is_ok());
    assert!(ok.is_terminal());
    assert!(!ok.is_err());
    assert!(!ok.is_retry());
    assert!(!ok.is_pending());
    assert!(!ok.is_cancelled());
    assert!(!ok.is_batch());

    let err: Outcome<i32> = Outcome::Err(test_err());
    assert!(err.is_err());
    assert!(err.is_terminal());

    let retry: Outcome<i32> = Outcome::retry(100, 1, 3, "r");
    assert!(retry.is_retry());
    assert!(!retry.is_terminal());

    let pending: Outcome<i32> = Outcome::pending(
        free_batteries::outcome::WaitCondition::Event { event_id: 123 },
        456,
    );
    assert!(pending.is_pending());
    assert!(!pending.is_terminal());

    let cancelled: Outcome<i32> = Outcome::cancelled("c");
    assert!(cancelled.is_cancelled());
    assert!(cancelled.is_terminal());

    let batch: Outcome<i32> = Outcome::Batch(vec![Outcome::Ok(1)]);
    assert!(batch.is_batch());
    assert!(!batch.is_terminal());
}

// --- join_any ---

#[test]
fn join_any_first_ok_wins() {
    let outcomes = vec![
        Outcome::Err(test_err()),
        Outcome::Ok(42),
        Outcome::Ok(99), // should never be reached
    ];
    let result = free_batteries::outcome::join_any(outcomes);
    assert_eq!(result, Outcome::Ok(42));
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
    let result = free_batteries::outcome::join_any(outcomes);
    match result {
        Outcome::Err(e) => assert_eq!(e.message, "last"),
        _ => panic!("Expected Err"),
    }
}

#[test]
fn join_any_empty_returns_err() {
    let outcomes: Vec<Outcome<i32>> = vec![];
    let result = free_batteries::outcome::join_any(outcomes);
    assert!(result.is_err(), "join_any on empty vec should return Err");
}

#[test]
fn join_any_retry_propagates() {
    let outcomes: Vec<Outcome<i32>> = vec![
        Outcome::Err(test_err()),
        Outcome::retry(100, 1, 3, "try later"),
    ];
    let result = free_batteries::outcome::join_any(outcomes);
    assert!(result.is_retry(), "join_any should propagate Retry immediately");
}

// --- zip edge cases ---

#[test]
fn zip_err_plus_ok() {
    let result = free_batteries::outcome::zip(
        Outcome::<i32>::Err(test_err()),
        Outcome::Ok(42),
    );
    assert!(result.is_err());
}

#[test]
fn zip_ok_plus_cancelled() {
    let result = free_batteries::outcome::zip(
        Outcome::Ok(1),
        Outcome::<i32>::cancelled("no"),
    );
    assert!(result.is_cancelled());
}

#[test]
fn zip_batch_plus_ok() {
    let batch = Outcome::Batch(vec![Outcome::Ok(1), Outcome::Ok(2)]);
    let result = free_batteries::outcome::zip(batch, Outcome::Ok(10));
    match result {
        Outcome::Batch(items) => {
            assert_eq!(items.len(), 2);
            assert_eq!(items[0], Outcome::Ok((1, 10)));
            assert_eq!(items[1], Outcome::Ok((2, 10)));
        }
        _ => panic!("Expected Batch"),
    }
}

#[test]
fn zip_ok_plus_batch() {
    let batch = Outcome::Batch(vec![Outcome::Ok(10), Outcome::Ok(20)]);
    let result = free_batteries::outcome::zip(Outcome::Ok(1), batch);
    match result {
        Outcome::Batch(items) => {
            assert_eq!(items.len(), 2);
            assert_eq!(items[0], Outcome::Ok((1, 10)));
            assert_eq!(items[1], Outcome::Ok((1, 20)));
        }
        _ => panic!("Expected Batch"),
    }
}

// --- map distributes over Batch ---

#[test]
fn map_distributes_over_batch() {
    let batch = Outcome::Batch(vec![Outcome::Ok(1), Outcome::Ok(2), Outcome::Err(test_err())]);
    let result = batch.map(|x| x * 10);
    match result {
        Outcome::Batch(items) => {
            assert_eq!(items[0], Outcome::Ok(10));
            assert_eq!(items[1], Outcome::Ok(20));
            assert!(items[2].is_err()); // Err passes through
        }
        _ => panic!("Expected Batch"),
    }
}

// --- and_then distributes over Batch ---

#[test]
fn and_then_distributes_over_batch() {
    let batch = Outcome::Batch(vec![Outcome::Ok(1), Outcome::Ok(2)]);
    let result = batch.and_then(|x| {
        if x > 1 { Outcome::Ok(x * 10) } else { Outcome::Err(test_err()) }
    });
    match result {
        Outcome::Batch(items) => {
            assert!(items[0].is_err());         // 1 <= 1 → Err
            assert_eq!(items[1], Outcome::Ok(20)); // 2 > 1 → Ok(20)
        }
        _ => panic!("Expected Batch"),
    }
}

// --- Retry/Pending/Cancelled pass through map/and_then ---

#[test]
fn map_passes_through_non_ok() {
    let retry: Outcome<i32> = Outcome::retry(100, 1, 3, "wait");
    let result = retry.map(|x| x * 2);
    assert!(result.is_retry());

    let pending: Outcome<i32> = Outcome::pending(
        free_batteries::outcome::WaitCondition::Timeout { resume_at_ms: 1000 },
        42,
    );
    let result = pending.map(|x| x * 2);
    assert!(result.is_pending());

    let cancelled: Outcome<i32> = Outcome::cancelled("no");
    let result = cancelled.map(|x| x * 2);
    assert!(result.is_cancelled());
}

// --- WaitCondition / CompensationAction coverage ---

#[test]
fn wait_condition_variants() {
    use free_batteries::outcome::WaitCondition;

    let timeout = WaitCondition::Timeout { resume_at_ms: 5000 };
    let event = WaitCondition::Event { event_id: 123 };
    let all = WaitCondition::All(vec![timeout.clone(), event.clone()]);
    let any = WaitCondition::Any(vec![timeout, event]);
    let custom = WaitCondition::Custom { tag: 42, data: vec![1, 2, 3] };

    // Verify serde round-trip
    for condition in [all, any, custom] {
        let json = serde_json::to_string(&condition).expect("serialize");
        let _: WaitCondition = serde_json::from_str(&json).expect("deserialize");
    }
}

#[test]
fn compensation_action_variants() {
    use free_batteries::outcome::CompensationAction;

    let rollback = CompensationAction::Rollback { event_ids: vec![1, 2, 3] };
    let notify = CompensationAction::Notify { target_id: 42, message: "oops".into() };
    let release = CompensationAction::Release { resource_ids: vec![100] };
    let custom = CompensationAction::Custom { action_type: "refund".into(), data: vec![0xFF] };

    // Verify serde round-trip
    for action in [rollback, notify, release, custom] {
        let json = serde_json::to_string(&action).expect("serialize");
        let _: CompensationAction = serde_json::from_str(&json).expect("deserialize");
    }
}

#[test]
fn outcome_error_with_compensation() {
    use free_batteries::outcome::CompensationAction;

    let err = OutcomeError {
        kind: ErrorKind::Conflict,
        message: "double booking".into(),
        compensation: Some(CompensationAction::Rollback { event_ids: vec![1] }),
        retryable: true,
    };

    assert!(err.retryable);
    assert!(err.compensation.is_some());
    assert_eq!(err.kind, ErrorKind::Conflict);

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
    assert!(err2.compensation.is_some());
}
