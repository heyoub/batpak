//! GAUNTLET semantic_diff family (GAUNT-SEMDIFF-1).
//!
//! Invariant: INV-SEMANTIC-DIFF-EQUIVALENCE.
//!
//! Every pair of store configurations that CLAIM to expose identical visible
//! truth must, when fed the SAME seeded operation stream, produce byte-identical
//! observables: query results across every region shape, the visible HLC, and the
//! global sequence. A divergence between an equivalence-claiming pair is a hard
//! finding — the cheap "island syndrome" killer (the audit found cached==uncached
//! and the mmap/checkpoint/projection equivalences un-tested).
//!
//! Equivalence axes covered (each variant compared against the scan/no-index
//! baseline over the same stream):
//!   - mmap index ON vs OFF            (mmap <-> scan)
//!   - checkpoint ON vs OFF            (checkpoint <-> rebuilt-from-scan)
//!   - incremental projection ON/OFF   (fused <-> unfused)
//!   - fast path (mmap+checkpoint) vs scan path (combined)
//!   - reopened (cold start) across representations (cold-start planner must agree)
//!   - cached vs uncached              (a re-query of a warmed store must agree)
//!
//! Determinism: every store runs on a FIXED injected clock so HLC coordinates are
//! reproducible across the paired runs (the only nondeterminism removed is the one
//! that is NOT under test — real wall time).
//!
//! NOT covered here (honest scope): debug<->release equivalence needs two build
//! profiles, so it cannot run inside one process; it is a CI build-matrix concern,
//! not a single-test property. Tracked for the perf/CI matrix, not faked here.

mod support;
use batpak::store::index::IndexEntry;
use batpak::store::{HlcPoint, Store, StoreConfig};
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
use support::prelude::*;
use tempfile::TempDir;

#[path = "common/proptest.rs"]
mod proptest_support;

/// Fixed wall-clock (ms) for every store so HLC coordinates are reproducible
/// across paired runs. Degenerates HLC to its logical counter, which must agree
/// for an identical operation stream regardless of index representation.
const FIXED_WALL_MS: i64 = 1_700_000_000_000;

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

/// The full observable surface two equivalence-claiming configs must agree on.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Snapshot {
    all: Vec<EventSummary>,
    streams: BTreeMap<String, Vec<EventSummary>>,
    scopes: BTreeMap<String, Vec<EventSummary>>,
    categories: BTreeMap<u8, Vec<EventSummary>>,
    /// Visible-truth frontier (deterministic under the fixed clock).
    visible_hlc: HlcPoint,
    /// Monotonic global sequence high-water.
    global_sequence: u64,
    event_count: usize,
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
        1..20,
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

/// An equivalence-claiming config: the index representation varies, the visible
/// truth must not. Fixed clock makes HLC reproducible across paired runs.
fn equiv_config(dir: &TempDir, mmap: bool, checkpoint: bool, incremental: bool) -> StoreConfig {
    StoreConfig::new(dir.path())
        .with_enable_mmap_index(mmap)
        .with_enable_checkpoint(checkpoint)
        .with_incremental_projection(incremental)
        .with_sync_every_n_events(8)
        .with_clock_fn(|| FIXED_WALL_MS)
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
    // sync() guarantees DURABILITY; visibility advances on a separate watermark.
    // Wait until the visible frontier reaches everything written so the snapshot
    // is taken at a SETTLED state — otherwise the capture races the watermark and
    // the comparison is timing-dependent (not a real path divergence).
    let written = store.frontier().written_hlc;
    store.wait_for_visible(written, std::time::Duration::from_secs(10))?;
    Ok(())
}

