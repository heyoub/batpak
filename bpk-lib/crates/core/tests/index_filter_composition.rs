//! Index filter composition across overlays.
//!
//! [INV-INDEX-FILTER-COMPOSES] For every supported overlay topology and every
//! combination of `Region` predicates (entity prefix, scope, kind filter,
//! clock range), the index returns exactly the same set of events as a
//! linear ground-truth scan over the same corpus. This pins B1 (filters
//! apply inside the overlay) and B4 (KindFilter::Any respects limit during
//! collection). Deterministic PRNG: one fixed seed, one shuffled corpus,
//! many queries.

use batpak::coordinate::{ClockRange, Coordinate, KindFilter, Region};
use batpak::event::EventKind;
use batpak::store::index::IndexEntry;
use batpak::store::{IndexTopology, Store, StoreConfig};
use std::collections::HashSet;
use tempfile::TempDir;

const SEED_CORPUS_SIZE: usize = 120;

fn topologies() -> Vec<(&'static str, IndexTopology)> {
    vec![
        ("aos", IndexTopology::aos()),
        ("scan", IndexTopology::scan()),
        ("entity-local", IndexTopology::entity_local()),
        ("tiled", IndexTopology::tiled()),
        ("all", IndexTopology::all()),
    ]
}

fn open_store(dir: &TempDir, topology: IndexTopology) -> Store {
    let config = StoreConfig::new(dir.path())
        .with_index_topology(topology)
        .with_incremental_projection(false)
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false)
        .with_sync_every_n_events(1);
    Store::open(config).expect("open store")
}

fn entity_scoped_region(prefix: &str, scope: &str) -> Region {
    let with_scope = |region: Region, scope: &str| Region::with_scope(region, scope);
    with_scope(Region::entity(prefix), scope)
}

/// Deterministic PRNG — a tiny xorshift so every run produces the same
/// corpus. Using a hand-rolled generator keeps the test free of any extra
/// test-dependency and pins the corpus shape exactly.
struct Xor {
    state: u64,
}

impl Xor {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }
}

#[derive(Clone, Debug)]
struct GroundTruthEvent {
    entity: &'static str,
    scope: &'static str,
    kind: EventKind,
    clock_slot: u32,
}

fn build_corpus() -> Vec<GroundTruthEvent> {
    const ENTITIES: [&str; 3] = ["entity:alpha", "entity:bravo", "entity:charlie"];
    const SCOPES: [&str; 3] = ["scope:X", "scope:Y", "scope:Z"];
    const KINDS: [EventKind; 3] = [
        EventKind::custom(0x5, 1),
        EventKind::custom(0x5, 2),
        EventKind::custom(0x6, 1),
    ];

    let mut rng = Xor::new(0x1234_5678_9ABC_DEF0);
    let mut corpus = Vec::with_capacity(SEED_CORPUS_SIZE);
    // Per-entity clock counters — the store's `clock_range` semantics are
    // per entity, not per (entity, scope) stream.
    let mut clocks = std::collections::HashMap::<usize, u32>::new();

    for _ in 0..SEED_CORPUS_SIZE {
        let entity_idx = usize::try_from(rng.next() % ENTITIES.len() as u64)
            .expect("entity index stays within static corpus bounds");
        let scope_idx = usize::try_from(rng.next() % SCOPES.len() as u64)
            .expect("scope index stays within static corpus bounds");
        let kind_idx = usize::try_from(rng.next() % KINDS.len() as u64)
            .expect("kind index stays within static corpus bounds");
        let clock_slot = clocks.entry(entity_idx).or_insert(0);
        let this_clock = *clock_slot;
        *clock_slot += 1;
        corpus.push(GroundTruthEvent {
            entity: ENTITIES[entity_idx],
            scope: SCOPES[scope_idx],
            kind: KINDS[kind_idx],
            clock_slot: this_clock,
        });
    }
    corpus
}

fn seed_store_with_corpus(store: &Store, corpus: &[GroundTruthEvent]) {
    for (i, ev) in corpus.iter().enumerate() {
        let coord = Coordinate::new(ev.entity, ev.scope).expect("valid coord");
        let _ = store
            .append(&coord, ev.kind, &serde_json::json!({"i": i}))
            .expect("seed append");
    }
}

fn ground_truth(corpus: &[GroundTruthEvent], region: &Region) -> HashSet<(String, String, u32)> {
    let mut out = HashSet::new();
    for ev in corpus {
        if let Some(prefix) = region.entity_prefix() {
            if !ev.entity.starts_with(prefix) {
                continue;
            }
        }
        if let Some(scope) = region.scope_value() {
            if scope != ev.scope {
                continue;
            }
        }
        if let Some(fact) = region.fact() {
            let matches = match fact {
                KindFilter::Exact(k) => ev.kind == *k,
                KindFilter::Category(c) => ev.kind.category() == *c,
                KindFilter::Any => true,
                _ => unreachable!("reference model must be updated for new KindFilter variants"),
            };
            if !matches {
                continue;
            }
        }
        if let Some(range) = region.clock_range() {
            if ev.clock_slot < range.start() || ev.clock_slot > range.end() {
                continue;
            }
        }
        out.insert((ev.entity.to_owned(), ev.scope.to_owned(), ev.clock_slot));
    }
    out
}

