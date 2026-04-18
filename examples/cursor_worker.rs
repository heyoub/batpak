//! # cursor_worker
//!
//! **Teaches:** cursor-based worker with ordered pull replay and observable
//! stop/join lifecycle.
//!
//! The worker drives its own lifecycle through `CursorWorkerAction::Stop`
//! returned from the handler once the observable-state condition is met;
//! the main thread waits on that observable state (`processed >= 3`) instead
//! of sleeping before calling `stop_and_join`.
//!
//! Run: `cargo run --example cursor_worker`

use batpak::prelude::*;
use batpak::store::delivery::cursor::{CursorWorkerAction, CursorWorkerConfig};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(serde::Serialize, serde::Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 6)]
struct Tick {
    n: u32,
}

// justifies: INV-EXAMPLES-OBSERVABLE-OUTPUT; example main in examples/cursor_worker.rs prints cursor-worker progress to stdout so the reader can see the observable drain.
#[allow(clippy::print_stdout)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = Arc::new(Store::open(StoreConfig::new(dir.path()))?);
    let coord = Coordinate::new("player:cursor", "room:worker")?;
    let processed = Arc::new(AtomicUsize::new(0));
    let mut worker_config = CursorWorkerConfig::default();
    worker_config.batch_size = 1;
    worker_config.idle_sleep = Duration::from_millis(5);

    let worker = store.cursor_worker(&Region::entity("player:cursor"), worker_config, {
        let processed = Arc::clone(&processed);
        move |_batch, _store| {
            let seen = processed.fetch_add(1, Ordering::SeqCst) + 1;
            if seen >= 3 {
                CursorWorkerAction::Stop
            } else {
                CursorWorkerAction::Continue
            }
        }
    })?;

    for n in 0..3u32 {
        store.append_typed(&coord, &Tick { n })?;
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    while processed.load(Ordering::SeqCst) < 3 {
        if Instant::now() >= deadline {
            return Err("cursor worker example timed out".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    worker.stop_and_join()?;
    println!(
        "cursor worker processed {} event(s) through the ordered pull path",
        processed.load(Ordering::SeqCst)
    );

    Ok(())
}
