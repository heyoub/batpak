//! Raw-msgpack lane parity test for `#[derive(MultiEventReactor)]` (T6).
//! Proves invariant 5 at the reactor surface: a reactor derived with
//! `input = RawMsgpackInput` behaves identically to the `JsonValueInput`
//! variant (see `react_loop_multi.rs`) for analogous payloads.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use batpak::event::StoredEvent;
use batpak_testkit::prelude::*;

use batpak_testkit::small_store as small_store_support;
use small_store_support::small_segment_store;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xC, type_id = 31)]
struct AlphaRaw {
    n: u64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xC, type_id = 32)]
struct BetaRaw {
    label: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xC, type_id = 33)]
struct ReactionRaw {
    via: String,
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
#[batpak(input = RawMsgpackInput, error = NeverFails)]
#[batpak(event = AlphaRaw, handler = on_alpha)]
#[batpak(event = BetaRaw, handler = on_beta)]
struct RawReactor {
    alphas: Arc<AtomicUsize>,
    betas: Arc<AtomicUsize>,
}

impl RawReactor {
    fn on_alpha(
        &mut self,
        _e: &StoredEvent<AlphaRaw>,
        out: &mut ReactionBatch,
        _witness: Option<&batpak::store::AtLeastOnce>,
    ) -> Result<(), NeverFails> {
        self.alphas.fetch_add(1, Ordering::SeqCst);
        let coord = Coordinate::new("entity:raw-out", "scope:test").expect("alpha reaction coord");
        out.push_typed(
            coord,
            &ReactionRaw {
                via: "alpha".into(),
            },
            CausationRef::None,
        )
        .expect("push alpha reaction");
        Ok(())
    }
    fn on_beta(
        &mut self,
        _e: &StoredEvent<BetaRaw>,
        out: &mut ReactionBatch,
        _witness: Option<&batpak::store::AtLeastOnce>,
    ) -> Result<(), NeverFails> {
        self.betas.fetch_add(1, Ordering::SeqCst);
        let coord = Coordinate::new("entity:raw-out", "scope:test").expect("beta reaction coord");
        out.push_typed(
            coord,
            &ReactionRaw { via: "beta".into() },
            CausationRef::None,
        )
        .expect("push beta reaction");
        Ok(())
    }
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

#[test]
fn raw_msgpack_multi_reactor_dispatches_same_as_json_lane() {
    let (_dir, store) = small_segment_store().expect("small segment store");
    let store = Arc::new(store);
    let alphas = Arc::new(AtomicUsize::new(0));
    let betas = Arc::new(AtomicUsize::new(0));

    let reactor = RawReactor {
        alphas: Arc::clone(&alphas),
        betas: Arc::clone(&betas),
    };
    let handle = store
        .react_loop_multi_raw(&Region::all(), ReactorConfig::default(), reactor)
        .expect("spawn raw reactor");

    let source = Coordinate::new("entity:raw-src", "scope:test").expect("source coord");
    store
        .append_typed(&source, &AlphaRaw { n: 1 })
        .expect("append AlphaRaw n=1");
    store
        .append_typed(
            &source,
            &BetaRaw {
                label: "one".into(),
            },
        )
        .expect("append BetaRaw one");
    store
        .append_typed(&source, &AlphaRaw { n: 2 })
        .expect("append AlphaRaw n=2");
    store
        .append_typed(
            &source,
            &BetaRaw {
                label: "two".into(),
            },
        )
        .expect("append BetaRaw two");
    store
        .append_typed(&source, &AlphaRaw { n: 3 })
        .expect("append AlphaRaw n=3");

    assert!(
        wait_for(
            || alphas.load(Ordering::SeqCst) == 3 && betas.load(Ordering::SeqCst) == 2,
            Duration::from_secs(3),
        ),
        "expected 3 alphas + 2 betas; got {} / {}",
        alphas.load(Ordering::SeqCst),
        betas.load(Ordering::SeqCst)
    );

    assert!(wait_for(
        || store.by_fact_typed::<ReactionRaw>().len() == 5,
        Duration::from_secs(3),
    ));

    handle.stop();
    handle.join().expect("clean stop");
}

#[test]
fn store_read_raw_round_trip_witness() {
    // Witness test for `Store::read_raw` — proves the new public surface
    // added in T6 is exercised directly (independent of reactor plumbing).
    let (_dir, store) = small_segment_store().expect("small segment store");
    let coord = Coordinate::new("entity:read-raw-witness", "scope:test").expect("witness coord");
    let receipt = store
        .append_typed(&coord, &AlphaRaw { n: 42 })
        .expect("append");
    let stored = store.read_raw(receipt.event_id).expect("read_raw first");
    let read = store.read_raw(receipt.event_id).expect("read_raw");
    assert_eq!(stored.event.header.event_id, receipt.event_id);
    assert_eq!(read.event.header.event_id, receipt.event_id);
    assert_eq!(
        read.event.payload, stored.event.payload,
        "PROPERTY: read_raw must return stable raw payload bytes across repeated reads"
    );
    assert_eq!(stored.event.header.event_kind, AlphaRaw::KIND);
    // Decode the raw bytes back into AlphaRaw and verify.
    let round_trip: AlphaRaw = rmp_serde::from_slice(&stored.event.payload).expect("decode");
    assert_eq!(round_trip.n, 42);
}
