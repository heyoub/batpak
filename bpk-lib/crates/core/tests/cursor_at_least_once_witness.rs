// justifies: INV-TEST-PANIC-AS-ASSERTION; witness plumbing tests use panic/expect as ordinary integration-test assertions.
#![allow(clippy::unwrap_used, clippy::panic)]
//! Integration coverage for at-least-once witness delivery through cursor and
//! typed-reactor handler surfaces.
//! PROVES: INV-DELIVERY-AT-LEAST-ONCE-WITNESS.

use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use batpak::coordinate::{Coordinate, Region};
use batpak::event::{EventKind, JsonValueInput, StoredEvent, TypedReactive};
use batpak::prelude::{EventPayload, Store, StoreConfig};
use batpak::store::{
    AtLeastOnce, CausationRef, CheckpointId, CursorWorkerAction, CursorWorkerConfig,
    IdempotencyKey, ObservedOnce, ReactionBatch, ReactorCanal, ReactorConfig, RestartPolicy,
};
use batpak::MultiEventReactor;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xE, type_id = 1)]
struct WitnessPayload {
    n: u64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xE, type_id = 2)]
struct WitnessOther {
    label: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xE, type_id = 3)]
struct WitnessReaction {
    source: u64,
}

#[derive(Debug)]
struct NeverFails;

impl std::fmt::Display for NeverFails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("never fails")
    }
}

impl std::error::Error for NeverFails {}

fn test_store() -> (tempfile::TempDir, Arc<Store>) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    (dir, Arc::new(store))
}

fn source_coord() -> Coordinate {
    Coordinate::new("entity:witness-source", "scope:test").expect("coordinate")
}

fn wait_for(cond: impl Fn() -> bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    cond()
}

fn worker_config(checkpoint_id: Option<CheckpointId>) -> CursorWorkerConfig {
    let mut config = CursorWorkerConfig::default();
    config.batch_size = 1;
    config.idle_sleep = Duration::from_millis(5);
    config.restart = RestartPolicy::Once;
    config.checkpoint_id = checkpoint_id;
    config
}

fn reactor_config(checkpoint_id: Option<CheckpointId>) -> ReactorConfig {
    ReactorConfig {
        batch_size: 1,
        idle_sleep: Duration::from_millis(5),
        restart_policy: RestartPolicy::Once,
        checkpoint_id,
        canal: ReactorCanal::CursorGuaranteed,
    }
}

#[test]
fn cursor_worker_with_checkpoint_id_passes_witness() {
    let (_dir, store) = test_store();
    let checkpoint_id = CheckpointId::new("witness-cursor").expect("valid checkpoint id");
    store
        .append(
            &source_coord(),
            EventKind::custom(0xE, 1),
            &serde_json::json!({"n": 1}),
        )
        .expect("append source event");

    let (tx, rx) = mpsc::channel();
    let handle = store
        .cursor_worker(
            &Region::all(),
            worker_config(Some(checkpoint_id.clone())),
            move |_batch, _store, witness| {
                tx.send(witness.map(|w| w.checkpoint_id().as_str().to_owned()))
                    .expect("send witness");
                CursorWorkerAction::Stop
            },
        )
        .expect("spawn cursor worker");

    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2)).expect("witness"),
        Some(checkpoint_id.as_str().to_owned())
    );
    handle.join().expect("worker joins");
}

#[test]
fn cursor_worker_without_checkpoint_id_passes_none() {
    let (_dir, store) = test_store();
    store
        .append(
            &source_coord(),
            EventKind::custom(0xE, 1),
            &serde_json::json!({"n": 2}),
        )
        .expect("append source event");

    let (tx, rx) = mpsc::channel();
    let handle = store
        .cursor_worker(
            &Region::all(),
            worker_config(None),
            move |_batch, _store, witness| {
                tx.send(witness.is_some()).expect("send witness presence");
                CursorWorkerAction::Stop
            },
        )
        .expect("spawn cursor worker");

    assert!(
        !rx.recv_timeout(Duration::from_secs(2))
            .expect("witness presence"),
        "ephemeral cursor workers must not mint an at-least-once witness"
    );
    handle.join().expect("worker joins");
}

struct WitnessTypedReactor {
    tx: mpsc::Sender<Option<String>>,
}

impl TypedReactive<WitnessPayload> for WitnessTypedReactor {
    type Error = NeverFails;

