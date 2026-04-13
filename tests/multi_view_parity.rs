//! Property tests proving that all view topologies agree on query results.

use batpak::coordinate::KindFilter;
use batpak::prelude::*;
use batpak::store::{Store, StoreConfig, ViewConfig};
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
use tempfile::TempDir;

mod common;

#[derive(Clone, Debug)]
struct AppendSpec {
    entity_idx: u8,
    scope_idx: u8,
    category: u8,
    type_id: u16,
    payload: i16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EventSummary {
    entity: String,
    scope: String,
    category: u8,
    type_id: u16,
    global_sequence: u64,
    clock: u32,
    payload: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct QuerySnapshot {
    all: Vec<EventSummary>,
    streams: BTreeMap<String, Vec<EventSummary>>,
    scopes: BTreeMap<String, Vec<EventSummary>>,
    exact_kinds: BTreeMap<String, Vec<EventSummary>>,
    categories: BTreeMap<u8, Vec<EventSummary>>,
    scoped_exact: BTreeMap<String, Vec<EventSummary>>,
}

fn arb_append_specs() -> impl Strategy<Value = Vec<AppendSpec>> {
    prop::collection::vec(
        (
            0u8..4,
            0u8..3,
            prop_oneof![Just(0x1u8), Just(0x2u8), Just(0xFu8)],
            1u16..8,
            any::<i16>(),
        )
            .prop_map(
                |(entity_idx, scope_idx, category, type_id, payload)| AppendSpec {
                    entity_idx,
                    scope_idx,
                    category,
                    type_id,
                    payload,
                },
            ),
        1..24,
    )
}

fn entity_name(idx: u8) -> String {
    format!("entity:{idx:02}")
}

fn scope_name(idx: u8) -> String {
    format!("scope:{idx:02}")
}

fn event_kind(spec: &AppendSpec) -> EventKind {
    EventKind::custom(spec.category, spec.type_id)
}

fn store_config(dir: &TempDir, views: ViewConfig) -> StoreConfig {
    StoreConfig::new(dir.path())
        .with_views(views)
        .with_index_layout(IndexLayout::AoS)
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false)
        .with_sync_every_n_events(64)
}

fn compatibility_tiled_config(dir: &TempDir, layout: IndexLayout) -> StoreConfig {
    StoreConfig::new(dir.path())
        .with_views(ViewConfig::none())
        .with_index_layout(layout)
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false)
        .with_sync_every_n_events(64)
}

fn populate(store: &Store, specs: &[AppendSpec]) -> Result<(), StoreError> {
    for spec in specs {
        let coord = Coordinate::new(entity_name(spec.entity_idx), scope_name(spec.scope_idx))
            .expect("generated coordinates must be valid");
        store.append(
            &coord,
            event_kind(spec),
            &serde_json::json!({
                "entity_idx": spec.entity_idx,
                "scope_idx": spec.scope_idx,
                "payload": spec.payload,
            }),
        )?;
    }
    store.sync()?;
    Ok(())
}

fn summarize_entries<State>(store: &Store<State>, entries: Vec<IndexEntry>) -> Vec<EventSummary> {
    entries
        .into_iter()
        .map(|entry| {
            let payload = store
                .get(entry.event_id)
                .expect("query result must be readable from disk")
                .event
                .payload;
            EventSummary {
                entity: entry.coord.entity().to_owned(),
                scope: entry.coord.scope().to_owned(),
                category: entry.kind.category(),
                type_id: entry.kind.type_id(),
                global_sequence: entry.global_sequence,
                clock: entry.clock,
                payload,
            }
        })
        .collect()
}

fn capture_snapshot<State>(store: &Store<State>, specs: &[AppendSpec]) -> QuerySnapshot {
    let mut entities = BTreeSet::new();
    let mut scope_names = BTreeSet::new();
    let mut exact_kind_keys = BTreeSet::new();
    let mut categories = BTreeSet::new();

    for spec in specs {
        entities.insert(entity_name(spec.entity_idx));
        scope_names.insert(scope_name(spec.scope_idx));
        exact_kind_keys.insert((spec.category, spec.type_id));
        categories.insert(spec.category);
    }

    let all = summarize_entries(store, store.query(&Region::all()));

    let streams = entities
        .iter()
        .map(|entity| {
            (
                entity.clone(),
                summarize_entries(store, store.stream(entity)),
            )
        })
        .collect();

    let scopes = scope_names
        .iter()
        .map(|scope| {
            (
                scope.clone(),
                summarize_entries(store, store.by_scope(scope)),
            )
        })
        .collect();

    let exact_kinds = exact_kind_keys
        .iter()
        .map(|(category, type_id)| {
            let kind = EventKind::custom(*category, *type_id);
            (
                format!("{category:02x}:{type_id:04x}"),
                summarize_entries(store, store.by_fact(kind)),
            )
        })
        .collect();

    let categories = categories
        .iter()
        .map(|category| {
            (
                *category,
                summarize_entries(
                    store,
                    store.query(&Region::all().with_fact_category(*category)),
                ),
            )
        })
        .collect();

    let mut scoped_exact = BTreeMap::new();
    for scope in &scope_names {
        for (category, type_id) in &exact_kind_keys {
            let kind = EventKind::custom(*category, *type_id);
            let key = format!("{scope}|{category:02x}:{type_id:04x}");
            let results = store.query(&Region::scope(scope).with_fact(KindFilter::Exact(kind)));
            scoped_exact.insert(key, summarize_entries(store, results));
        }
    }

    QuerySnapshot {
        all,
        streams,
        scopes,
        exact_kinds,
        categories,
        scoped_exact,
    }
}

