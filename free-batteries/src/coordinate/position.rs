use serde::{Deserialize, Serialize};
use std::fmt;

/// DagPosition: graph position with depth + lane + sequence.
/// v1: depth=0, lane=0 always. Sequence is per-entity monotonic counter.
/// Lane/depth vocabulary is scaffolding for future distributed fan-out.
/// [SPEC:src/coordinate/position.rs]
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DagPosition {
    pub depth: u32,
    pub lane: u32,
    pub sequence: u32,
}

impl DagPosition {
    pub const fn new(depth: u32, lane: u32, sequence: u32) -> Self {
        Self {
            depth,
            lane,
            sequence,
        }
    }

    pub const fn root() -> Self {
        Self {
            depth: 0,
            lane: 0,
            sequence: 0,
        }
    }

    /// v1: always depth=0, lane=0, sequence=N
    pub const fn child(sequence: u32) -> Self {
        Self {
            depth: 0,
            lane: 0,
            sequence,
        }
    }

    /// Future: fork creates a new lane at depth+1
    pub const fn fork(parent_depth: u32, new_lane: u32) -> Self {
        Self {
            depth: parent_depth + 1,
            lane: new_lane,
            sequence: 0,
        }
    }

    pub const fn is_root(&self) -> bool {
        self.depth == 0 && self.lane == 0 && self.sequence == 0
    }

    /// Causal ordering: ancestor if same lane and lower depth+sequence.
    /// v1: same lane always (lane=0), so just compare sequence.
    pub const fn is_ancestor_of(&self, other: &DagPosition) -> bool {
        self.lane == other.lane && self.depth <= other.depth && self.sequence < other.sequence
    }
}

impl fmt::Display for DagPosition {
    /// "depth:lane:sequence"
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.depth, self.lane, self.sequence)
    }
}

/// PartialOrd for causal ordering — not total because different lanes
/// are incomparable. [SPEC:src/coordinate/position.rs — PartialOrd]
impl PartialOrd for DagPosition {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        if self.lane != other.lane {
            return None; // different lanes are incomparable
        }
        Some(self.sequence.cmp(&other.sequence))
    }
}
