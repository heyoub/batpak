//! Cooperative single-thread scheduler.
//!
//! [`SimScheduler`] implements [`Spawn`] without ever touching an OS thread.
//! Each `spawn` enqueues the body (a `FnOnce`) on a shared FIFO and hands back a
//! [`SimJoin`] handle bound to a slot. Bodies execute on the *calling* thread
//! when the queue is drained — either explicitly via [`SimScheduler::run_all`]
//! or implicitly when a handle's [`SimJoin::join`] is called, which drains until
//! its slot is finished.
//!
//! Determinism: because there is exactly one execution thread and a single FIFO
//! drained in enqueue order, the interleaving is a pure function of the order
//! in which `spawn` is called. No wall-clock, no OS scheduler, no data races.
//!
//! Panic contract: a panicking body is caught with
//! [`std::panic::catch_unwind`] and recorded as a failed slot, so
//! [`SimJoin::join`] returns `Err` exactly like
//! [`std::thread::JoinHandle::join`].
//!
//! Shared state lives behind an internal `Arc<Shared>`, so the bare-`&self`
//! [`Spawn::spawn`] method can still mint a self-draining join handle (the
//! handle clones the `Arc`). That is what lets `SimScheduler` be installed on a
//! `StoreConfig` via `with_spawner` and produce working joins.

use crate::store::platform::spawn::{SimJoin, SimJoinResult, Spawn};
use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};

/// Terminal state of a spawned body.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SlotState {
    /// Enqueued but not yet executed.
    Pending,
    /// Ran to completion without unwinding.
    Done,
    /// Unwound (panicked) during execution.
    Panicked,
}

/// One queued unit of work plus its terminal state.
struct Slot {
    /// The body to run, taken out (`Option::take`) when it executes.
    body: Option<Box<dyn FnOnce() + Send + 'static>>,
    /// Outcome once executed.
    state: SlotState,
}

/// Interior scheduler state shared by every handle.
///
/// State lives behind [`Mutex`]es so the type is legitimately `Send + Sync`
/// (required by the [`Spawn`] supertrait) without any `unsafe`. The simulation
/// drives it single-threaded, so the locks are always uncontended; they exist
/// for soundness, not for parallelism. A body never runs *while holding* a lock
/// (bodies are taken out first), so a panicking body cannot poison a lock; the
/// `unwrap_or_else(PoisonError::into_inner)` recovery is therefore unreachable
/// in practice and never panics regardless.
#[derive(Default)]
struct Shared {
    slots: Mutex<Vec<Slot>>,
    queue: Mutex<VecDeque<usize>>,
}

impl Shared {
    /// Enqueue `body`, returning its dense slot id.
    fn enqueue(&self, body: Box<dyn FnOnce() + Send + 'static>) -> usize {
        let mut slots = self
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = slots.len();
        slots.push(Slot {
            body: Some(body),
            state: SlotState::Pending,
        });
        drop(slots);
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(id);
        id
    }

    /// Pop the next pending slot id, or `None` when the queue is empty.
    fn next_pending(&self) -> Option<usize> {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
    }

    /// Execute the body for `id`, recording its terminal state. The body is
    /// taken out under a short lock so it may itself spawn more work without
    /// re-entering a held lock.
    fn run_slot(&self, id: usize) {
        let body = self
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)[id]
            .body
            .take();
        let Some(body) = body else {
            return;
        };
        let result = std::panic::catch_unwind(AssertUnwindSafe(body));
        self.slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)[id]
            .state = if result.is_ok() {
            SlotState::Done
        } else {
            SlotState::Panicked
        };
    }

    /// Whether slot `id` has reached a terminal state.
    fn is_finished(&self, id: usize) -> bool {
        matches!(
            self.slots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)[id]
                .state,
            SlotState::Done | SlotState::Panicked
        )
    }

    /// Drain until slot `id` is finished, then map its state to a join result.
    fn join_slot(&self, id: usize) -> SimJoinResult {
        while !self.is_finished(id) {
            match self.next_pending() {
                Some(next) => self.run_slot(next),
                None => self.run_slot(id),
            }
        }
        let state = self
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)[id]
            .state;
        match state {
            SlotState::Done | SlotState::Pending => Ok(()),
            // Opaque payload; callers only inspect Err-ness, matching
            // std::thread::JoinHandle::join's Box<dyn Any> contract.
            SlotState::Panicked => {
                Err(Box::new("sim body panicked") as Box<dyn std::any::Any + Send>)
            }
        }
    }
}

