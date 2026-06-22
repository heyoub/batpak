//! Lane branch behavior proofs.
//!
//! PROVES: INV-LANE-BRANCH-ISOLATION. Per-entity hash-chain heads, clocks,
//! compare-and-swap checks, and reads are lane-scoped while lane-0 default
//! behavior remains the compatibility path.
//! CATCHES: writer paths that flatten all lanes through one entity head,
//! `DagPosition::fork` not being called for branch roots, lane filters that
//! leak events from other lanes, and cold-start rebuild losing per-lane heads.
//! SEEDED: tempfile stores, explicit branch-root hints, interleaved lane
//! appends, reopen through rebuild/checkpoint/mmap paths.

use batpak::store::index::IndexEntry;
use batpak_testkit::prelude::*;
use std::time::Duration;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn io_err(message: &'static str) -> std::io::Error {
    std::io::Error::other(message)
}

fn store_config(dir: &TempDir) -> StoreConfig {
    StoreConfig::new(dir.path()).with_sync_every_n_events(1)
}

fn coord() -> TestResult<Coordinate> {
    Ok(Coordinate::new("entity:lane", "scope:lane")?)
}

fn append_on_lane(store: &Store, lane: u32, branch_root: bool, value: u32) -> TestResult {
    let hint = if branch_root {
        AppendPositionHint::branch_root(lane, 0)
    } else {
        AppendPositionHint::new(lane, u32::from(lane != 0))
    };
    let _ = store.append_with_options(
        &coord()?,
        EventKind::DATA,
        &serde_json::json!({ "value": value, "lane": lane }),
        AppendOptions::new().with_position_hint(hint),
    )?;
    Ok(())
}

fn wait_batch_ticket(ticket: &BatchAppendTicket) -> TestResult<Vec<AppendReceipt>> {
    Ok(ticket.receiver().recv_timeout(Duration::from_secs(2))??)
}

fn batch_item(lane: u32, branch_root: bool, value: u32) -> TestResult<BatchAppendItem> {
    let hint = if branch_root {
        AppendPositionHint::branch_root(lane, 0)
    } else {
        AppendPositionHint::new(lane, u32::from(lane != 0))
    };
    Ok(BatchAppendItem::new(
        coord()?,
        EventKind::DATA,
        &serde_json::json!({ "value": value, "lane": lane }),
        AppendOptions::new().with_position_hint(hint),
        CausationRef::None,
    )?)
}

#[test]
fn default_lane_zero_path_and_lane_scoped_cas() -> TestResult {
    let dir = TempDir::new()?;
    let store = Store::open(store_config(&dir))?;
    let coord = coord()?;

    let _ = store.append(&coord, EventKind::DATA, &serde_json::json!({ "value": 0 }))?;
    let _ = store.append(&coord, EventKind::DATA, &serde_json::json!({ "value": 1 }))?;
    append_on_lane(&store, 1, true, 10)?;

    let _ = store.append_with_options(
        &coord,
        EventKind::DATA,
        &serde_json::json!({ "value": 11, "lane": 1 }),
        AppendOptions::new()
            .with_position_hint(AppendPositionHint::new(1, 1))
            .with_cas(0),
    )?;

    let lane0 = store.stream_lane("entity:lane", 0);
    let lane1 = store.stream_lane("entity:lane", 1);
    assert_eq!(
        lane0.iter().map(IndexEntry::clock).collect::<Vec<_>>(),
        vec![0, 1],
        "PROPERTY: appends with no position hint must remain the lane-0 compatibility path"
    );
    assert_eq!(
        lane1.iter().map(IndexEntry::clock).collect::<Vec<_>>(),
        vec![0, 1],
        "PROPERTY: CAS expected_sequence must compare against the lane-1 head, not the lane-0 head"
    );
    Ok(())
}

