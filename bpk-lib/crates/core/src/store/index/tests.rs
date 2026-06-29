use super::*;
use crate::coordinate::{Coordinate, Region};
use crate::event::{EventKind, HashChain};
use crate::store::{IndexConfig, IndexTopology};

fn make_entry(seq: u64, entity: &str, scope: &str) -> IndexEntry {
    let coord = Coordinate::new(entity, scope).expect("coord");
    IndexEntry {
        event_id: seq as u128 + 1,
        correlation_id: seq as u128 + 1,
        causation_id: None,
        entity_id: self::interner::InternId::sentinel(),
        scope_id: self::interner::InternId::sentinel(),
        coord,
        kind: EventKind::custom(0xF, 1),
        wall_ms: seq,
        clock: u32::try_from(seq).expect("small seq"),
        dag_lane: 0,
        dag_depth: 0,
        hash_chain: HashChain::default(),
        disk_pos: DiskPos {
            segment_id: 0,
            offset: seq * 16,
            length: 16,
        },
        global_sequence: seq,
        receipt_extensions: BTreeMap::new(),
    }
}

#[test]
fn hlc_for_global_sequence_matches_the_queried_sequence_exactly() {
    // `find(... global_sequence == g)` -> `!=` would return the HLC of some OTHER
    // entry. `make_entry` sets `wall_ms == seq`, so the entry for seq=2 is the only
    // one whose wall_ms is 2; an inverted match resolves a different entry whose
    // wall_ms is never 2.
    let index = StoreIndex::new();
    let entity_id = index.interner.intern("entity:hlc").expect("intern");
    let scope_id = index.interner.intern("scope:hlc").expect("intern");
    for seq in 0..3 {
        let mut entry = make_entry(seq, "entity:hlc", "scope:hlc");
        entry.entity_id = entity_id;
        entry.scope_id = scope_id;
        index.insert(entry);
    }

    let point = index
        .hlc_for_global_sequence(2)
        .expect("global_sequence 2 must resolve to an HlcPoint");
    let mut failures: Vec<String> = Vec::new();
    if point.global_sequence != 2 {
        failures.push(format!(
            "returned HlcPoint must name the queried sequence 2, got {}",
            point.global_sequence
        ));
    }
    if point.wall_ms != 2 {
        failures.push(format!(
            "HLC must come from the entry whose global_sequence == 2 (wall_ms==seq); an \
             inverted `!=` match returns a different entry's wall_ms, got {}",
            point.wall_ms
        ));
    }
    assert!(
        failures.is_empty(),
        "hlc_for_global_sequence mismatches: {failures:?}"
    );
}

#[test]
fn clock_key_orders_by_wall_then_clock_then_uuid() {
    let mut keys = [
        ClockKey {
            wall_ms: 10,
            clock: 3,
            uuid: 9,
        },
        ClockKey {
            wall_ms: 9,
            clock: 99,
            uuid: 1,
        },
        ClockKey {
            wall_ms: 10,
            clock: 2,
            uuid: 99,
        },
        ClockKey {
            wall_ms: 10,
            clock: 3,
            uuid: 4,
        },
    ];

    keys.sort();

    assert_eq!(
            keys,
            [
                ClockKey {
                    wall_ms: 9,
                    clock: 99,
                    uuid: 1,
                },
                ClockKey {
                    wall_ms: 10,
                    clock: 2,
                    uuid: 99,
                },
                ClockKey {
                    wall_ms: 10,
                    clock: 3,
                    uuid: 4,
                },
                ClockKey {
                    wall_ms: 10,
                    clock: 3,
                    uuid: 9,
                },
            ],
            "PROPERTY: ClockKey ordering must be wall_ms first, then clock, then uuid as the deterministic tiebreaker"
        );
}

