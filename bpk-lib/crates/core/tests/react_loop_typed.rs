//! Integration tests for `react_loop_typed<T, R>` and the shared canal
//! runner (Dispatch Chapter T4b).
//!
//! Exercises:
//!   * happy-path wrong-kind filtering + matched-kind reaction
//!   * user error → `ReactorError::User` surfaced through `join`
//!   * matched-kind decode failure → `ReactorError::Decode` surfaced through `join`
//!   * reactor handler owns mutable state across events (`&mut self`)
//!   * raw `react_loop` + `Reactive<P>` remain unchanged (invariant 6)
//!
//! The canal is `cursor_guaranteed` per ADR-0011, with the same
//! at-least-once / checkpoint semantics documented on the typed reactor
//! surface.

use std::error::Error;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use batpak::event::StoredEvent;
use batpak_testkit::prelude::*;

use batpak_testkit::small_store as small_store_support;
use small_store_support::small_segment_store;

// ─── Payloads ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 9, type_id = 1)]
struct PayloadA {
    n: u64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 9, type_id = 2)]
struct PayloadB {
    note: String,
}

/// Reaction event emitted by the reactor under test.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 9, type_id = 3)]
struct PayloadAReaction {
    original_n: u64,
}

// ─── Reactors ────────────────────────────────────────────────────────────────

/// Basic reactor: for each `PayloadA`, emit one `PayloadAReaction`.
struct CountingReactor {
    seen: Arc<AtomicUsize>,
}

#[derive(Debug)]
struct NeverFails;

impl std::fmt::Display for NeverFails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NeverFails")
    }
}

impl std::error::Error for NeverFails {}

impl TypedReactive<PayloadA> for CountingReactor {
    type Error = NeverFails;
    fn react(
        &mut self,
        event: &StoredEvent<PayloadA>,
        out: &mut ReactionBatch,
        _witness: Option<&batpak::store::AtLeastOnce>,
    ) -> Result<(), Self::Error> {
        self.seen.fetch_add(1, Ordering::SeqCst);
        let reaction_coord =
            Coordinate::new("entity:reaction", "scope:test").expect("reaction coord");
        out.push_typed(
            reaction_coord,
            &PayloadAReaction {
                original_n: event.event.payload.n,
            },
            CausationRef::None,
        )
        .expect("push reaction");
        Ok(())
    }
}

/// Reactor that fails on the third event.
struct FailOnThird {
    seen: Arc<AtomicUsize>,
}

#[derive(Debug)]
struct ThirdFailure;

impl std::fmt::Display for ThirdFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "third event failed")
    }
}

impl std::error::Error for ThirdFailure {}

impl TypedReactive<PayloadA> for FailOnThird {
    type Error = ThirdFailure;
    fn react(
        &mut self,
        _event: &StoredEvent<PayloadA>,
        _out: &mut ReactionBatch,
        _witness: Option<&batpak::store::AtLeastOnce>,
    ) -> Result<(), Self::Error> {
        let prev = self.seen.fetch_add(1, Ordering::SeqCst);
        if prev == 2 {
            return Err(ThirdFailure);
        }
        Ok(())
    }
}

struct WitnessRecordingReactor {
    seen: Arc<AtomicUsize>,
    witness_seen: Arc<AtomicUsize>,
}

impl TypedReactive<PayloadA> for WitnessRecordingReactor {
    type Error = NeverFails;

