//! Proof: the active-segment positioned frame read (`read_exact_at`) is now
//! routed through [`StoreFs`], so a [`SimFs`] fault SURFACES on the FD/pread read
//! path — where the same read, as a direct `platform::fs::read_exact_at` free fn,
//! was unfaultable.
//!
//! Each test pairs a CONTROL (an honest [`SimFs`] serves the real bytes, so the
//! read succeeds) with a FAULT armed on the SAME store (the routed read now
//! returns the injected [`super::fs::ReadFaultKind`]). The contrast is the
//! evidence: before routing, `read_exact_at` took no fs handle, so no backend
//! could intercept the read — the CONTROL and FAULT reads were byte-identical and
//! this fixture could not have been written. Because a read fault never touches
//! the real file, the CONTROL and FAULT reads can share one store: the faulted
//! read leaves the segment intact for the next arming.
//!
//! The three injected kinds map onto the three shapes the reader's active-frame
//! read (`Reader::read_active_frame_into`) already distinguishes:
//!
//!   * `ShortRead { bytes_read: 0 }` — an EOF at the frame boundary ⇒
//!     [`StoreError::corrupt_eof`] (a `CorruptSegment` with the EOF detail).
//!   * `ShortRead { bytes_read: n>0 }` — a torn/partial frame ⇒ a `CorruptSegment`
//!     with the "ended before requested length" detail.
//!   * `Io` — a hard positioned-read error ⇒ [`StoreError::Io`].

use super::fs::{ReadFaultKind, SimFs};
use crate::coordinate::Coordinate;
use crate::event::EventKind;
use crate::id::EventId;
use crate::store::platform::fs::StoreFs;
use crate::store::{Store, StoreConfig, StoreError};
use std::sync::Arc;

/// User-visible event kind the fixture appends (category 0xC is a free custom
/// range; 0xD is reserved for effect kinds).
const KIND: EventKind = EventKind::custom(0xC, 0x5);

/// Open a real `Store` over `sim_fs` and append one event to the ACTIVE segment,
/// returning the store and the appended event's id. `Store::get` reads that
/// frame back through the FD/pread path (the active segment is never sealed), so
/// it terminates in the routed `read_exact_at` seam.
fn open_with_one_event(dir: &std::path::Path, sim_fs: &Arc<SimFs>) -> (Store, EventId) {
    let config = StoreConfig::new(dir).with_fs(Arc::clone(sim_fs) as Arc<dyn StoreFs>);
    let store = Store::open(config).expect("open store over SimFs");
    let coord = Coordinate::new("entity:read-fault", "scope:read-fault").expect("coordinate");
    let receipt = store
        .append(&coord, KIND, &serde_json::json!({ "n": 1 }))
        .expect("append one event to the active segment");
    (store, receipt.event_id)
}

#[test]
fn read_fault_short_read_zero_surfaces_corrupt_eof() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let sim_fs = Arc::new(SimFs::new(0x0EAD_0001, 0));
    let (store, event_id) = open_with_one_event(dir.path(), &sim_fs);

    // CONTROL: the honest SimFs serves the active-segment frame read.
    store
        .get(event_id)
        .expect("PROPERTY: an honest active-segment read must succeed (control)");

    // FAULT: a zero-length short read on the next positioned read models an EOF
    // exactly at the frame boundary.
    sim_fs.arm_read_fault_on(1, ReadFaultKind::ShortRead { bytes_read: 0 });
    let err = store
        .get(event_id)
        .expect_err("a torn active-segment read must surface an error");
    assert!(
        matches!(&err, StoreError::CorruptSegment { detail, .. } if detail.contains("unexpected EOF")),
        "PROPERTY: a routed read faulted with ShortRead{{0}} must map to corrupt_eof, got {err:?}"
    );
}

#[test]
fn read_fault_partial_short_read_surfaces_corrupt_segment() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let sim_fs = Arc::new(SimFs::new(0x0EAD_0002, 0));
    let (store, event_id) = open_with_one_event(dir.path(), &sim_fs);

    store
        .get(event_id)
        .expect("PROPERTY: an honest active-segment read must succeed (control)");

    // FAULT: a non-zero short read models a frame whose tail was torn away.
    sim_fs.arm_read_fault_on(1, ReadFaultKind::ShortRead { bytes_read: 4 });
    let err = store
        .get(event_id)
        .expect_err("a partial active-segment read must surface an error");
    assert!(
        matches!(
            &err,
            StoreError::CorruptSegment { detail, .. } if detail.contains("ended before requested length")
        ),
        "PROPERTY: a routed read faulted with a non-zero ShortRead must map to a corrupt-segment error, got {err:?}"
    );
}

#[test]
fn read_fault_io_surfaces_store_io() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let sim_fs = Arc::new(SimFs::new(0x0EAD_0003, 0));
    let (store, event_id) = open_with_one_event(dir.path(), &sim_fs);

    store
        .get(event_id)
        .expect("PROPERTY: an honest active-segment read must succeed (control)");

    // FAULT: a hard positioned-read error.
    sim_fs.arm_read_fault_on(1, ReadFaultKind::Io);
    let err = store
        .get(event_id)
        .expect_err("a faulted active-segment read must surface an error");
    assert!(
        matches!(&err, StoreError::Io(_)),
        "PROPERTY: a routed read faulted with Io must surface as StoreError::Io, got {err:?}"
    );
}

#[test]
fn read_fault_is_one_shot_on_the_targeted_occurrence() {
    // Build the SimFs with the fault PRE-ARMED via the builder form (mirrors
    // `SimFs::with_fault_on`): store open + the single append perform no
    // active-segment reads, so the first `get` is the targeted occurrence. The
    // schedule then stops, so the store stays usable — pinning the targeted-Nth
    // semantics (distinct from a sticky fault) that lets CONTROL and FAULT share
    // one store in the tests above.
    let dir = tempfile::tempdir().expect("tmpdir");
    let sim_fs = Arc::new(SimFs::new(0x0EAD_0004, 0).with_read_fault_on(1, ReadFaultKind::Io));
    let (store, event_id) = open_with_one_event(dir.path(), &sim_fs);

    let err = store
        .get(event_id)
        .expect_err("the first read must fault (the targeted occurrence)");
    assert!(
        matches!(&err, StoreError::Io(_)),
        "PROPERTY: the pre-armed Io read fault must surface as StoreError::Io, got {err:?}"
    );
    store.get(event_id).expect(
        "PROPERTY: a read fault targets exactly one occurrence; the next read must succeed",
    );
}