#[test]
fn bulk_restore_keeps_entries_invisible_until_publish() {
    let index = StoreIndex::new();
    let entity_id = index.interner.intern("entity:bulk").expect("intern");
    let scope_id = index.interner.intern("scope:bulk").expect("intern");
    let entries = (0..3)
        .map(|seq| {
            let mut entry = make_entry(seq, "entity:bulk", "scope:bulk");
            entry.entity_id = entity_id;
            entry.scope_id = scope_id;
            entry
        })
        .collect();

    index
        .restore_sorted_entries_with_before_publish(entries, 3, |index| {
            assert_eq!(
                index.visible_sequence(),
                0,
                "visibility watermark must not advance until every view is rebuilt"
            );
            assert!(
                index.query(&Region::all()).is_empty(),
                "PROPERTY: reads must observe neither base maps nor overlays before publish"
            );
        })
        .expect("bulk restore publish must succeed");

    assert_eq!(index.query(&Region::all()).len(), 3);
    assert_eq!(index.visible_sequence(), 3);
}

#[test]
fn upgrade_with_visibility_snapshot_rejects_cancelled_ranges() {
    let index = StoreIndex::new();
    let entity_id = index.interner.intern("entity:visibility").expect("intern");
    let scope_id = index.interner.intern("scope:visibility").expect("intern");
    for seq in 0..3 {
        let mut entry = make_entry(seq, "entity:visibility", "scope:visibility");
        entry.entity_id = entity_id;
        entry.scope_id = scope_id;
        index.insert(entry);
    }
    index
        .publish(3, "test-publish")
        .expect("publish test entries");
    index.restore_cancelled_visibility_ranges(CancelledVisibilityRanges {
        global: vec![(1, 2)],
        lanes: BTreeMap::new(),
    });

    let hidden = QueryHit {
        event_id: 2,
        global_sequence: 1,
        disk_pos: DiskPos::new(0, 16, 16),
        kind: EventKind::custom(0xF, 1),
        clock: 1,
        dag_lane: 0,
    };
    let (hits, visibility) = index.query_hits_with_snapshot(&Region::all());

    assert_eq!(
            hits.iter()
                .map(|hit| hit.global_sequence)
                .collect::<Vec<_>>(),
            vec![0, 2],
            "PROPERTY: query-hit collection must skip cancelled hidden ranges below the visible watermark"
        );
    assert!(
            index
                .upgrade_hit_with_visibility(hidden, &visibility)
                .is_none(),
            "PROPERTY: hit upgrade must use the same hidden-range visibility predicate as query collection"
        );
}

#[test]
fn cancel_visibility_fence_only_records_lanes_inside_half_open_range() {
    // Drive `cancel_visibility_fence` over a fence range of [2, 4). The
    // per-entry collection at index/mod.rs uses the half-open predicate
    // `seq >= start && seq < end`. We seed entries straddling both
    // boundaries on distinct lanes so the resulting per-lane cancelled map
    // pins the `< end` comparison against every off-by-one mutation:
    //   seq 2 (lane 10): inside  -> recorded
    //   seq 3 (lane 11): inside  -> recorded
    //   seq 4 (lane 12): == end  -> EXCLUDED (kills `<=` and `==`)
    //   seq 5 (lane 13): > end   -> EXCLUDED (kills `>`)
    let index = StoreIndex::new();
    let entity_id = index.interner.intern("entity:fence").expect("intern");
    let scope_id = index.interner.intern("scope:fence").expect("intern");
    for (seq, lane) in [(2u64, 10u32), (3, 11), (4, 12), (5, 13)] {
        let mut entry = make_entry(seq, "entity:fence", "scope:fence");
        entry.entity_id = entity_id;
        entry.scope_id = scope_id;
        entry.dag_lane = lane;
        index.insert(entry);
    }

    let token = index
        .begin_visibility_fence()
        .expect("begin visibility fence");
    index
        .note_visibility_fence_progress(token, 2, 4)
        .expect("note fence range [2, 4)");
    index
        .cancel_visibility_fence(token)
        .expect("cancel visibility fence");

    let cancelled = index.cancelled_visibility_ranges();
    let recorded_lanes: Vec<u32> = cancelled.lanes.keys().copied().collect();
    assert_eq!(
            recorded_lanes,
            vec![10, 11],
            "PROPERTY: cancel must record only lanes whose entry sequence lies in the half-open fence range [start, end)"
        );
    assert_eq!(
        cancelled.lanes.get(&10).map(Vec::as_slice),
        Some([(2, 3)].as_slice()),
        "lane 10 entry at the inclusive lower bound must be cancelled"
    );
    assert_eq!(
        cancelled.lanes.get(&11).map(Vec::as_slice),
        Some([(3, 4)].as_slice()),
        "lane 11 interior entry must be cancelled"
    );
    assert!(
        !cancelled.lanes.contains_key(&12),
        "entry at == end must NOT be cancelled (half-open upper bound)"
    );
    assert!(
        !cancelled.lanes.contains_key(&13),
        "entry beyond end must NOT be cancelled"
    );
}