#[test]
fn default_lane_zero_position_matches_golden_wire_bytes() -> TestResult {
    let dir = TempDir::new()?;
    let store = Store::open(StoreConfig::new(dir.path()).with_clock_fn(|| 1_234_000))?;
    let coord = coord()?;

    let _ = store.append(&coord, EventKind::DATA, &serde_json::json!({ "value": 0 }))?;
    let entry = store
        .latest_lane("entity:lane", 0)
        .ok_or_else(|| io_err("missing lane-0 latest"))?;
    let stored = store.get(entry.event_id())?;
    let encoded_position = batpak::encoding::to_bytes(&stored.event.header.position)?;

    let golden = [
        0x85, 0xa7, b'w', b'a', b'l', b'l', b'_', b'm', b's', 0xcd, 0x04, 0xd2, 0xa7, b'c', b'o',
        b'u', b'n', b't', b'e', b'r', 0x00, 0xa5, b'd', b'e', b'p', b't', b'h', 0x00, 0xa4, b'l',
        b'a', b'n', b'e', 0x00, 0xa8, b's', b'e', b'q', b'u', b'e', b'n', b'c', b'e', 0x00,
    ];
    assert_eq!(
        encoded_position,
        golden,
        "PROPERTY: default append lane-0 DagPosition wire bytes must remain byte-identical to the compatibility path"
    );
    store.close()?;
    Ok(())
}

#[test]
fn per_lane_chain_heads_do_not_alias() -> TestResult {
    let dir = TempDir::new()?;
    let store = Store::open(store_config(&dir))?;

    append_on_lane(&store, 0, false, 0)?;
    append_on_lane(&store, 1, true, 10)?;
    append_on_lane(&store, 0, false, 1)?;
    append_on_lane(&store, 1, false, 11)?;

    let lane0 = store.stream_lane("entity:lane", 0);
    let lane1 = store.stream_lane("entity:lane", 1);
    assert_eq!(lane0.len(), 2);
    assert_eq!(lane1.len(), 2);
    assert_eq!(
        lane0.iter().map(IndexEntry::clock).collect::<Vec<_>>(),
        vec![0, 1],
        "PROPERTY: lane-0 clock must advance against only the lane-0 head"
    );
    assert_eq!(
        lane1.iter().map(IndexEntry::clock).collect::<Vec<_>>(),
        vec![0, 1],
        "PROPERTY: lane-1 clock must advance against only the lane-1 head"
    );
    assert_eq!(lane0[0].hash_chain().prev_hash, [0; 32]);
    assert_eq!(lane1[0].hash_chain().prev_hash, [0; 32]);
    assert_eq!(
        lane0[1].hash_chain().prev_hash,
        lane0[0].hash_chain().event_hash
    );
    assert_eq!(
        lane1[1].hash_chain().prev_hash,
        lane1[0].hash_chain().event_hash
    );
    assert_ne!(
        lane1[0].hash_chain().prev_hash,
        lane0[0].hash_chain().event_hash,
        "PROPERTY: a branch root must not inherit lane-0's prev_hash"
    );
    Ok(())
}

#[test]
fn same_lane_fenced_writes_chain_through_hidden_head() -> TestResult {
    let dir = TempDir::new()?;
    let store = Store::open(store_config(&dir))?;
    let fence = store.begin_visibility_fence()?;

    let mut outbox = fence.outbox();
    outbox.stage_with_options(
        coord()?,
        EventKind::DATA,
        &serde_json::json!({ "value": 10, "lane": 1 }),
        AppendOptions::new().with_position_hint(AppendPositionHint::branch_root(1, 0)),
    )?;
    let first = outbox.submit_flush()?;

    let mut outbox = fence.outbox();
    outbox.stage_with_options(
        coord()?,
        EventKind::DATA,
        &serde_json::json!({ "value": 11, "lane": 1 }),
        AppendOptions::new().with_position_hint(AppendPositionHint::new(1, 1)),
    )?;
    let second = outbox.submit_flush()?;

    fence.commit()?;
    assert_eq!(wait_batch_ticket(&first)?.len(), 1);
    assert_eq!(wait_batch_ticket(&second)?.len(), 1);

    let lane1 = store.stream_lane("entity:lane", 1);
    assert_eq!(
        lane1.iter().map(IndexEntry::clock).collect::<Vec<_>>(),
        vec![0, 1],
        "PROPERTY: a second fenced write on the same lane must chain through the first hidden committed head"
    );
    assert_eq!(
        lane1[1].hash_chain().prev_hash,
        lane1[0].hash_chain().event_hash,
        "PROPERTY: hidden fenced writes must still form the per-lane hash chain before publication"
    );
    Ok(())
}

