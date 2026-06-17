//! Shared non-test helpers for the `cursor_durability*` harness family.
//!
//! Every split binary in the family (`cursor_durability` checkpoint-corruption
//! and `cursor_durability_progress` delivery-progress) uses each item here, so
//! none of them goes dead in any binary. Family-specific helpers
//! (`wait_until`, `assert_checkpoint_position`) live inline in their one
//! binary instead.

use batpak::event::EventKind;
use batpak::store::{CheckpointId, StoreConfig};
use std::path::PathBuf;
use tempfile::TempDir;

pub const KIND: EventKind = EventKind::custom(0xA, 1);

pub fn config(dir: &TempDir) -> StoreConfig {
    StoreConfig::new(dir.path())
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false)
        .with_sync_every_n_events(1)
}

pub fn valid_checkpoint_id(id: &str) -> CheckpointId {
    CheckpointId::new(id).expect("valid checkpoint id")
}

pub struct StrayCheckpointGuard {
    path: PathBuf,
}

impl StrayCheckpointGuard {
    pub fn new(id: &str) -> Self {
        assert!(
            id.starts_with("batpak-test-"),
            "test-owned checkpoint ids must stay namespaced, got `{id}`"
        );
        let path = std::env::current_dir()
            .expect("current dir")
            .join(format!("{id}.ckpt"));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&path);
        Self { path }
    }

    pub fn assert_absent(&self) {
        assert!(
            !self.path.exists(),
            "PROPERTY: durable checkpoint writes must stay under the store data dir, not leak to {}",
            self.path.display()
        );
    }
}

impl Drop for StrayCheckpointGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
