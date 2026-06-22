//! dm-flakey batch proofs for `INV-FRONTIER-DURABLE-COVERS-RECOVERED`.
//!
//! These scenarios are the batch analog of `single_append_written.rs`.
//! They use a real Linux device-mapper failure boundary and assert Meaning-2
//! durable frontier semantics: on recovery, `durable_hlc` covers whatever
//! events the OS and filesystem preserved, and it remains monotonic across
//! the crash boundary. They deliberately do not assert all-or-nothing recovery
//! for unsynced batches; ext4 write-back and page-cache timing are outside
//! batpak's contract and are recorded in
//! `OBS-DURABLE-HLC-INCLUDES-OS-PRESERVED-DATA`.

use crate::chaos::dm_flakey::FlakeyDevice;
use batpak::id::EntityIdType;
use batpak::prelude::{Coordinate, EventKind, Region};
use batpak::store::{
    AppendOptions, AppendReceipt, BatchAppendItem, CausationRef, HlcPoint, Store, StoreConfig,
    StoreError,
};
use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const DEVICE_SIZE_BYTES: u64 = 64 * 1024 * 1024;
const BATCH_TORN_TAIL_SCOPE: &str = "scope:frontier-batch-torn-tail";

fn chaos_enabled() -> bool {
    std::env::var_os("BATPAK_RUN_CHAOS").is_some()
}

fn coord(entity: &str) -> Coordinate {
    Coordinate::new(entity, BATCH_TORN_TAIL_SCOPE).expect("valid batch torn-tail coordinate")
}

fn kind() -> EventKind {
    EventKind::custom(0xF, 0x93)
}

fn point(entry: &batpak::store::index::IndexEntry) -> HlcPoint {
    HlcPoint {
        wall_ms: entry.wall_ms(),
        global_sequence: entry.global_sequence(),
    }
}

fn backing_path(temp: &TempDir) -> PathBuf {
    temp.path().join("flakey-backing.img")
}

fn open_store_on_device(device: &FlakeyDevice, sync_every_n_events: u32) -> Store {
    std::fs::create_dir_all(device.data_dir()).expect("create store data dir");
    Store::open(StoreConfig::new(device.data_dir()).with_sync_every_n_events(sync_every_n_events))
        .expect("open store on flakey device")
}

fn batch_items(prefix: &str, count: usize) -> Vec<BatchAppendItem> {
    (0..count)
        .map(|idx| {
            BatchAppendItem::new(
                coord(&format!("entity:{prefix}:batch-{idx}")),
                kind(),
                &serde_json::json!({ "batch": prefix, "idx": idx }),
                AppendOptions::default(),
                CausationRef::None,
            )
            .expect("construct batch item")
        })
        .collect()
}

fn append_single(store: &Store, entity: &str, value: u32) -> AppendReceipt {
    store
        .append(
            &coord(entity),
            kind(),
            &serde_json::json!({ "value": value }),
        )
        .expect("append single event")
}

fn append_batch(store: &Store, prefix: &str, count: usize) -> Vec<AppendReceipt> {
    store
        .append_batch(batch_items(prefix, count))
        .expect("append batch")
}

fn is_device_failure_surface(err: &StoreError) -> bool {
    if matches!(err, StoreError::Io(_) | StoreError::WriterCrashed) {
        return true;
    }
    if let StoreError::BatchFailed { source, .. } = err {
        return is_device_failure_surface(source);
    }
    if let StoreError::BatchSyncFailed { source, .. } = err {
        return is_device_failure_surface(source);
    }
    false
}

fn recovered_entries(store: &Store) -> Vec<batpak::store::index::IndexEntry> {
    store.query(&Region::scope(BATCH_TORN_TAIL_SCOPE))
}

fn event_ids(entries: &[batpak::store::index::IndexEntry]) -> HashSet<u128> {
    entries
        .iter()
        .map(|entry| entry.event_id().as_u128())
        .collect()
}

fn assert_durable_covers_recovered(store: &Store) {
    let frontier = store.frontier();
    let entries = recovered_entries(store);
    for entry in &entries {
        assert!(
            point(entry) <= frontier.durable_hlc,
            "PROPERTY: durable_hlc must cover every recovered event; \
             entry={entry:?}, frontier={frontier:?}"
        );
    }
}