    fn react(
        &mut self,
        _event: &StoredEvent<PayloadA>,
        _out: &mut ReactionBatch,
        witness: Option<&batpak::store::AtLeastOnce>,
    ) -> Result<(), Self::Error> {
        self.seen.fetch_add(1, Ordering::SeqCst);
        if witness.is_some() {
            self.witness_seen.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────────

fn source_coord() -> Coordinate {
    Coordinate::new("entity:typed-reactor-source", "scope:test").expect("source coord")
}

fn wait_for<F: Fn() -> bool>(cond: F, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    cond()
}

fn test_store() -> (tempfile::TempDir, Arc<Store>) {
    let (d, s) = small_segment_store().expect("small segment store");
    (d, Arc::new(s))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[test]
fn happy_path_reactor_filters_wrong_kind_and_reacts_to_matched() {
    let (_dir, store) = test_store();
    let seen = Arc::new(AtomicUsize::new(0));
    let handle: TypedReactorHandle<NeverFails> = store
        .react_loop_typed::<PayloadA, _>(
            &Region::all(),
            ReactorConfig {
                batch_size: 8,
                idle_sleep: Duration::from_millis(5),
                restart_policy: RestartPolicy::Once,
                checkpoint_id: None,
                canal: ReactorCanal::CursorGuaranteed,
            },
            CountingReactor {
                seen: Arc::clone(&seen),
            },
        )
        .expect("spawn reactor");

    // Interleave two kinds: only PayloadA should reach the reactor.
    store
        .append_typed(&source_coord(), &PayloadA { n: 1 })
        .expect("append PayloadA n=1");
    store
        .append_typed(
            &source_coord(),
            &PayloadB {
                note: "skip me".into(),
            },
        )
        .expect("append PayloadB skip me");
    store
        .append_typed(&source_coord(), &PayloadA { n: 2 })
        .expect("append PayloadA n=2");
    store
        .append_typed(
            &source_coord(),
            &PayloadB {
                note: "skip me again".into(),
            },
        )
        .expect("append PayloadB skip me again");
    store
        .append_typed(&source_coord(), &PayloadA { n: 3 })
        .expect("append PayloadA n=3");

    assert!(
        wait_for(|| seen.load(Ordering::SeqCst) == 3, Duration::from_secs(3)),
        "reactor should see exactly 3 PayloadA events (PayloadB must be filtered): saw {}",
        seen.load(Ordering::SeqCst)
    );

    // Reactions must have landed atomically per source event.
    assert!(wait_for(
        || store.by_fact_typed::<PayloadAReaction>().len() == 3,
        Duration::from_secs(3),
    ));

    handle.stop();
    let _ = handle.join();
}

#[test]
fn user_error_stops_loop_and_surfaces_through_join() {
    let (_dir, store) = test_store();
    let seen = Arc::new(AtomicUsize::new(0));
    let handle: TypedReactorHandle<ThirdFailure> = store
        .react_loop_typed::<PayloadA, _>(
            &Region::all(),
            ReactorConfig {
                batch_size: 1,
                idle_sleep: Duration::from_millis(5),
                // Choose a policy that does not retry forever: Once gives the
                // worker a single restart attempt before it gives up.
                restart_policy: RestartPolicy::Once,
                checkpoint_id: None,
                canal: ReactorCanal::CursorGuaranteed,
            },
            FailOnThird {
                seen: Arc::clone(&seen),
            },
        )
        .expect("spawn reactor");

    for n in 1..=5 {
        store
            .append_typed(&source_coord(), &PayloadA { n })
            .expect("append PayloadA in fail-on-third stream");
    }

    // Wait for at least the 3rd event to have been attempted.
    let _ = wait_for(|| seen.load(Ordering::SeqCst) >= 3, Duration::from_secs(5));

    // Worker will exhaust its restart budget and stop.
    let join_result = handle.join();
    assert!(
        matches!(&join_result, Err(ReactorError::User(_))),
        "expected ReactorError::User, got {join_result:?}"
    );
    let err = join_result.expect_err("user error must surface through join");
    assert!(
        err.source().is_some(),
        "ReactorError::User must expose the handler error as source()"
    );
}

#[test]
fn lossy_subscription_canal_is_explicit_and_never_mints_at_least_once() {
    let (_dir, store) = test_store();
    let seen = Arc::new(AtomicUsize::new(0));
    let witness_seen = Arc::new(AtomicUsize::new(0));
    let handle: TypedReactorHandle<NeverFails> = store
        .react_loop_typed::<PayloadA, _>(
            &Region::all(),
            ReactorConfig {
                batch_size: 1,
                idle_sleep: Duration::from_millis(5),
                restart_policy: RestartPolicy::Once,
                checkpoint_id: None,
                canal: ReactorCanal::LossySubscription,
            },
            WitnessRecordingReactor {
                seen: Arc::clone(&seen),
                witness_seen: Arc::clone(&witness_seen),
            },
        )
        .expect("spawn lossy reactor");

    store
        .append_typed(&source_coord(), &PayloadA { n: 77 })
        .expect("append PayloadA n=77");

    assert!(
        wait_for(|| seen.load(Ordering::SeqCst) == 1, Duration::from_secs(3)),
        "lossy subscription canal should process the matching event when the subscriber keeps up"
    );
    assert_eq!(
        witness_seen.load(Ordering::SeqCst),
        0,
        "lossy subscription canal must not fabricate an AtLeastOnce witness"
    );

    handle.stop_and_join().expect("clean stop and join");
}

// ─── Matched-kind decode failure path ─────────────────────────────────────────
//
// A reactor bound to `ShapeX` expects `Event<Value>.payload` to deserialize
// into `ShapeX`. We write events at `ShapeX::KIND` via the raw `store.append`
// surface with a JSON payload that does NOT match `ShapeX` — e.g., missing a
// required field. Per the unified decode-failure contract, the runner must
// stop and surface `ReactorError::Decode` through `handle.join()`.

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 9, type_id = 5)]
struct ShapeX {
    required_field: u64,
}

struct ShapeXReactor;

impl TypedReactive<ShapeX> for ShapeXReactor {
    type Error = NeverFails;
    fn react(
        &mut self,
        _event: &StoredEvent<ShapeX>,
        _out: &mut ReactionBatch,
        _witness: Option<&batpak::store::AtLeastOnce>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[test]
fn matched_kind_decode_failure_surfaces_reactor_error_decode() {
    let (_dir, store) = test_store();
    let handle: batpak::store::reactor_typed::TypedReactorHandle<NeverFails> = store
        .react_loop_typed::<ShapeX, _>(
            &Region::all(),
            ReactorConfig {
                batch_size: 1,
                idle_sleep: Duration::from_millis(5),
                restart_policy: RestartPolicy::Once,
                checkpoint_id: None,
                canal: ReactorCanal::CursorGuaranteed,
            },
            ShapeXReactor,
        )
        .expect("spawn reactor");

    // Raw append with a payload that is NOT a valid `ShapeX` — kind matches,
    // decode will fail. This is the "matched kind + decode fail" path.
    store
        .append(
            &source_coord(),
            ShapeX::KIND,
            &serde_json::json!({ "different_field": "oops" }),
        )
        .expect("raw append");

    // Under the Once policy the worker exhausts its restart budget on its
    // own after the matched-kind decode fails. `join` is the passive wait
    // for that natural exit — no explicit stop needed.
    let join_result = handle.join();
    assert!(
        matches!(
            join_result,
            Err(batpak::store::reactor_typed::ReactorError::Decode(_))
        ),
        "expected ReactorError::Decode, got {join_result:?}"
    );
}