#[test]
fn branch_root_hint_uses_dag_position_fork() -> TestResult {
    let dir = TempDir::new()?;
    let store = Store::open(store_config(&dir))?;

    append_on_lane(&store, 0, false, 0)?;
    append_on_lane(&store, 1, true, 10)?;

    let root = store
        .latest_lane("entity:lane", 0)
        .ok_or_else(|| io_err("missing lane 0 latest"))?;
    let fork = store
        .latest_lane("entity:lane", 1)
        .ok_or_else(|| io_err("missing lane 1 latest"))?;
    let root_pos = store.get(root.event_id())?.event.header.position;
    let fork_pos = store.get(fork.event_id())?.event.header.position;

    assert_eq!(fork_pos.lane(), 1);
    assert_eq!(
        fork_pos.depth(),
        1,
        "PROPERTY: branch_root(lane=1,parent_depth=0) must call DagPosition::fork"
    );
    assert!(
        root_pos.partial_cmp(&fork_pos).is_none(),
        "PROPERTY: lane-0 and forked lane positions must be incomparable"
    );
    assert!(
        !root_pos.is_ancestor_of(&fork_pos),
        "PROPERTY: lane-0 must not be an ancestor of a forked lane"
    );
    Ok(())
}

#[test]
fn region_and_public_reads_filter_by_lane() -> TestResult {
    let dir = TempDir::new()?;
    let store = Store::open(store_config(&dir))?;

    append_on_lane(&store, 0, false, 0)?;
    append_on_lane(&store, 1, true, 10)?;
    append_on_lane(&store, 1, false, 11)?;

    let all = store.by_entity("entity:lane");
    let region_lane = store.query(&Region::all().with_lane(1));
    let method_lane = store.query_lane(&Region::all(), 1);
    let entity_lane = store.by_entity_lane("entity:lane", 1);

    assert_eq!(all.len(), 3);
    assert_eq!(region_lane.len(), 2);
    assert_eq!(method_lane.len(), 2);
    assert_eq!(entity_lane.len(), 2);
    assert!(region_lane.iter().all(|entry| entry.dag_lane() == 1));
    assert!(method_lane.iter().all(|entry| entry.dag_lane() == 1));
    assert!(entity_lane.iter().all(|entry| entry.dag_lane() == 1));
    assert_eq!(Region::all().with_lane(7).lane(), Some(7));
    Ok(())
}