fn assert_all_receipts_recovered(store: &Store, receipts: &[AppendReceipt]) {
    let ids = event_ids(&recovered_entries(store));
    for receipt in receipts {
        let raw = u128::from(receipt.event_id);
        assert!(
            ids.contains(&raw),
            "PROPERTY: fsynced receipt {raw} must recover; recovered ids={ids:?}",
        );
    }
}

fn create_default_mounted_device(backing: &Path) -> FlakeyDevice {
    let device = FlakeyDevice::create_with_backing(backing, DEVICE_SIZE_BYTES)
        .expect("create flakey device with caller-owned backing");
    device
        .format_and_mount_ext4_default()
        .expect("format and mount ext4 without sync");
    device
}

fn reopen_existing_device(backing: &Path) -> FlakeyDevice {
    let device = FlakeyDevice::open_existing_backing(backing).expect("open existing backing file");
    device
        .mount_existing_ext4()
        .expect("mount existing ext4 filesystem");
    device
}

#[test]
fn durable_frontier_covers_recovered_state_after_batch_device_failure_cadence_1000() {
    if !chaos_enabled() {
        let _ = writeln!(
            std::io::stderr(),
            "skipping privileged batch torn-tail proof; set BATPAK_RUN_CHAOS=1 to run it"
        );
        return;
    }

    let temp = TempDir::new().expect("caller-owned backing tempdir");
    let backing = backing_path(&temp);
    let device = create_default_mounted_device(&backing);
    let store = open_store_on_device(&device, 1000);

    let _receipts = append_batch(&store, "unsynced-batch", 5);
    let pre_failure_entries = recovered_entries(&store);
    assert_eq!(
        pre_failure_entries.len(),
        5,
        "PROPERTY: batch must be query-visible before device failure"
    );
    let pre_failure_durable_hlc = store.frontier().durable_hlc;

    device.flip_to_error().expect("flip device to error target");
    drop(store);
    drop(device);

    let reopened_device = reopen_existing_device(&backing);
    let reopened = open_store_on_device(&reopened_device, 1000);
    let entries = recovered_entries(&reopened);
    let recovered_durable_hlc = reopened.frontier().durable_hlc;

    assert_durable_covers_recovered(&reopened);
    assert!(
        recovered_durable_hlc >= pre_failure_durable_hlc,
        "PROPERTY: durable frontier must be monotonic across crash; \
         pre_failure_durable_hlc={pre_failure_durable_hlc:?}, \
         recovered_durable_hlc={recovered_durable_hlc:?}"
    );
    if let Some(highest) = entries.iter().map(point).max() {
        assert!(
            highest <= recovered_durable_hlc,
            "PROPERTY: highest recovered batch HLC must be covered by durable_hlc"
        );
    }
}

#[test]
fn batch_append_surfaces_io_error_after_device_failure_cadence_1000() {
    if !chaos_enabled() {
        let _ = writeln!(
            std::io::stderr(),
            "skipping privileged batch IO proof; set BATPAK_RUN_CHAOS=1 to run it"
        );
        return;
    }

    let temp = TempDir::new().expect("caller-owned backing tempdir");
    let backing = backing_path(&temp);
    let device = create_default_mounted_device(&backing);
    let store = open_store_on_device(&device, 1000);

    let _baseline = append_batch(&store, "baseline", 2);
    store.sync().expect("sync baseline batch");
    let _pre_failure_durable_hlc = store.frontier().durable_hlc;

    device.flip_to_error().expect("flip device to error target");
    let err = store
        .append_batch(batch_items("after-flip", 3))
        .expect_err("PROPERTY: batch append after dm-flakey error target must not succeed");
    assert!(
        is_device_failure_surface(&err),
        "PROPERTY: device failure must surface as IO or writer crash, \
         directly or through the batch item boundary, got {err:?}"
    );
}

#[test]
fn post_fsync_batches_survive_device_failure_durability_floor() {
    if !chaos_enabled() {
        let _ = writeln!(
            std::io::stderr(),
            "skipping privileged batch durability-floor proof; set BATPAK_RUN_CHAOS=1 to run it"
        );
        return;
    }

    let temp = TempDir::new().expect("caller-owned backing tempdir");
    let backing = backing_path(&temp);
    let device = create_default_mounted_device(&backing);
    let store = open_store_on_device(&device, 1000);

    let mut receipts = Vec::new();
    for idx in 0..3 {
        receipts.extend(append_batch(&store, &format!("fsynced-batch-{idx}"), 2));
        store.sync().expect("sync fsynced batch");
    }

    device.flip_to_error().expect("flip device to error target");
    drop(store);
    drop(device);

    let reopened_device = reopen_existing_device(&backing);
    let reopened = open_store_on_device(&reopened_device, 1000);
    assert_all_receipts_recovered(&reopened, &receipts);
    assert_durable_covers_recovered(&reopened);
}

