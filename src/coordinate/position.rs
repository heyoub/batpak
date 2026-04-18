use serde::{Deserialize, Serialize};
use std::fmt;

/// DagPosition: graph position with hybrid logical clock + depth + lane + sequence.
/// wall_ms + counter provide global causal ordering (HLC-style) across entities.
/// depth/lane/sequence provide per-entity chain ordering.
/// Early releases always committed depth=0/lane=0. Current append-position hints
/// may supply non-root depth/lane, while the writer still owns HLC fields and
/// the per-entity sequence counter.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(into = "DagPositionWire")]
pub struct DagPosition {
    /// Wall-clock milliseconds at event creation. HLC layer 1.
    /// Enables global causal ordering across entities (not just per-entity sequence).
    pub(crate) wall_ms: u64,
    /// HLC counter for same-millisecond tiebreaking. Incremented when wall_ms hasn't advanced.
    pub(crate) counter: u16,
    /// DAG depth level. Root appends default to 0; append-position hints may set non-root depth.
    pub(crate) depth: u32,
    /// Parallel branch index. Root appends default to 0; append-position hints may set non-root lane.
    pub(crate) lane: u32,
    /// Per-entity monotonic event counter within this lane and depth.
    pub(crate) sequence: u32,
}

/// Serde intermediate that mirrors the on-wire layout of [`DagPosition`].
/// All construction routes through [`DagPosition::with_hlc`] so any future
/// invariants land in one place.
#[derive(Serialize, Deserialize)]
struct DagPositionWire {
    wall_ms: u64,
    counter: u16,
    depth: u32,
    lane: u32,
    sequence: u32,
}

impl From<DagPosition> for DagPositionWire {
    fn from(pos: DagPosition) -> Self {
        Self {
            wall_ms: pos.wall_ms,
            counter: pos.counter,
            depth: pos.depth,
            lane: pos.lane,
            sequence: pos.sequence,
        }
    }
}

impl From<DagPositionWire> for DagPosition {
    fn from(wire: DagPositionWire) -> Self {
        Self::with_hlc(
            wire.wall_ms,
            wire.counter,
            wire.depth,
            wire.lane,
            wire.sequence,
        )
    }
}

impl<'de> Deserialize<'de> for DagPosition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        DagPositionWire::deserialize(deserializer).map(DagPosition::from)
    }
}

impl DagPosition {
    /// Creates a new position with the given depth, lane, and sequence; wall clock fields zeroed.
    pub const fn new(depth: u32, lane: u32, sequence: u32) -> Self {
        Self {
            wall_ms: 0,
            counter: 0,
            depth,
            lane,
            sequence,
        }
    }

    /// Full constructor with HLC fields.
    pub const fn with_hlc(
        wall_ms: u64,
        counter: u16,
        depth: u32,
        lane: u32,
        sequence: u32,
    ) -> Self {
        Self {
            wall_ms,
            counter,
            depth,
            lane,
            sequence,
        }
    }

    /// Returns the genesis position: depth 0, lane 0, sequence 0.
    pub const fn root() -> Self {
        Self {
            wall_ms: 0,
            counter: 0,
            depth: 0,
            lane: 0,
            sequence: 0,
        }
    }

    /// Root-lane child: depth=0, lane=0, sequence=N. wall_ms set by writer when committed.
    pub const fn child(sequence: u32) -> Self {
        Self {
            wall_ms: 0,
            counter: 0,
            depth: 0,
            lane: 0,
            sequence,
        }
    }

    /// Root-lane child with HLC context supplied by the writer.
    pub const fn child_at(sequence: u32, wall_ms: u64, counter: u16) -> Self {
        Self {
            wall_ms,
            counter,
            depth: 0,
            lane: 0,
            sequence,
        }
    }

    /// Construct a new branch root at `depth + 1` on the given lane.
    pub const fn fork(parent_depth: u32, new_lane: u32) -> Self {
        Self {
            wall_ms: 0,
            counter: 0,
            depth: parent_depth + 1,
            lane: new_lane,
            sequence: 0,
        }
    }

    /// Returns the wall-clock milliseconds timestamp (HLC layer 1).
    pub const fn wall_ms(&self) -> u64 {
        self.wall_ms
    }

    /// Returns the HLC counter (same-millisecond tiebreak).
    pub const fn counter(&self) -> u16 {
        self.counter
    }

    /// Returns the DAG depth.
    pub const fn depth(&self) -> u32 {
        self.depth
    }

    /// Returns the DAG lane index.
    pub const fn lane(&self) -> u32 {
        self.lane
    }

    /// Returns the per-entity sequence number.
    pub const fn sequence(&self) -> u32 {
        self.sequence
    }

    /// Returns `true` if this is the root position (depth 0, lane 0, sequence 0).
    pub const fn is_root(&self) -> bool {
        self.depth == 0 && self.lane == 0 && self.sequence == 0
    }

    /// Causal ordering: ancestor if same lane, same depth, and lower sequence.
    /// Different lanes or depths are different branches and are not comparable.
    pub const fn is_ancestor_of(&self, other: &DagPosition) -> bool {
        self.lane == other.lane && self.depth == other.depth && self.sequence < other.sequence
    }
}

impl fmt::Display for DagPosition {
    /// "depth:lane:sequence@wall_ms.counter"
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}@{}.{}",
            self.depth, self.lane, self.sequence, self.wall_ms, self.counter
        )
    }
}

/// PartialOrd for causal ordering — not total because different lanes
/// are incomparable, and different depths are incomparable.
impl PartialOrd for DagPosition {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        if self.lane != other.lane || self.depth != other.depth {
            return None; // different lanes or depths are incomparable
        }
        let sequence_order = self.sequence.cmp(&other.sequence);
        if sequence_order != std::cmp::Ordering::Equal {
            return Some(sequence_order);
        }
        if self.wall_ms == other.wall_ms && self.counter == other.counter {
            return Some(std::cmp::Ordering::Equal);
        }
        None
    }
}
