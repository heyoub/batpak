use crate::coordinate::Region;
use crate::store::index::{IndexEntry, StoreIndex};
use std::sync::Arc;

/// Cursor: pull-based event consumption with guaranteed delivery.
/// Reads from index, not channels. Cannot lose events.
/// [SPEC:src/store/cursor.rs]
pub struct Cursor {
    region: Region,
    position: u64, // tracks global_sequence — next poll starts after this
    started: bool, // false until first event consumed (global_sequence 0 is valid)
    index: Arc<StoreIndex>,
}

impl Cursor {
    pub(crate) fn new(region: Region, index: Arc<StoreIndex>) -> Self {
        Self {
            region,
            position: 0,
            started: false,
            index,
        }
    }

    /// Poll for the next matching event at or after our current position.
    pub fn poll(&mut self) -> Option<IndexEntry> {
        let results = self.index.query(&self.region);
        for entry in results {
            if !self.started || entry.global_sequence > self.position {
                self.position = entry.global_sequence;
                self.started = true;
                return Some(entry);
            }
        }
        None
    }

    /// Poll for up to max matching events.
    pub fn poll_batch(&mut self, max: usize) -> Vec<IndexEntry> {
        let mut batch = Vec::with_capacity(max);
        let results = self.index.query(&self.region);
        for entry in results {
            if !self.started || entry.global_sequence > self.position {
                self.position = entry.global_sequence;
                self.started = true;
                batch.push(entry);
                if batch.len() >= max {
                    break;
                }
            }
        }
        batch
    }
}
