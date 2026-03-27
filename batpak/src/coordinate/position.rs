use serde::{Deserialize, Serialize};
use std::fmt;

/// DagPosition: graph position with hybrid logical clock + depth + lane + sequence.
/// wall_ms + counter provide global causal ordering (HLC-style) across entities.
/// depth/lane/sequence provide per-entity chain ordering.
/// v1: depth=0, lane=0 always. Sequence is per-entity monotonic counter.
/// Lane/depth vocabulary is scaffolding for future distributed fan-out.
/// [SPEC:src/coordinate/position.rs]
/// [CROSS-POLLINATION:czap/hlc.ts — HLC adds wall-clock causality to event ordering]
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DagPosition {
    /// Wall-clock milliseconds at event creation. HLC layer 1.
    /// Enables global causal ordering across entities (not just per-entity sequence).
    pub wall_ms: u64,
    /// HLC counter for same-millisecond tiebreaking. Incremented when wall_ms hasn't advanced.
    pub counter: u16,
    pub depth: u32,
    pub lane: u32,
    pub sequence: u32,
}

impl DagPosition {
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

    pub const fn root() -> Self {
        Self {
            wall_ms: 0,
            counter: 0,
            depth: 0,
            lane: 0,
            sequence: 0,
        }
    }

    /// v1: always depth=0, lane=0, sequence=N. wall_ms set by writer.
    pub const fn child(sequence: u32) -> Self {
        Self {
            wall_ms: 0,
            counter: 0,
            depth: 0,
            lane: 0,
            sequence,
        }
    }

    /// v1 with HLC: same as child but with wall clock context.
    pub const fn child_at(sequence: u32, wall_ms: u64, counter: u16) -> Self {
        Self {
            wall_ms,
            counter,
            depth: 0,
            lane: 0,
            sequence,
        }
    }

    /// Future: fork creates a new lane at depth+1
    pub const fn fork(parent_depth: u32, new_lane: u32) -> Self {
        Self {
            wall_ms: 0,
            counter: 0,
            depth: parent_depth + 1,
            lane: new_lane,
            sequence: 0,
        }
    }

    pub const fn is_root(&self) -> bool {
        self.depth == 0 && self.lane == 0 && self.sequence == 0
    }

    /// Causal ordering: ancestor if same lane, same depth, and lower sequence.
    /// v1: depth is always 0, lane always 0, so just compare sequence.
    /// DAG-ready: different depths means different branches — not ancestor.
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
/// [SPEC:src/coordinate/position.rs — PartialOrd]
impl PartialOrd for DagPosition {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        if self.lane != other.lane || self.depth != other.depth {
            return None; // different lanes or depths are incomparable
        }
        Some(self.sequence.cmp(&other.sequence))
    }
}