fn actual(entries: &[IndexEntry]) -> HashSet<(String, String, u32)> {
    entries
        .iter()
        .filter(|e| {
            !matches!(
                e.event_kind(),
                EventKind::SYSTEM_OPEN_COMPLETED | EventKind::SYSTEM_CLOSE_COMPLETED
            )
        })
        .map(|e| {
            (
                e.coord().entity().to_owned(),
                e.coord().scope().to_owned(),
                e.clock(),
            )
        })
        .collect()
}

fn ground_truth_ordered(
    corpus: &[GroundTruthEvent],
    region: &Region,
) -> Vec<(u64, String, String, u32)> {
    corpus
        .iter()
        .enumerate()
        .filter_map(|(seq, ev)| {
            if let Some(prefix) = region.entity_prefix() {
                if !ev.entity.starts_with(prefix) {
                    return None;
                }
            }
            if let Some(scope) = region.scope_value() {
                if scope != ev.scope {
                    return None;
                }
            }
            if let Some(fact) = region.fact() {
                let matches = match fact {
                    KindFilter::Exact(k) => ev.kind == *k,
                    KindFilter::Category(c) => ev.kind.category() == *c,
                    KindFilter::Any => true,
                    _ => {
                        unreachable!("reference model must be updated for new KindFilter variants")
                    }
                };
                if !matches {
                    return None;
                }
            }
            if let Some(range) = region.clock_range() {
                if ev.clock_slot < range.start() || ev.clock_slot > range.end() {
                    return None;
                }
            }
            Some((
                u64::try_from(seq).expect("seed corpus index fits u64"),
                ev.entity.to_owned(),
                ev.scope.to_owned(),
                ev.clock_slot,
            ))
        })
        .collect()
}

fn actual_ordered(entries: &[IndexEntry]) -> Vec<(u64, String, String, u32)> {
    entries
        .iter()
        .filter(|e| {
            !matches!(
                e.event_kind(),
                EventKind::SYSTEM_OPEN_COMPLETED | EventKind::SYSTEM_CLOSE_COMPLETED
            )
        })
        .map(|e| {
            (
                e.global_sequence()
                    .checked_sub(1)
                    .expect("user events follow the mutable-open lifecycle receipt"),
                e.coord().entity().to_owned(),
                e.coord().scope().to_owned(),
                e.clock(),
            )
        })
        .collect()
}

fn assert_matches(
    label: &str,
    query_name: &str,
    region: &Region,
    store: &Store,
    corpus: &[GroundTruthEvent],
) {
    let actual_entries = store.query(region);
    let filtered_entries: Vec<_> = actual_entries
        .into_iter()
        .filter(|entry| {
            !matches!(
                entry.event_kind(),
                EventKind::SYSTEM_OPEN_COMPLETED | EventKind::SYSTEM_CLOSE_COMPLETED
            )
        })
        .collect();
    let actual_set = actual(&filtered_entries);
    let expected = ground_truth(corpus, region);
    assert_eq!(
        actual_set, expected,
        "topology `{label}` query `{query_name}` mismatch.\n\
         expected={expected:?}\n\
         actual  ={actual_set:?}\n\
         region={region:?}"
    );
    assert_eq!(
        filtered_entries.len(),
        actual_set.len(),
        "topology `{label}` query `{query_name}` returned duplicate entries"
    );
}

fn assert_cursor_matches(
    label: &str,
    query_name: &str,
    region: &Region,
    batch_size: usize,
    store: &Store,
    corpus: &[GroundTruthEvent],
) {
    let expected = ground_truth_ordered(corpus, region);
    let mut cursor = store.cursor_guaranteed(region);
    let mut actual_entries = Vec::new();
    let max_batches = expected.len().saturating_add(4);

    for _ in 0..=max_batches {
        let batch = cursor.poll_batch(batch_size);
        if batch.is_empty() {
            let unique: HashSet<_> = actual_entries.iter().cloned().collect();
            assert_eq!(
                unique.len(),
                actual_entries.len(),
                "topology `{label}` cursor query `{query_name}` produced duplicates with batch_size={batch_size}"
            );
            assert_eq!(
                actual_entries, expected,
                "topology `{label}` cursor query `{query_name}` mismatch with batch_size={batch_size}.\n\
                 expected={expected:?}\n\
                 actual  ={actual_entries:?}\n\
                 region={region:?}"
            );
            return;
        }
        actual_entries.extend(actual_ordered(&batch));
    }
    unreachable!(
        "topology `{label}` cursor query `{query_name}` did not terminate within {max_batches} batches. \
         expected_len={}, actual_len={}, batch_size={batch_size}, region={region:?}",
        expected.len(),
        actual_entries.len()
    );
}