fn summarize_entries<State: batpak::store::StoreState>(
    store: &Store<State>,
    entries: Vec<IndexEntry>,
) -> Vec<EventSummary> {
    entries
        .into_iter()
        .filter(|entry| {
            entry.event_kind() != EventKind::SYSTEM_CLOSE_COMPLETED
                && entry.event_kind() != EventKind::SYSTEM_OPEN_COMPLETED
        })
        .map(|entry| {
            let payload = store
                .get(batpak::id::EventId::from(entry.event_id()))
                .expect("query result must be readable from disk")
                .event
                .payload;
            EventSummary {
                entity: entry.coord().entity().to_owned(),
                scope: entry.coord().scope().to_owned(),
                category: entry.event_kind().category(),
                type_id: entry.event_kind().type_id(),
                global_sequence: entry.global_sequence(),
                clock: entry.clock(),
                payload,
            }
        })
        .collect()
}

fn capture_snapshot<State: batpak::store::StoreState>(
    store: &Store<State>,
    specs: &[AppendSpec],
) -> Snapshot {
    let mut entities = BTreeSet::new();
    let mut scope_names = BTreeSet::new();
    let mut categories = BTreeSet::new();
    for spec in specs {
        entities.insert(entity_name(spec.entity_idx));
        scope_names.insert(scope_name(spec.scope_idx));
        categories.insert(spec.category);
    }

    let all = summarize_entries(store, store.query(&Region::all()));
    let streams = entities
        .iter()
        .map(|e| (e.clone(), summarize_entries(store, store.by_entity(e))))
        .collect();
    let scopes = scope_names
        .iter()
        .map(|s| (s.clone(), summarize_entries(store, store.by_scope(s))))
        .collect();
    let categories = categories
        .iter()
        .map(|c| {
            (
                *c,
                summarize_entries(store, store.query(&Region::all().with_fact_category(*c))),
            )
        })
        .collect();

    let frontier = store.frontier();
    let stats = store.stats();
    Snapshot {
        all,
        streams,
        scopes,
        categories,
        visible_hlc: frontier.visible_hlc,
        global_sequence: stats.global_sequence,
        event_count: stats.event_count,
    }
}

fn assert_equivalent(label: &str, baseline: &Snapshot, candidate: &Snapshot) {
    assert_eq!(
        candidate, baseline,
        "PROPERTY (semantic_diff): the `{label}` config claims to expose identical visible truth as \
         the scan baseline but DIVERGED on the same operation stream. An equivalence-claiming pair \
         that disagrees is a hard finding (island syndrome / silent path bug)."
    );
}

/// The equivalence-claiming variants, each compared against the scan baseline.
fn variants() -> Vec<(&'static str, bool, bool, bool)> {
    vec![
        ("mmap-on", true, false, false),
        ("checkpoint-on", false, true, false),
        ("incremental-projection-on", false, false, true),
        ("fast-path-mmap+checkpoint", true, true, false),
        ("all-on", true, true, true),
    ]
}

