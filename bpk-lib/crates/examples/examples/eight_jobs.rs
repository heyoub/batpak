//! # eight_jobs
//!
//! **Teaches:** the canonical BatPAK store path for 0.8.
//!
//! This example keeps `batpak::prelude::*` honest: it uses only the beginner
//! substrate jobs and leaves pipelines, reactors, delivery cursors, cache
//! backends, and evidence reports to explicit advanced examples.
//!
//! Run: `cargo run --example eight_jobs`

use batpak::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, EventPayload)]
#[batpak(category = 0xF, type_id = 0x201)]
struct NoteAdded {
    body: String,
}

#[derive(Debug, Default, Serialize, Deserialize, EventSourced)]
#[batpak(input = JsonValueInput, cache_version = 0)]
#[batpak(event = NoteAdded, handler = on_note_added)]
struct NoteStream {
    count: usize,
    last_body: Option<String>,
}

impl NoteStream {
    fn on_note_added(&mut self, event: &NoteAdded) {
        self.count += 1;
        self.last_body = Some(event.body.clone());
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();

    let dir = tempfile::tempdir()?;

    // 1. Open.
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let coord = Coordinate::new("entity:eight-jobs", "scope:eight-jobs")?;

    // 2. Append typed events.
    let first = store.append_typed(
        &coord,
        &NoteAdded {
            body: "first note".into(),
        },
    )?;
    let second = store.append_typed(
        &coord,
        &NoteAdded {
            body: "second note".into(),
        },
    )?;

    // 3. Query commit-order pages by Region and after_global_sequence.
    let region = Region::scope("scope:eight-jobs");
    let page = store.query_entries_after(&region, None, 16);
    assert_eq!(page.len(), 2);
    assert_eq!(page[0].event_id(), first.event_id);

    // 4. Get a point-read payload and decode it through the typed registry.
    let stored = store.get(page[0].event_id())?;
    let decoded: NoteAdded = stored.event.decode_typed()?;
    assert_eq!(decoded.body, "first note");

    // 5. Walk bounded hash-chain ancestry.
    let ancestors = store.walk_ancestors(second.event_id, 8);
    assert!(ancestors.len() >= 2);

    // 6. Verify a native append receipt with detailed proof language.
    let verification = store.verify_append_receipt(&second);
    assert!(verification.is_valid());

    // 7. Project derived state from committed history.
    let Some(projected) =
        store.project::<NoteStream>("entity:eight-jobs", &Freshness::Consistent)?
    else {
        return Err(std::io::Error::other("projection should exist after appends").into());
    };
    assert_eq!(projected.count, 2);

    // 8. Close.
    store.close()?;

    let _ = writeln!(
        out,
        "eight jobs ok: page={} ancestors={} receipt={verification:?}",
        page.len(),
        ancestors.len()
    );
    Ok(())
}