#[test]
fn mixed_single_and_batch_durable_floor_survives_device_failure() {
    if !chaos_enabled() {
        let _ = writeln!(
            std::io::stderr(),
            "skipping privileged mixed durability-floor proof; set BATPAK_RUN_CHAOS=1 to run it"
        );
        return;
    }

    let temp = TempDir::new().expect("caller-owned backing tempdir");
    let backing = backing_path(&temp);
    let device = create_default_mounted_device(&backing);
    let store = open_store_on_device(&device, 1000);

    let mut receipts = Vec::new();
    receipts.push(append_single(&store, "entity:mixed:single-0", 0));
    store.sync().expect("sync first single");
    receipts.extend(append_batch(&store, "mixed-batch-0", 2));
    store.sync().expect("sync first batch");
    receipts.push(append_single(&store, "entity:mixed:single-1", 1));
    store.sync().expect("sync second single");
    receipts.extend(append_batch(&store, "mixed-batch-1", 3));
    store.sync().expect("sync second batch");
    let pre_failure_durable_hlc = store.frontier().durable_hlc;

    device.flip_to_error().expect("flip device to error target");
    drop(store);
    drop(device);

    let reopened_device = reopen_existing_device(&backing);
    let reopened = open_store_on_device(&reopened_device, 1000);
    let recovered_durable_hlc = reopened.frontier().durable_hlc;
    assert_all_receipts_recovered(&reopened, &receipts);
    assert_durable_covers_recovered(&reopened);
    assert!(
        recovered_durable_hlc >= pre_failure_durable_hlc,
        "PROPERTY: durable frontier must be monotonic across mixed crash boundary"
    );
}

#[test]
fn partial_batch_writeback_durable_hlc_remains_monotonic() {
    if !chaos_enabled() {
        let _ = writeln!(
            std::io::stderr(),
            "skipping privileged partial-batch proof; set BATPAK_RUN_CHAOS=1 to run it"
        );
        return;
    }

    let temp = TempDir::new().expect("caller-owned backing tempdir");
    let backing = backing_path(&temp);
    let device = create_default_mounted_device(&backing);
    let store = open_store_on_device(&device, 1000);

    let floor = append_single(&store, "entity:partial:floor", 0);
    store.sync().expect("sync durable floor");
    let pre_failure_durable_hlc = store.frontier().durable_hlc;
    let _unsynced = append_batch(&store, "partial-unsynced", 20);

    device.flip_to_error().expect("flip device to error target");
    drop(store);
    drop(device);

    let reopened_device = reopen_existing_device(&backing);
    let reopened = open_store_on_device(&reopened_device, 1000);
    let recovered_durable_hlc = reopened.frontier().durable_hlc;
    assert_all_receipts_recovered(&reopened, &[floor]);
    assert_durable_covers_recovered(&reopened);
    assert!(
        recovered_durable_hlc >= pre_failure_durable_hlc,
        "PROPERTY: durable frontier must remain monotonic across partial batch writeback"
    );
}

#[test]
fn batch_append_surfaces_io_error_after_device_failure_cadence_1() {
    if !chaos_enabled() {
        let _ = writeln!(
            std::io::stderr(),
            "skipping privileged cadence=1 batch IO proof; set BATPAK_RUN_CHAOS=1 to run it"
        );
        return;
    }

    let temp = TempDir::new().expect("caller-owned backing tempdir");
    let backing = backing_path(&temp);
    let device = create_default_mounted_device(&backing);
    let store = open_store_on_device(&device, 1);

    device.flip_to_error().expect("flip device to error target");
    let err = store
        .append_batch(batch_items("cadence1-after-flip", 3))
        .expect_err("PROPERTY: cadence=1 batch append after device failure must not succeed");
    assert!(
        is_device_failure_surface(&err),
        "PROPERTY: device failure must surface as IO or writer crash, \
         directly or through the batch item boundary, got {err:?}"
    );
}