proptest! {
    #![proptest_config(proptest_support::cfg(16))]

    /// Every equivalence-claiming config agrees with the scan baseline on the same
    /// seeded stream — fresh open.
    #[test]
    fn equivalence_claiming_configs_agree_on_visible_truth(specs in arb_append_specs()) {
        let base_dir = TempDir::new().expect("temp dir");
        let baseline_store = Store::open(equiv_config(&base_dir, false, false, false)).expect("open baseline");
        populate(&baseline_store, &specs).expect("populate baseline");
        let baseline = capture_snapshot(&baseline_store, &specs);
        baseline_store.close().expect("close baseline");

        for (label, mmap, checkpoint, incremental) in variants() {
            let dir = TempDir::new().expect("temp dir");
            let store = Store::open(equiv_config(&dir, mmap, checkpoint, incremental)).expect("open variant");
            populate(&store, &specs).expect("populate variant");
            let snapshot = capture_snapshot(&store, &specs);
            assert_equivalent(label, &baseline, &snapshot);
            store.close().expect("close variant");
        }
    }

    /// Reopened (cold-start) stores agree across index representations: the
    /// cold-start planner (mmap-load / checkpoint-decode / scan-rebuild) must
    /// reconstruct identical visible truth. The baseline goes through the SAME
    /// open->close->reopen lifecycle as the variants, so the frontier/counter
    /// fields (which legitimately count the extra SYSTEM_OPEN/CLOSE lifecycle
    /// events) are compared apples-to-apples — only the index representation differs.
    #[test]
    fn reopened_configs_agree_across_representations(specs in arb_append_specs()) {
        let reopen_and_capture = |mmap, checkpoint, incremental| {
            let dir = TempDir::new().expect("temp dir");
            let config = equiv_config(&dir, mmap, checkpoint, incremental);
            let store = Store::open(config.clone()).expect("open");
            populate(&store, &specs).expect("populate");
            store.close().expect("close");
            let reopened = Store::open(config).expect("reopen");
            let snapshot = capture_snapshot(&reopened, &specs);
            reopened.close().expect("close reopened");
            // Keep `dir` alive until after capture by returning it with the snapshot.
            (dir, snapshot)
        };

        let (_base_dir, baseline) = reopen_and_capture(false, false, false);
        for (label, mmap, checkpoint, incremental) in variants() {
            let (_dir, snapshot) = reopen_and_capture(mmap, checkpoint, incremental);
            assert_equivalent(label, &baseline, &snapshot);
        }
    }

    /// cached <-> uncached: a re-query of a warmed store returns identical results
    /// to the first (cold) query.
    #[test]
    fn cached_requery_agrees_with_cold_query(specs in arb_append_specs()) {
        let dir = TempDir::new().expect("temp dir");
        let store = Store::open(equiv_config(&dir, true, false, false)).expect("open store");
        populate(&store, &specs).expect("populate store");
        let cold = capture_snapshot(&store, &specs);
        let warm = capture_snapshot(&store, &specs);
        assert_equivalent("cached-requery", &cold, &warm);
        store.close().expect("close store");
    }
}

/// Under `--cfg gauntlet_red_fixture`, store B is fed one EXTRA operation, so its
/// visible truth diverges from A and the equivalence assertion must FAIL. Under
/// normal builds B's stream equals A's, so the configs agree. This is the
/// anti-vacuous ProductionFlip fixture proving the diff actually catches a
/// divergence (the `gauntlet-red-fixtures-bite` lane asserts it reds).
#[cfg(not(gauntlet_red_fixture))]
fn divergence_stream(base: &[AppendSpec]) -> Vec<AppendSpec> {
    base.to_vec()
}

#[cfg(gauntlet_red_fixture)]
fn divergence_stream(base: &[AppendSpec]) -> Vec<AppendSpec> {
    let mut specs = base.to_vec();
    specs.push(AppendSpec {
        entity_idx: 0,
        scope_idx: 0,
        category: 0x1,
        type_id: 1,
        payload: 12345,
    });
    specs
}

#[test]
fn semantic_diff_detects_planted_divergence() {
    let base = vec![
        AppendSpec {
            entity_idx: 1,
            scope_idx: 0,
            category: 0x1,
            type_id: 2,
            payload: 7,
        },
        AppendSpec {
            entity_idx: 2,
            scope_idx: 1,
            category: 0x2,
            type_id: 3,
            payload: -4,
        },
    ];

    let dir_a = TempDir::new().expect("temp dir");
    let store_a = Store::open(equiv_config(&dir_a, false, false, false)).expect("open A");
    populate(&store_a, &base).expect("populate A");
    let snap_a = capture_snapshot(&store_a, &base);
    store_a.close().expect("close A");

    // B claims equivalence (different index representation). Its stream equals A's
    // in a normal build; under the red cfg it carries one extra op.
    let dir_b = TempDir::new().expect("temp dir");
    let store_b = Store::open(equiv_config(&dir_b, true, true, true)).expect("open B");
    populate(&store_b, &divergence_stream(&base)).expect("populate B");
    let snap_b = capture_snapshot(&store_b, &base);
    store_b.close().expect("close B");

    assert_equivalent("planted-divergence-B", &snap_a, &snap_b);
}
