//! Integration tests for `#[derive(MultiEventReactor)]` + `react_loop_multi`
//! (Dispatch Chapter T6). Cover the JSON lane here; raw-msgpack lane
//! parity is covered in `react_loop_multi_raw.rs`.
//!
//! Exercises:
//!   * multi-kind dispatch routes each kind to the right handler
//!   * wrong-kind events are filtered silently
//!   * reactor shares the shared canal runner with T4b (same RestartPolicy,
//!     same JoinHandle, same ReactorError variants)
//!   * matched-kind decode failure surfaces `ReactorError::Decode`
//!   * user error surfaces `ReactorError::User`

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use batpak::event::StoredEvent;
use batpak_testkit::prelude::*;

use batpak_testkit::small_store as small_store_support;
use small_store_support::small_segment_store;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xC, type_id = 1)]
struct PayloadA {
    n: u64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xC, type_id = 2)]
struct PayloadB {
    label: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xC, type_id = 3)]
struct PayloadC {
    amount: i64,
}

/// Reaction events emitted by the multi-reactor, tagged by source kind.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xC, type_id = 10)]
struct ReactionOut {
    source: String,
}

#[derive(Debug)]
struct NeverFails;
impl std::fmt::Display for NeverFails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "never")
    }
}
impl std::error::Error for NeverFails {}

#[derive(Default, MultiEventReactor)]
#[batpak(input = JsonValueInput, error = NeverFails)]
#[batpak(event = PayloadA, handler = on_a)]
#[batpak(event = PayloadB, handler = on_b)]
#[batpak(event = PayloadC, handler = on_c)]
struct Counter {
    a: Arc<AtomicUsize>,
    b: Arc<AtomicUsize>,
    c: Arc<AtomicUsize>,
}

impl Counter {
    fn on_a(
        &mut self,
        _e: &StoredEvent<PayloadA>,
        out: &mut ReactionBatch,
        _witness: Option<&batpak::store::AtLeastOnce>,
    ) -> Result<(), NeverFails> {
        self.a.fetch_add(1, Ordering::SeqCst);
        emit(out, "A")
    }
    fn on_b(
        &mut self,
        _e: &StoredEvent<PayloadB>,
        out: &mut ReactionBatch,
        _witness: Option<&batpak::store::AtLeastOnce>,
    ) -> Result<(), NeverFails> {
        self.b.fetch_add(1, Ordering::SeqCst);
        emit(out, "B")
    }
    fn on_c(
        &mut self,
        _e: &StoredEvent<PayloadC>,
        out: &mut ReactionBatch,
        _witness: Option<&batpak::store::AtLeastOnce>,
    ) -> Result<(), NeverFails> {
        self.c.fetch_add(1, Ordering::SeqCst);
        emit(out, "C")
    }
}

fn emit(out: &mut ReactionBatch, tag: &str) -> Result<(), NeverFails> {
    let coord = Coordinate::new("entity:multi-out", "scope:test").expect("reaction coord");
    out.push_typed(
        coord,
        &ReactionOut { source: tag.into() },
        CausationRef::None,
    )
    .expect("push reaction event");
    Ok(())
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

fn source_coord() -> Coordinate {
    Coordinate::new("entity:multi-src", "scope:test").expect("source coord")
}

fn test_store() -> (tempfile::TempDir, Arc<Store>) {
    let (d, s) = small_segment_store().expect("small segment store");
    (d, Arc::new(s))
}

fn join_with_timeout(
    handle: TypedReactorHandle<NeverFails>,
    timeout: Duration,
) -> Result<(), ReactorError<NeverFails>> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("react-loop-multi-join".into())
        .spawn(move || {
            let _ = tx.send(handle.join());
        })
        .expect("spawn bounded join worker");
    rx.recv_timeout(timeout).unwrap_or_else(|err| match err {
        mpsc::RecvTimeoutError::Timeout => {
            assert!(
                std::hint::black_box(false),
                "multi-reactor dispatch contract: expected reactor to stop within {timeout:?}"
            );
            unreachable!()
        }
        mpsc::RecvTimeoutError::Disconnected => {
            assert!(
                std::hint::black_box(false),
                "multi-reactor dispatch contract: join worker disconnected"
            );
            unreachable!()
        }
    })
}