fn assert_snapshot_matches(
    label: &str,
    baseline_label: &str,
    baseline: &QuerySnapshot,
    candidate: &QuerySnapshot,
) {
    // Vec<EventSummary> equality is order-sensitive: this assertion proves
    // both set-equivalence AND ordering-equivalence across view topologies.
    assert_eq!(
        candidate, baseline,
        "PROPERTY: query results must match across view topologies.\n\
         baseline={baseline_label}, candidate={label}.\n\
         This defends the multi-view routing invariant: base AoS, SoA, SoAoS, tiled, and fully-enabled stores must expose identical visible truth."
    );
}

proptest! {
    #![proptest_config(common::proptest::cfg(16))]

    #[test]
    fn all_view_topologies_return_identical_query_results(specs in arb_append_specs()) {
        let configs = vec![
            ("aos", store_config(&TempDir::new().expect("temp dir"), ViewConfig::none())),
            ("soa-only", store_config(&TempDir::new().expect("temp dir"), ViewConfig::none().with_soa(true))),
            ("entity-groups-only", store_config(&TempDir::new().expect("temp dir"), ViewConfig::none().with_entity_groups(true))),
            ("tiles-only", store_config(&TempDir::new().expect("temp dir"), ViewConfig::none().with_tiles64(true))),
            ("all-views", store_config(&TempDir::new().expect("temp dir"), ViewConfig::all())),
            ("compat-aosoa8", compatibility_tiled_config(&TempDir::new().expect("temp dir"), IndexLayout::AoSoA8)),
            ("compat-aosoa16", compatibility_tiled_config(&TempDir::new().expect("temp dir"), IndexLayout::AoSoA16)),
        ];

        let mut snapshots = Vec::new();
        for (label, config) in configs {
            let store = Store::open(config).expect("open store");
            populate(&store, &specs).expect("populate store");
            let snapshot = capture_snapshot(&store, &specs);
            snapshots.push((label, snapshot));
            store.close().expect("close store");
        }

        let (baseline_label, baseline) = snapshots
            .first()
            .expect("at least one topology must be tested");
        for (label, snapshot) in snapshots.iter().skip(1) {
            assert_snapshot_matches(label, baseline_label, baseline, snapshot);
        }
    }

    #[test]
    fn reopened_view_topologies_return_identical_query_results(specs in arb_append_specs()) {
        let configs = vec![
            ("aos", store_config(&TempDir::new().expect("temp dir"), ViewConfig::none())),
            ("soa-only", store_config(&TempDir::new().expect("temp dir"), ViewConfig::none().with_soa(true))),
            ("entity-groups-only", store_config(&TempDir::new().expect("temp dir"), ViewConfig::none().with_entity_groups(true))),
            ("tiles-only", store_config(&TempDir::new().expect("temp dir"), ViewConfig::none().with_tiles64(true))),
            ("all-views", store_config(&TempDir::new().expect("temp dir"), ViewConfig::all())),
        ];

        let mut snapshots = Vec::new();
        for (label, config) in configs {
            let store = Store::open(config.clone()).expect("open store");
            populate(&store, &specs).expect("populate store");
            store.close().expect("close store");

            let reopened = Store::open(config).expect("reopen store");
            let snapshot = capture_snapshot(&reopened, &specs);
            snapshots.push((label, snapshot));
            reopened.close().expect("close reopened store");
        }

        let (baseline_label, baseline) = snapshots
            .first()
            .expect("at least one topology must be tested");
        for (label, snapshot) in snapshots.iter().skip(1) {
            assert_snapshot_matches(label, baseline_label, baseline, snapshot);
        }
    }
}
