//! dm-flakey proofs for `INV-FRONTIER-DURABLE-COVERS-RECOVERED`.
//!
//! These tests use a real Linux device-mapper failure boundary instead of the
//! in-process `FaultInjector` panic seam. They prove that batpak's recovered
//! durable frontier covers every event recovered from the segment log, that the
//! durable frontier remains monotonic across a device failure, that fsynced
//! events remain recoverable, and that cadence=1 surfaces device failure to the
//! caller.
//!
//! Writer-side sync audit for this workload: single appends write frames without
//! calling fsync directly; durability is advanced by explicit `Store::sync()`,
//! cadence/group-commit drains, visibility-fence drains, segment rotation, or
//! shutdown/close drains. This scenario uses `sync_every_n_events=1000`, one
//! explicit sync after event A, no fences, and no segment rotation pressure, so
//! event B has no batpak-side fsync before the injected device failure. The OS
//! may still preserve B through page-cache writeback or ext4 journal behavior;
//! that is allowed. Batpak's contract is Meaning-2 durable_hlc semantics: on
//! recovery, whatever was physically preserved and can be queried is classified
//! as durable going forward.

use crate::chaos::dm_flakey::FlakeyDevice;
use batpak::prelude::{Coordinate, EventKind, Region};
use batpak::store::{AppendReceipt, HlcPoint, Store, StoreConfig, StoreError};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const DEVICE_SIZE_BYTES: u64 = 64 * 1024 * 1024;
const TORN_TAIL_SCOPE: &str = "scope:frontier-torn-tail";

fn chaos_enabled() -> bool {
    std::env::var_os("BATPAK_RUN_CHAOS").is_some()
}

fn coord(entity: &str) -> Coordinate {
    Coordinate::new(entity, TORN_TAIL_SCOPE).expect("valid torn-tail coordinate")
}

fn kind() -> EventKind {
    EventKind::custom(0xF, 0x92)
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

fn append_named(store: &Store, entity: &str, value: u32) -> AppendReceipt {
    store
        .append(
            &coord(entity),
            kind(),
            &serde_json::json!({ "value": value }),
        )
        .expect("append named event")
}

fn recovered_entries(store: &Store) -> Vec<batpak::store::index::IndexEntry> {
    store.query(&Region::scope(TORN_TAIL_SCOPE))
}

fn event_ids(entries: &[batpak::store::index::IndexEntry]) -> Vec<u128> {
    entries.iter().map(|entry| entry.event_id()).collect()
}

fn entry_point_for(
    entries: &[batpak::store::index::IndexEntry],
    receipt: &AppendReceipt,
) -> HlcPoint {
    entries
        .iter()
        .find(|entry| entry.event_id() == receipt.event_id)
        .map(point)
        .expect("receipt event must be query-visible before failure")
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
fn durable_frontier_covers_recovered_state_after_device_failure_cadence_1000() {
    if !chaos_enabled() {
        eprintln!("skipping privileged torn-tail proof; set BATPAK_RUN_CHAOS=1 to run it");
        return;
    }

    let temp = TempDir::new().expect("caller-owned backing tempdir");
    let backing = backing_path(&temp);
    let device = create_default_mounted_device(&backing);
    let store = open_store_on_device(&device, 1000);

    let durable = append_named(&store, "entity:torn-tail:durable", 1);
    store.sync().expect("sync durable lower-bound event");
    let pre_failure_durable_hlc = store.frontier().durable_hlc;
    let _unsynced = append_named(&store, "entity:torn-tail:unsynced", 2);
    let pre_failure_entries = recovered_entries(&store);
    let durable_point = entry_point_for(&pre_failure_entries, &durable);
    assert!(
        pre_failure_durable_hlc >= durable_point,
        "PROPERTY: explicit sync must advance durable_hlc to cover event A"
    );

    device.flip_to_error().expect("flip device to error target");
    drop(store);
    drop(device);

    let reopened_device = reopen_existing_device(&backing);
    let reopened = open_store_on_device(&reopened_device, 1000);
    let entries = recovered_entries(&reopened);
    let ids = event_ids(&entries);

    assert!(
        ids.contains(&durable.event_id),
        "PROPERTY: fsynced lower-bound event must recover"
    );
    let recovered_durable_hlc = reopened.frontier().durable_hlc;
    for entry in &entries {
        assert!(
            point(entry) <= recovered_durable_hlc,
            "PROPERTY: durable_hlc must cover every recovered event; \
             entry={entry:?}, recovered_durable_hlc={recovered_durable_hlc:?}, \
             recovered ids={ids:?}, reopened frontier={:?}",
            reopened.frontier()
        );
    }
    assert!(
        recovered_durable_hlc >= pre_failure_durable_hlc,
        "PROPERTY: durable frontier must be monotonic across crash; \
         pre_failure_durable_hlc={pre_failure_durable_hlc:?}, \
         recovered_durable_hlc={recovered_durable_hlc:?}, recovered ids={ids:?}"
    );
}

#[test]
fn single_append_written_surfaces_io_error_cadence_1() {
    if !chaos_enabled() {
        eprintln!("skipping privileged cadence=1 IO proof; set BATPAK_RUN_CHAOS=1 to run it");
        return;
    }

    let temp = TempDir::new().expect("caller-owned backing tempdir");
    let backing = backing_path(&temp);
    let device = create_default_mounted_device(&backing);
    let store = open_store_on_device(&device, 1);

    let _durable = append_named(&store, "entity:torn-tail:cadence1-durable", 1);
    device.flip_to_error().expect("flip device to error target");

    let err = match store.append(
        &coord("entity:torn-tail:cadence1-after-flip"),
        kind(),
        &serde_json::json!({ "value": 2 }),
    ) {
        Ok(_) => panic!("PROPERTY: append after dm-flakey error target must not succeed"),
        Err(err) => err,
    };
    assert!(
        matches!(err, StoreError::Io(_) | StoreError::WriterCrashed),
        "PROPERTY: device failure must surface as IO or writer crash, got {err:?}"
    );
}

#[test]
fn post_fsync_events_survive_device_failure_durability_floor() {
    if !chaos_enabled() {
        eprintln!("skipping privileged durability-floor proof; set BATPAK_RUN_CHAOS=1 to run it");
        return;
    }

    let temp = TempDir::new().expect("caller-owned backing tempdir");
    let backing = backing_path(&temp);
    let device = create_default_mounted_device(&backing);
    let store = open_store_on_device(&device, 1000);

    let receipts = (0..3)
        .map(|idx| {
            let receipt = append_named(&store, &format!("entity:torn-tail:fsynced-{idx}"), idx);
            store.sync().expect("sync fsynced event");
            receipt
        })
        .collect::<Vec<_>>();

    device.flip_to_error().expect("flip device to error target");
    drop(store);
    drop(device);

    let reopened_device = reopen_existing_device(&backing);
    let reopened = open_store_on_device(&reopened_device, 1000);
    let ids = event_ids(&recovered_entries(&reopened));

    assert_eq!(
        ids.len(),
        receipts.len(),
        "PROPERTY: device failure after fsync must not drop durable events"
    );
    for receipt in receipts {
        assert!(
            ids.contains(&receipt.event_id),
            "PROPERTY: fsynced event {} must recover",
            receipt.event_id
        );
    }
}