fn standard_queries() -> Vec<(&'static str, Region)> {
    vec![
        ("entity(alpha)", Region::entity("entity:alpha")),
        ("scope(X)", Region::scope("scope:X")),
        ("scope(Y)", Region::scope("scope:Y")),
        (
            "scope(X) + kind(5,1)",
            Region::scope("scope:X").with_fact(KindFilter::Exact(EventKind::custom(0x5, 1))),
        ),
        (
            "scope(Z) + kind(6,1) + clock(0..=3)",
            Region::scope("scope:Z")
                .with_fact(KindFilter::Exact(EventKind::custom(0x6, 1)))
                .with_clock_range(ClockRange::new(0, 3).expect("valid clock range")),
        ),
        (
            "kind(5,2)",
            Region::all().with_fact(KindFilter::Exact(EventKind::custom(0x5, 2))),
        ),
        (
            "category(5)",
            Region::all().with_fact(KindFilter::Category(0x5)),
        ),
        ("kind(Any)", Region::all().with_fact(KindFilter::Any)),
        (
            "clock(2..=5)",
            Region::all().with_clock_range(ClockRange::new(2, 5).expect("valid clock range")),
        ),
        (
            "entity(bravo) + scope(Y) + category(5) + clock(0..=2)",
            entity_scoped_region("entity:bravo", "scope:Y")
                .with_fact(KindFilter::Category(0x5))
                .with_clock_range(ClockRange::new(0, 2).expect("valid clock range")),
        ),
    ]
}

fn cursor_queries() -> Vec<(&'static str, Region)> {
    vec![
        ("all + any", Region::all().with_fact(KindFilter::Any)),
        (
            "scope(X) + kind(5,1)",
            Region::scope("scope:X").with_fact(KindFilter::Exact(EventKind::custom(0x5, 1))),
        ),
        (
            "entity(bravo) + clock(1..=6)",
            Region::entity("entity:bravo")
                .with_clock_range(ClockRange::new(1, 6).expect("valid clock range")),
        ),
        (
            "entity(alpha) + scope(Z) + category(5)",
            entity_scoped_region("entity:alpha", "scope:Z").with_fact(KindFilter::Category(0x5)),
        ),
    ]
}

fn assert_query_matrix(label: &str, store: &Store, corpus: &[GroundTruthEvent]) {
    for (query_name, region) in standard_queries() {
        assert_matches(label, query_name, &region, store, corpus);
    }
}

fn assert_cursor_query_matrix(label: &str, store: &Store, corpus: &[GroundTruthEvent]) {
    for (query_name, region) in cursor_queries() {
        for batch_size in [1usize, 3, 11] {
            assert_cursor_matches(label, query_name, &region, batch_size, store, corpus);
        }
    }
}

#[test]
fn overlays_return_ground_truth_for_every_filter_shape() {
    let corpus = build_corpus();

    for (label, topology) in topologies() {
        let dir = TempDir::new().expect("temp dir");
        let store = open_store(&dir, topology);
        seed_store_with_corpus(&store, &corpus);
        assert_query_matrix(label, &store, &corpus);
        store.close().expect("close");
    }
}

#[test]
fn cursor_batches_match_ground_truth_order_across_topologies() {
    let corpus = build_corpus();

    for (label, topology) in topologies() {
        let dir = TempDir::new().expect("temp dir");
        let store = open_store(&dir, topology);
        seed_store_with_corpus(&store, &corpus);
        assert_cursor_query_matrix(label, &store, &corpus);
        store.close().expect("close");
    }
}

#[test]
fn reopen_matches_live_oracle_across_topologies() {
    let corpus = build_corpus();

    for (label, topology) in topologies() {
        let dir = TempDir::new().expect("temp dir");
        let live = open_store(&dir, topology.clone());
        seed_store_with_corpus(&live, &corpus);
        let live_all = live.query(&Region::all());
        assert_query_matrix(label, &live, &corpus);
        assert_cursor_query_matrix(label, &live, &corpus);
        live.close().expect("close live store");

        let reopened = open_store(&dir, topology);
        let reopened_all = reopened.query(&Region::all());
        assert_query_matrix(label, &reopened, &corpus);
        assert_cursor_query_matrix(label, &reopened, &corpus);
        assert_eq!(
            actual(&reopened_all),
            actual(&live_all),
            "topology `{label}` reopen must preserve the same all-region visible set as the live build"
        );
        assert_eq!(
            actual_ordered(&reopened_all),
            actual_ordered(&live_all),
            "topology `{label}` reopen must preserve the same all-region order as the live build"
        );
        reopened.close().expect("close reopened store");
    }
}
