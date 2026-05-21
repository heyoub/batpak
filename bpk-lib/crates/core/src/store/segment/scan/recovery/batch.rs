use crate::store::segment::scan::ScannedIndexEntry;

#[derive(Default)]
pub(crate) struct BatchRecoveryState {
    pub staged: Vec<ScannedIndexEntry>,
    pub remaining: u32,
    pub started_count: u32,
    pub in_batch: bool,
}

impl BatchRecoveryState {
    pub(super) fn stage_entry(&mut self, entry: ScannedIndexEntry) -> bool {
        if self.remaining == 0 {
            return false;
        }
        self.staged.push(entry);
        self.remaining -= 1;
        true
    }

    pub(super) fn discard_incomplete(&mut self) {
        self.in_batch = false;
        self.remaining = 0;
        self.started_count = 0;
        self.staged.clear();
    }
}
