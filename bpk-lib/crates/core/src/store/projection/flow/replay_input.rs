use crate::event::{Event, JsonValueInput, ProjectionInput, RawMsgpackInput};
use crate::store::index::DiskPos;
use crate::store::StoreError;

/// Internal projection-replay machinery. Exposed as `pub` (behind
/// `#[doc(hidden)]`) only to satisfy the public bound on
/// `Store::project` / `project_if_changed` / `watch_projection` without
/// tripping the `private_bounds` lint. External callers cannot implement
/// this trait (its `Reader` parameter is a `#[doc(hidden)]` internal
/// type) and must not rely on it being stable.
#[doc(hidden)]
pub trait ReplayInput: ProjectionInput {
    fn read_batch(
        reader: &crate::store::segment::scan::Reader,
        positions: &[&DiskPos],
    ) -> Result<Vec<Event<Self::Payload>>, StoreError>;

    fn read_one(
        reader: &crate::store::segment::scan::Reader,
        pos: &DiskPos,
    ) -> Result<Event<Self::Payload>, StoreError>;
}

impl ReplayInput for JsonValueInput {
    fn read_batch(
        reader: &crate::store::segment::scan::Reader,
        positions: &[&DiskPos],
    ) -> Result<Vec<Event<Self::Payload>>, StoreError> {
        reader.read_events_batch(positions)
    }

    fn read_one(
        reader: &crate::store::segment::scan::Reader,
        pos: &DiskPos,
    ) -> Result<Event<Self::Payload>, StoreError> {
        reader.read_event_only(pos)
    }
}

impl ReplayInput for RawMsgpackInput {
    fn read_batch(
        reader: &crate::store::segment::scan::Reader,
        positions: &[&DiskPos],
    ) -> Result<Vec<Event<Self::Payload>>, StoreError> {
        reader.read_raw_events_batch(positions)
    }

    fn read_one(
        reader: &crate::store::segment::scan::Reader,
        pos: &DiskPos,
    ) -> Result<Event<Self::Payload>, StoreError> {
        reader.read_event_raw_only(pos)
    }
}
