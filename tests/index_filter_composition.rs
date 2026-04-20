// justifies: INV-TEST-PANIC-AS-ASSERTION; tests in tests/index_filter_composition.rs rely on expect/panic on unreachable failures; clippy::unwrap_used and clippy::panic are the standard harness allowances for integration tests.
#![allow(clippy::unwrap_used, clippy::panic)]
//! Index filter composition across overlays.
//!
//! [INV-INDEX-FILTER-COMPOSE] For every supported overlay topology and every
//! combination of `Region` predicates (entity prefix, scope, kind filter,
//! clock range), the index returns exactly the same set of events as a
//! linear ground-truth scan over the same corpus. This pins B1 (filters
//! apply inside the overlay) and B4 (KindFilter::Any respects limit during
//! collection). Deterministic PRNG: one fixed seed, one shuffled corpus,
//! many queries.

use batpak::coordinate::{Coordinate, KindFilter, Region};
use batpak::event::EventKind;
use batpak::store::{IndexConfig, IndexEntry, IndexTopology, Store, StoreConfig};
use std::collections::HashSet;
use tempfile::TempDir;

const SEED_CORPUS_SIZE: usize = 120;

fn topologies() -> Vec<(&'static str, IndexTopology)> {
    vec![
        ("aos", IndexTopology::aos()),
        ("scan", IndexTopology::scan()),
        ("entity-local", IndexTopology::entity_local()),
        ("tiled", IndexTopology::tiled()),
        ("tiled_simd", IndexTopology::tiled_simd()),
        ("all", IndexTopology::all()),
    ]
}

fn open_store(dir: &TempDir, topology: IndexTopology) -> Store {
    let config = StoreConfig {
        index: IndexConfig {
            topology,
            incremental_projection: false,
            enable_checkpoint: false,
            enable_mmap_index: false,
        },
        ..StoreConfig::new(dir.path())
    }
    .with_sync_every_n_events(1);
    Store::open(config).expect("open store")
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
        store
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
                _ => panic!("reference model must be updated for new KindFilter variants"),
            };
            if !matches {
                continue;
            }
        }
        if let Some((lo, hi)) = region.clock_range() {
            if ev.clock_slot < lo || ev.clock_slot > hi {
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
        .map(|e| {
            (
                e.coord.entity().to_owned(),
                e.coord.scope().to_owned(),
                e.clock,
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
                    _ => panic!("reference model must be updated for new KindFilter variants"),
                };
                if !matches {
                    return None;
                }
            }
            if let Some((lo, hi)) = region.clock_range() {
                if ev.clock_slot < lo || ev.clock_slot > hi {
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
        .map(|e| {
            (
                e.global_sequence,
                e.coord.entity().to_owned(),
                e.coord.scope().to_owned(),
                e.clock,
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
    let actual_set = actual(&actual_entries);
    let expected = ground_truth(corpus, region);
    assert_eq!(
        actual_set, expected,
        "topology `{label}` query `{query_name}` mismatch.\n\
         expected={expected:?}\n\
         actual  ={actual_set:?}\n\
         region={region:?}"
    );
    assert_eq!(
        actual_entries.len(),
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

    loop {
        let batch = cursor.poll_batch(batch_size);
        if batch.is_empty() {
            break;
        }
        actual_entries.extend(actual_ordered(&batch));
    }

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
}

#[test]
fn overlays_return_ground_truth_for_every_filter_shape() {
    let corpus = build_corpus();

    for (label, topology) in topologies() {
        let dir = TempDir::new().expect("temp dir");
        let store = open_store(&dir, topology);
        seed_store_with_corpus(&store, &corpus);

        // entity prefix only
        assert_matches(
            label,
            "entity(alpha)",
            &Region::entity("entity:alpha"),
            &store,
            &corpus,
        );

        // scope only
        assert_matches(
            label,
            "scope(X)",
            &Region::scope("scope:X"),
            &store,
            &corpus,
        );
        assert_matches(
            label,
            "scope(Y)",
            &Region::scope("scope:Y"),
            &store,
            &corpus,
        );

        // scope + kind
        assert_matches(
            label,
            "scope(X) + kind(5,1)",
            &Region::scope("scope:X").with_fact(KindFilter::Exact(EventKind::custom(0x5, 1))),
            &store,
            &corpus,
        );

        // scope + kind + clock_range
        assert_matches(
            label,
            "scope(Z) + kind(6,1) + clock(0..=3)",
            &Region::scope("scope:Z")
                .with_fact(KindFilter::Exact(EventKind::custom(0x6, 1)))
                .with_clock_range((0, 3)),
            &store,
            &corpus,
        );

        // kind only
        assert_matches(
            label,
            "kind(5,2)",
            &Region::all().with_fact(KindFilter::Exact(EventKind::custom(0x5, 2))),
            &store,
            &corpus,
        );

        // category-only kind filter
        assert_matches(
            label,
            "category(5)",
            &Region::all().with_fact(KindFilter::Category(0x5)),
            &store,
            &corpus,
        );

        // KindFilter::Any — the B4 fix: limit applied during collection so
        // all entries round-trip when no limit is set.
        assert_matches(
            label,
            "kind(Any)",
            &Region::all().with_fact(KindFilter::Any),
            &store,
            &corpus,
        );

        // clock_range only (entity/scope unconstrained)
        assert_matches(
            label,
            "clock(2..=5)",
            &Region::all().with_clock_range((2, 5)),
            &store,
            &corpus,
        );

        // combined: entity + scope + category + clock_range
        assert_matches(
            label,
            "entity(bravo) + scope(Y) + category(5) + clock(0..=2)",
            &Region::entity("entity:bravo")
                .with_scope("scope:Y")
                .with_fact(KindFilter::Category(0x5))
                .with_clock_range((0, 2)),
            &store,
            &corpus,
        );

        store.close().expect("close");
    }
}

#[test]
fn cursor_batches_match_ground_truth_order_across_topologies() {
    let corpus = build_corpus();
    let batch_sizes = [1usize, 3, 11];
    let queries = vec![
        ("all + any", Region::all().with_fact(KindFilter::Any)),
        (
            "scope(X) + kind(5,1)",
            Region::scope("scope:X").with_fact(KindFilter::Exact(EventKind::custom(0x5, 1))),
        ),
        (
            "entity(bravo) + clock(1..=6)",
            Region::entity("entity:bravo").with_clock_range((1, 6)),
        ),
        (
            "entity(alpha) + scope(Z) + category(5)",
            Region::entity("entity:alpha")
                .with_scope("scope:Z")
                .with_fact(KindFilter::Category(0x5)),
        ),
    ];

    for (label, topology) in topologies() {
        let dir = TempDir::new().expect("temp dir");
        let store = open_store(&dir, topology);
        seed_store_with_corpus(&store, &corpus);

        for (query_name, region) in &queries {
            for batch_size in batch_sizes {
                assert_cursor_matches(label, query_name, region, batch_size, &store, &corpus);
            }
        }

        store.close().expect("close");
    }
}
