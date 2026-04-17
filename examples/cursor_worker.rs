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

#[allow(clippy::print_stdout)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = Arc::new(Store::open(StoreConfig::new(dir.path()))?);
    let coord = Coordinate::new("player:cursor", "room:worker")?;
    let processed = Arc::new(AtomicUsize::new(0));

    let worker = store.cursor_worker(
        &Region::entity("player:cursor"),
        CursorWorkerConfig {
            batch_size: 1,
            idle_sleep: Duration::from_millis(5),
            ..CursorWorkerConfig::default()
        },
        {
            let processed = Arc::clone(&processed);
            move |_batch, _store| {
                let seen = processed.fetch_add(1, Ordering::SeqCst) + 1;
                if seen >= 3 {
                    CursorWorkerAction::Stop
                } else {
                    CursorWorkerAction::Continue
                }
            }
        },
    )?;

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

    worker.join()?;
    println!(
        "cursor worker processed {} event(s) through the guaranteed-delivery path",
        processed.load(Ordering::SeqCst)
    );

    Ok(())
}
