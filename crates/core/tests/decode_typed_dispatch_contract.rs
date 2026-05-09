// justifies: INV-TEST-PANIC-AS-ASSERTION; test body in tests/decode_typed_dispatch_contract.rs exercises precondition-holds invariants; .unwrap is acceptable in test code where a panic is a test failure.
#![allow(clippy::unwrap_used, clippy::panic)]
//! Behavioural dispatch-contract test for the `DecodeTyped` seam.
//!
//! Covers the observable routing behaviour the `DecodeTyped` seam must
//! uphold so `#[derive(EventSourced)]` and `#[derive(MultiEventReactor)]`
//! continue to dispatch correctly:
//!
//! * A matched kind routes to exactly one handler, not both.
//! * An unrelated kind falls through with no handler call.
//! * Interleaved streams produce the correct per-kind counts.
//!
//! PROVES: any change to `route_typed`'s `Result<Option<T>, _>` signature
//! surfaces here through behavioural assertions alone.

use batpak::coordinate::DagPosition;
use batpak::event::{DecodeTyped, Event, EventHeader, EventKind};
use batpak::EventPayload;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 2, type_id = 1)]
struct PayloadA {
    n: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 2, type_id = 2)]
struct PayloadB {
    s: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, EventPayload)]
#[batpak(category = 2, type_id = 3)]
struct PayloadC {
    /// Never appears in the dispatch chain below — used to prove that events
    /// of unrelated kinds fall through without invoking any handler.
    flag: bool,
}

#[derive(Default)]
struct Counter {
    a_seen: u32,
    b_seen: u32,
}

impl Counter {
    fn on_a(&mut self, p: &PayloadA) {
        self.a_seen = self.a_seen.saturating_add(1);
        let _ = p;
    }

    fn on_b(&mut self, p: &PayloadB) {
        self.b_seen = self.b_seen.saturating_add(1);
        let _ = p;
    }

    /// Routing contract: call at most one handler, chosen by the first
    /// matching `route_typed`. Events outside the relevant kinds fall
    /// through without invoking any handler.
    fn apply(&mut self, event: &Event<serde_json::Value>) {
        match event.route_typed::<PayloadA>() {
            Ok(Some(p)) => self.on_a(&p),
            Ok(None) => match event.route_typed::<PayloadB>() {
                Ok(Some(p)) => self.on_b(&p),
                Ok(None) => {}
                Err(e) => panic!("dispatch contract: decode failed for PayloadB: {e}"),
            },
            Err(e) => panic!("dispatch contract: decode failed for PayloadA: {e}"),
        }
    }
}

fn make_event(kind: EventKind, payload: serde_json::Value) -> Event<serde_json::Value> {
    Event::new(
        EventHeader::new(1, 0, None, 0, DagPosition::root(), 0, kind),
        payload,
    )
}

#[test]
fn dispatch_chain_routes_to_correct_handler() {
    let mut counter = Counter::default();
    counter.apply(&make_event(PayloadA::KIND, serde_json::json!({ "n": 7 })));
    assert_eq!(counter.a_seen, 1);
    assert_eq!(counter.b_seen, 0);

    counter.apply(&make_event(PayloadB::KIND, serde_json::json!({ "s": "x" })));
    assert_eq!(counter.a_seen, 1);
    assert_eq!(counter.b_seen, 1);
}

#[test]
fn dispatch_chain_falls_through_unrelated_kind() {
    let mut counter = Counter::default();
    counter.apply(&make_event(
        PayloadC::KIND,
        serde_json::json!({ "flag": true }),
    ));
    assert_eq!(counter.a_seen, 0);
    assert_eq!(counter.b_seen, 0);
}

#[test]
fn dispatch_chain_counts_across_interleaved_stream() {
    let events = [
        make_event(PayloadA::KIND, serde_json::json!({ "n": 1 })),
        make_event(PayloadC::KIND, serde_json::json!({ "flag": true })),
        make_event(PayloadB::KIND, serde_json::json!({ "s": "a" })),
        make_event(PayloadA::KIND, serde_json::json!({ "n": 2 })),
        make_event(PayloadB::KIND, serde_json::json!({ "s": "b" })),
    ];
    let mut counter = Counter::default();
    for event in &events {
        counter.apply(event);
    }
    assert_eq!(counter.a_seen, 2);
    assert_eq!(counter.b_seen, 2);
}