#[test]
fn multi_kind_dispatch_routes_each_kind_to_right_handler() {
    let (_dir, store) = test_store();
    let a = Arc::new(AtomicUsize::new(0));
    let b = Arc::new(AtomicUsize::new(0));
    let c = Arc::new(AtomicUsize::new(0));

    let reactor = Counter {
        a: Arc::clone(&a),
        b: Arc::clone(&b),
        c: Arc::clone(&c),
    };
    let handle: TypedReactorHandle<NeverFails> = store
        .react_loop_multi(&Region::all(), ReactorConfig::default(), reactor)
        .expect("spawn");

    // Interleaved stream across all three kinds.
    let _ = store
        .append_typed(&source_coord(), &PayloadA { n: 1 })
        .expect("append PayloadA n=1");
    let _ = store
        .append_typed(&source_coord(), &PayloadB { label: "x".into() })
        .expect("append PayloadB x");
    let _ = store
        .append_typed(&source_coord(), &PayloadA { n: 2 })
        .expect("append PayloadA n=2");
    let _ = store
        .append_typed(&source_coord(), &PayloadC { amount: 7 })
        .expect("append PayloadC");
    let _ = store
        .append_typed(&source_coord(), &PayloadB { label: "y".into() })
        .expect("append PayloadB y");

    assert!(
        wait_for(
            || a.load(Ordering::SeqCst) == 2
                && b.load(Ordering::SeqCst) == 2
                && c.load(Ordering::SeqCst) == 1,
            Duration::from_secs(3),
        ),
        "expected 2 A, 2 B, 1 C; got {} / {} / {}",
        a.load(Ordering::SeqCst),
        b.load(Ordering::SeqCst),
        c.load(Ordering::SeqCst)
    );

    assert!(wait_for(
        || store.by_fact_typed::<ReactionOut>().len() == 5,
        Duration::from_secs(3),
    ));

    handle.stop();
    handle.join().expect("clean stop");
}

#[test]
fn relevant_event_kinds_is_generated_from_event_bindings() {
    assert_eq!(
        <Counter as MultiReactive<JsonValueInput>>::relevant_event_kinds(),
        &[PayloadA::KIND, PayloadB::KIND, PayloadC::KIND]
    );
}

// ─── Decode-failure path ──────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xC, type_id = 20)]
struct ShapeY {
    required: u64,
}

#[derive(Default, MultiEventReactor)]
#[batpak(input = JsonValueInput, error = NeverFails)]
#[batpak(event = ShapeY, handler = on_y)]
struct ShapeYReactor {
    _marker: (),
}

impl ShapeYReactor {
    fn on_y(
        &mut self,
        _e: &StoredEvent<ShapeY>,
        _out: &mut ReactionBatch,
        _witness: Option<&batpak::store::AtLeastOnce>,
    ) -> Result<(), NeverFails> {
        Ok(())
    }
}

#[test]
fn matched_kind_decode_failure_surfaces_reactor_error_decode() {
    let (_dir, store) = test_store();
    let handle: TypedReactorHandle<NeverFails> = store
        .react_loop_multi(
            &Region::all(),
            ReactorConfig {
                batch_size: 1,
                idle_sleep: Duration::from_millis(5),
                restart_policy: RestartPolicy::Once,
                checkpoint_id: None,
                canal: ReactorCanal::CursorGuaranteed,
            },
            ShapeYReactor { _marker: () },
        )
        .expect("spawn");

    // Write an event at ShapeY::KIND with a payload that will not decode.
    let _ = store
        .append(
            &source_coord(),
            ShapeY::KIND,
            &serde_json::json!({ "wrong_field": "nope" }),
        )
        .expect("append undecodable matched-kind event");

    // Under the Once policy the worker exhausts its restart budget on its
    // own after the matched-kind decode fails. `join` is the passive wait
    // for that natural exit — no explicit stop needed.
    let join_result = join_with_timeout(handle, Duration::from_secs(2));
    assert!(
        matches!(join_result, Err(ReactorError::Decode(_))),
        "expected ReactorError::Decode, got {join_result:?}"
    );
}

#[test]
fn multi_dispatch_error_reports_user_and_decode_sources() {
    let user_error: MultiDispatchError<NeverFails> = MultiDispatchError::User(NeverFails);
    let user_display = user_error.to_string();
    let user_source = std::error::Error::source(&user_error);
    assert!(
        user_display.contains("multi-reactor user error"),
        "user-facing display should describe the multi-reactor user-error path"
    );
    assert!(
        user_source.is_some(),
        "user variant should expose its source"
    );

    let decode_error: MultiDispatchError<NeverFails> =
        MultiDispatchError::Decode(TypedDecodeError::KindMismatch {
            expected: PayloadA::KIND,
            got: PayloadB::KIND,
        });
    let decode_display = decode_error.to_string();
    let decode_source = std::error::Error::source(&decode_error);
    assert!(
        decode_display.contains("multi-reactor decode failure"),
        "decode-facing display should describe the multi-reactor decode-failure path"
    );
    assert!(
        decode_source.is_some(),
        "decode variant should expose its source"
    );
}