    fn react(
        &mut self,
        event: &StoredEvent<WitnessPayload>,
        out: &mut ReactionBatch,
        at_least_once: Option<&AtLeastOnce>,
    ) -> Result<(), Self::Error> {
        self.tx
            .send(at_least_once.map(|w| w.checkpoint_id().as_str().to_owned()))
            .expect("send typed witness");
        out.push_typed(
            source_coord(),
            &WitnessReaction {
                source: event.event.payload.n,
            },
            CausationRef::None,
        )
        .expect("push reaction");
        Ok(())
    }
}

#[test]
fn typed_reactor_handler_receives_witness() {
    let (_dir, store) = test_store();
    let (tx, rx) = mpsc::channel();
    let checkpoint_id = CheckpointId::new("witness-typed").expect("valid checkpoint id");
    let handle = store
        .react_loop_typed::<WitnessPayload, _>(
            &Region::all(),
            reactor_config(Some(checkpoint_id.clone())),
            WitnessTypedReactor { tx },
        )
        .expect("spawn typed reactor");

    store
        .append_typed(&source_coord(), &WitnessPayload { n: 3 })
        .expect("append typed source");

    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2))
            .expect("typed witness"),
        Some(checkpoint_id.as_str().to_owned())
    );
    assert!(
        wait_for(
            || store.by_fact_typed::<WitnessReaction>().len() == 1,
            Duration::from_secs(2),
        ),
        "typed reactor should commit its reaction before shutdown"
    );
    handle.stop_and_join().expect("typed reactor stops");
}

#[derive(MultiEventReactor)]
#[batpak(input = JsonValueInput, error = NeverFails)]
#[batpak(event = WitnessPayload, handler = on_payload)]
#[batpak(event = WitnessOther, handler = on_other)]
struct WitnessMultiReactor {
    tx: mpsc::Sender<Option<String>>,
}

impl WitnessMultiReactor {
    fn on_payload(
        &mut self,
        _event: &StoredEvent<WitnessPayload>,
        _out: &mut ReactionBatch,
        at_least_once: Option<&AtLeastOnce>,
    ) -> Result<(), NeverFails> {
        self.tx
            .send(at_least_once.map(|w| w.checkpoint_id().as_str().to_owned()))
            .expect("send multi witness");
        Ok(())
    }

    fn on_other(
        &mut self,
        _event: &StoredEvent<WitnessOther>,
        _out: &mut ReactionBatch,
        _at_least_once: Option<&AtLeastOnce>,
    ) -> Result<(), NeverFails> {
        Ok(())
    }
}

#[test]
fn multi_reactor_handler_receives_witness() {
    let (_dir, store) = test_store();
    let (tx, rx) = mpsc::channel();
    let checkpoint_id = CheckpointId::new("witness-multi").expect("valid checkpoint id");
    let handle = store
        .react_loop_multi(
            &Region::all(),
            reactor_config(Some(checkpoint_id.clone())),
            WitnessMultiReactor { tx },
        )
        .expect("spawn multi reactor");

    store
        .append_typed(&source_coord(), &WitnessPayload { n: 4 })
        .expect("append multi source");

    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2))
            .expect("multi witness"),
        Some(checkpoint_id.as_str().to_owned())
    );
    handle.stop_and_join().expect("multi reactor stops");
}

#[test]
fn observed_once_composes_from_handler_witness() {
    let (_dir, store) = test_store();
    let checkpoint_id = CheckpointId::new("witness-observed-once").expect("valid checkpoint id");
    store
        .append(
            &source_coord(),
            EventKind::custom(0xE, 1),
            &serde_json::json!({"n": 5}),
        )
        .expect("append source event");

    let (tx, rx) = mpsc::channel();
    let handle = store
        .cursor_worker(
            &Region::all(),
            worker_config(Some(checkpoint_id.clone())),
            move |_batch, _store, witness| {
                let witness = witness.expect("durable cursor must provide witness");
                let observed =
                    ObservedOnce::new(witness.clone(), IdempotencyKey::from_bytes([7; 32]));
                let (at_least_once, idempotency) = observed.into_parts();
                tx.send((
                    at_least_once.checkpoint_id().as_str().to_owned(),
                    *idempotency.as_bytes(),
                ))
                .expect("send observed-once parts");
                CursorWorkerAction::Stop
            },
        )
        .expect("spawn cursor worker");

    let (actual_checkpoint_id, actual_idempotency) = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("observed-once parts");
    assert_eq!(actual_checkpoint_id, checkpoint_id.as_str());
    assert_eq!(actual_idempotency, [7; 32]);
    handle.join().expect("worker joins");
}