#[test]
fn cold_start_rebuild_restores_per_lane_heads() -> TestResult {
    let dir = TempDir::new()?;
    let config = store_config(&dir)
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false);
    {
        let store = Store::open(config.clone())?;
        append_on_lane(&store, 0, false, 0)?;
        append_on_lane(&store, 1, true, 10)?;
        store.close()?;
    }

    let reopened = Store::open(config)?;
    append_on_lane(&reopened, 1, false, 11)?;
    append_on_lane(&reopened, 0, false, 1)?;

    let lane0 = reopened.stream_lane("entity:lane", 0);
    let lane1 = reopened.stream_lane("entity:lane", 1);
    assert_eq!(
        lane0.iter().map(IndexEntry::clock).collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(
        lane1.iter().map(IndexEntry::clock).collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(
        lane1[1].hash_chain().prev_hash,
        lane1[0].hash_chain().event_hash,
        "PROPERTY: cold start must rebuild the lane-1 head from persisted lane metadata"
    );
    Ok(())
}

#[test]
fn batch_append_keeps_per_lane_heads_independent() -> TestResult {
    let dir = TempDir::new()?;
    let store = Store::open(store_config(&dir))?;

    store.append_batch(vec![
        batch_item(0, false, 0)?,
        batch_item(1, true, 10)?,
        batch_item(0, false, 1)?,
        batch_item(1, false, 11)?,
    ])?;

    let lane0 = store.stream_lane("entity:lane", 0);
    let lane1 = store.stream_lane("entity:lane", 1);
    assert_eq!(
        lane0.iter().map(IndexEntry::clock).collect::<Vec<_>>(),
        vec![0, 1],
        "PROPERTY: batch staging must key lane-0 state separately"
    );
    assert_eq!(
        lane1.iter().map(IndexEntry::clock).collect::<Vec<_>>(),
        vec![0, 1],
        "PROPERTY: batch staging must key lane-1 state separately"
    );
    assert_eq!(lane0[0].hash_chain().prev_hash, [0; 32]);
    assert_eq!(lane1[0].hash_chain().prev_hash, [0; 32]);
    assert_eq!(
        lane0[1].hash_chain().prev_hash,
        lane0[0].hash_chain().event_hash
    );
    assert_eq!(
        lane1[1].hash_chain().prev_hash,
        lane1[0].hash_chain().event_hash
    );
    Ok(())
}

#[test]
fn paged_query_and_cursor_filter_by_lane() -> TestResult {
    let dir = TempDir::new()?;
    let store = Store::open(store_config(&dir))?;

    append_on_lane(&store, 0, false, 0)?;
    append_on_lane(&store, 1, true, 10)?;
    append_on_lane(&store, 0, false, 1)?;
    append_on_lane(&store, 1, false, 11)?;

    let region = Region::entity("entity:lane").with_lane(1);
    let first = store.query_entries_after(&region, None, 1);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].dag_lane(), 1);
    let second = store.query_entries_after(&region, Some(first[0].global_sequence()), 8);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].dag_lane(), 1);

    let mut cursor = store.cursor_guaranteed(&region);
    let batch = cursor.poll_batch(8);
    assert_eq!(batch.len(), 2);
    assert!(batch.iter().all(|entry| entry.dag_lane() == 1));
    assert!(cursor.poll().is_none());
    Ok(())
}

#[test]
fn subscription_fanout_filters_by_lane_before_push() -> TestResult {
    let dir = TempDir::new()?;
    let store = Store::open(store_config(&dir))?;
    let subscription = store.subscribe_lossy(&Region::entity("entity:lane").with_lane(1));

    append_on_lane(&store, 0, false, 0)?;
    append_on_lane(&store, 1, true, 10)?;

    let notification = subscription
        .filtered_receiver()
        .recv_timeout(Duration::from_secs(1))?;
    assert_eq!(notification.position.lane(), 1);
    assert!(
        subscription
            .filtered_receiver()
            .recv_timeout(Duration::from_millis(25))
            .is_err(),
        "PROPERTY: lane-0 notifications must not enter a lane-1 subscription channel"
    );
    Ok(())
}

fn reopen_restores_per_lane_heads(config: StoreConfig) -> TestResult {
    {
        let store = Store::open(config.clone())?;
        append_on_lane(&store, 0, false, 0)?;
        append_on_lane(&store, 1, true, 10)?;
        store.close()?;
    }

    let reopened = Store::open(config)?;
    append_on_lane(&reopened, 1, false, 11)?;
    append_on_lane(&reopened, 0, false, 1)?;

    let lane0 = reopened.stream_lane("entity:lane", 0);
    let lane1 = reopened.stream_lane("entity:lane", 1);
    assert_eq!(
        lane0.iter().map(IndexEntry::clock).collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(
        lane1.iter().map(IndexEntry::clock).collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(
        lane0[1].hash_chain().prev_hash,
        lane0[0].hash_chain().event_hash
    );
    assert_eq!(
        lane1[1].hash_chain().prev_hash,
        lane1[0].hash_chain().event_hash
    );
    Ok(())
}

#[test]
fn cold_start_checkpoint_restores_per_lane_heads() -> TestResult {
    let dir = TempDir::new()?;
    reopen_restores_per_lane_heads(
        store_config(&dir)
            .with_enable_checkpoint(true)
            .with_enable_mmap_index(false),
    )
}

#[test]
fn cold_start_mmap_restores_per_lane_heads() -> TestResult {
    let dir = TempDir::new()?;
    reopen_restores_per_lane_heads(
        store_config(&dir)
            .with_enable_checkpoint(true)
            .with_enable_mmap_index(true),
    )
}