/// Cooperative scheduler shared behind `Arc<dyn Spawn>`.
#[derive(Default)]
pub(crate) struct SimScheduler {
    shared: Arc<Shared>,
}

impl SimScheduler {
    /// Create an empty cooperative scheduler.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Drain the FIFO, executing every currently-pending body in enqueue order.
    /// Bodies spawned by a running body are appended and run in the same pass.
    pub(crate) fn run_all(&self) {
        while let Some(id) = self.shared.next_pending() {
            self.shared.run_slot(id);
        }
    }

    /// Spawn `body` and return a self-draining join handle.
    ///
    /// Identical semantics to [`Spawn::spawn`]; provided as an inherent method
    /// so callers that already hold a `&SimScheduler` get the concrete handle
    /// without a name/stack_size ceremony.
    pub(crate) fn spawn_owned(&self, body: Box<dyn FnOnce() + Send + 'static>) -> Box<dyn SimJoin> {
        let id = self.shared.enqueue(body);
        Box::new(SimJoinHandle {
            shared: Arc::clone(&self.shared),
            id,
        })
    }
}

/// Join handle bound to a scheduler slot; holds a clone of the shared state so
/// it can drain the queue on `join` regardless of how it was minted.
struct SimJoinHandle {
    shared: Arc<Shared>,
    id: usize,
}

impl SimJoin for SimJoinHandle {
    fn join(self: Box<Self>) -> SimJoinResult {
        self.shared.join_slot(self.id)
    }

    fn is_finished(&self) -> bool {
        self.shared.is_finished(self.id)
    }
}

impl Spawn for SimScheduler {
    fn spawn(
        &self,
        _name: String,
        _stack_size: Option<usize>,
        body: Box<dyn FnOnce() + Send + 'static>,
    ) -> std::io::Result<Box<dyn SimJoin>> {
        Ok(self.spawn_owned(body))
    }
}

#[cfg(test)]
mod tests {
    // justifies: INV-TEST-PANIC-AS-ASSERTION; sim scheduler proof bodies panic on purpose to prove SimJoin::join surfaces unwinds as Err.
    #![allow(clippy::panic)]
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn run_all_executes_in_enqueue_order() {
        let sched = SimScheduler::new();
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        for i in 0..5usize {
            let log = Arc::clone(&log);
            sched
                .spawn(
                    "t".to_string(),
                    None,
                    Box::new(move || log.lock().expect("test log lock").push(i)),
                )
                .expect("sim spawn cannot fail");
        }
        sched.run_all();
        assert_eq!(
            *log.lock().expect("test log lock"),
            vec![0, 1, 2, 3, 4],
            "PROPERTY: cooperative scheduler runs bodies in deterministic enqueue order"
        );
    }

    #[test]
    fn spawn_join_drains_and_returns_ok() {
        let sched = SimScheduler::new();
        let ran = Arc::new(AtomicUsize::new(0));
        let ran_body = Arc::clone(&ran);
        let handle = sched
            .spawn(
                "owned".to_string(),
                None,
                Box::new(move || {
                    ran_body.fetch_add(1, Ordering::SeqCst);
                }),
            )
            .expect("sim spawn cannot fail");
        handle.join().expect("clean body joins Ok");
        assert_eq!(
            ran.load(Ordering::SeqCst),
            1,
            "PROPERTY: join drains the cooperative queue until the body completes"
        );
    }

    #[test]
    fn spawn_join_surfaces_panic_as_err() {
        let sched = SimScheduler::new();
        let handle = sched.spawn_owned(Box::new(|| panic!("intentional sim panic proof")));
        assert!(
            handle.join().is_err(),
            "PROPERTY: a panicking body surfaces through SimJoin::join as Err, matching std::thread"
        );
    }
}