#[test]
fn query_any_hits_after_excludes_wrong_lane_or_hidden_entries() {
    // `query_any_hits_after` is taken for a lane-scoped, fact-Any region
    // with no entity/scope filter. Its per-entry guard is the disjunction
    // `entry.dag_lane != lane || !is_visible_on_lane(seq, lane)`: an entry
    // is skipped if it is on the wrong lane OR hidden on the target lane.
    // We seed exactly the two cases where ONE disjunct is true (so `||`
    // skips but a `&&` mutant would NOT), plus a control that must survive:
    //   seq 0 (lane 7, hidden):  right lane, not visible -> skipped
    //   seq 1 (lane 9, visible): wrong lane              -> skipped
    //   seq 2 (lane 7, visible): right lane, visible     -> KEPT (control)
    let index = StoreIndex::new();
    let entity_id = index.interner.intern("entity:lane").expect("intern");
    let scope_id = index.interner.intern("scope:lane").expect("intern");
    for (seq, lane) in [(0u64, 7u32), (1, 9), (2, 7)] {
        let mut entry = make_entry(seq, "entity:lane", "scope:lane");
        entry.entity_id = entity_id;
        entry.scope_id = scope_id;
        entry.dag_lane = lane;
        index.insert(entry);
    }
    // Make seq < 3 visible on both lanes 7 and 9.
    index
        .publish_on_lanes(3, [(7, 3), (9, 3)], "test-lane-publish")
        .expect("publish lanes 7 and 9");
    // Hide seq 0 on lane 7 so it is on the right lane yet not visible.
    index.restore_cancelled_visibility_ranges(CancelledVisibilityRanges {
        global: Vec::new(),
        lanes: BTreeMap::from([(7u32, vec![(0u64, 1u64)])]),
    });

    let region = Region::all()
        .with_fact(crate::coordinate::KindFilter::Any)
        .with_lane(7);
    let hits = index.query_hits_after(&region, 0, false, 100);
    let seqs: Vec<u64> = hits.iter().map(|h| h.global_sequence).collect();

    assert_eq!(
            seqs,
            vec![2],
            "PROPERTY: a lane-scoped Any query must drop both wrong-lane and hidden-on-lane entries, keeping only the visible same-lane entry"
        );
}

#[test]
fn projection_replay_plan_preserves_scan_watermark_when_tail_candidate_is_hidden() {
    let index = StoreIndex::with_config(&IndexConfig {
        topology: IndexTopology::entity_local(),
        ..IndexConfig::default()
    });
    let entity_id = index
        .interner
        .intern("entity:projection-hidden-tail")
        .expect("intern");
    let scope_id = index
        .interner
        .intern("scope:projection-hidden-tail")
        .expect("intern");
    for seq in 0..2 {
        let mut entry = make_entry(
            seq,
            "entity:projection-hidden-tail",
            "scope:projection-hidden-tail",
        );
        entry.entity_id = entity_id;
        entry.scope_id = scope_id;
        index.insert(entry);
    }
    index
        .publish_on_lanes(1, [(0, 1)], "test-projection-plan")
        .expect("publish only the first lane-0 candidate");

    let plan = index
        .projection_replay_plan(
            "entity:projection-hidden-tail",
            &[EventKind::custom(0xF, 1)],
        )
        .expect("projection plan exists even when its tail candidate is hidden");

    assert_eq!(
            plan.watermark, 1,
            "PROPERTY: projection plan watermark must remain at the scan candidate watermark, not the last currently-visible item"
        );
    assert_eq!(
            plan.items
                .iter()
                .map(|item| item.global_sequence)
                .collect::<Vec<_>>(),
            vec![0],
            "PROPERTY: only visible candidates are replayed, while the watermark still records the scan high-water mark"
        );
}
