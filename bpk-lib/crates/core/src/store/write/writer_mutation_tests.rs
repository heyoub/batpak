//! Targeted tests for the `WriterHandle` liveness/quiescence seam.
//!
//! Split out of `writer.rs` via `#[path]` so the production file stays under the
//! 850-nonblank cap; this is the `#[cfg(test)] mod mutation_kill_tests` body.

use super::{
    ReactorSubscriberList, SubscriberList, WatermarkState, WriterCommand, WriterDrive, WriterHandle,
};
use crate::store::platform::spawn::{JobHandle, JoinError};
use crate::store::{StoreError, SystemClock};
use std::sync::Arc;

/// A `JobHandle` whose `is_finished` is fixed, so the writer's crash detector
/// can be exercised without racing a real thread.
struct FixedJob {
    finished: bool,
}

impl JobHandle for FixedJob {
    fn join(self: Box<Self>) -> Result<(), JoinError> {
        Ok(())
    }
    fn is_finished(&self) -> bool {
        self.finished
    }
}

fn handle_with_job(finished: bool) -> WriterHandle {
    let (tx, _rx) = flume::bounded::<WriterCommand>(1);
    WriterHandle {
        tx,
        subscribers: Arc::new(SubscriberList::new()),
        reactor_subscribers: Arc::new(ReactorSubscriberList::new()),
        watermark_handle: WatermarkState::handle(Arc::new(SystemClock::new())),
        drive: WriterDrive::Threaded {
            thread: Some(Box::new(FixedJob { finished })),
        },
    }
}

#[test]
fn fail_if_exited_reports_crash_only_when_the_writer_thread_has_finished() {
    // `fail_if_exited -> Ok(())` would never report a crash. The real method
    // must return `WriterCrashed` for a finished thread and `Ok` for a live one.
    let mut failures: Vec<String> = Vec::new();

    let finished = handle_with_job(true);
    if !matches!(finished.fail_if_exited(), Err(StoreError::WriterCrashed)) {
        failures.push("a finished writer thread must surface WriterCrashed".into());
    }

    let running = handle_with_job(false);
    if running.fail_if_exited().is_err() {
        failures.push("a still-running writer thread must report Ok".into());
    }

    assert!(
        failures.is_empty(),
        "fail_if_exited mismatches: {failures:?}"
    );
}

#[cfg(feature = "dangerous-test-hooks")]
#[test]
fn close_channel_and_join_drops_the_live_command_sender() {
    // `close_channel_and_join -> ()` would leave the original command sender
    // alive. The real method replaces it with a dead sender and drops the
    // live one, so the writer's receiver disconnects.
    let (tx, rx) = flume::bounded::<WriterCommand>(4);
    let mut handle = WriterHandle::from_parts_for_test(tx, Arc::new(SubscriberList::new()));

    assert!(
        !rx.is_disconnected(),
        "precondition: the receiver is connected before close"
    );
    handle.close_channel_and_join();
    assert!(
        rx.is_disconnected(),
        "close_channel_and_join must drop the live command sender so the writer rx disconnects"
    );
}
