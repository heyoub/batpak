use batpak::coordinate::Coordinate;
use batpak::event::EventKind;
use batpak::store::{IndexTopology, Store, StoreConfig};
use tempfile::TempDir;

#[test]
fn topology_constructors_enable_expected_overlays() {
    let cases = [
        ("aos", IndexTopology::aos(), IndexTopology::aos()),
        (
            "scan",
            IndexTopology::scan(),
            IndexTopology::aos().with_soa(true),
        ),
        (
            "entity_local",
            IndexTopology::entity_local(),
            IndexTopology::aos().with_entity_groups(true),
        ),
        (
            "tiled",
            IndexTopology::tiled(),
            IndexTopology::aos().with_tiles64(true),
        ),
        (
            "all",
            IndexTopology::all(),
            IndexTopology::aos()
                .with_soa(true)
                .with_entity_groups(true)
                .with_tiles64(true),
        ),
    ];

    for (label, topology, expected) in cases {
        assert_eq!(
            topology, expected,
            "constructor `{label}` must enable the intended overlay set"
        );
    }
}

#[test]
fn default_topology_is_aos() {
    let topology = IndexTopology::default();
    assert_eq!(
        topology,
        IndexTopology::aos(),
        "default topology must be explicit base AoS only"
    );
}

#[test]
fn diagnostics_reports_topology_labels() {
    let cases = [
        ("aos", IndexTopology::aos()),
        ("scan", IndexTopology::scan()),
        ("entity-local", IndexTopology::entity_local()),
        ("tiled", IndexTopology::tiled()),
        ("all", IndexTopology::all()),
    ];

    for (label, topology) in cases {
        let dir = TempDir::new().expect("temp dir");
        let store = Store::open(
            StoreConfig::new(dir.path())
                .with_index_topology(topology)
                .with_enable_checkpoint(false)
                .with_enable_mmap_index(false),
        )
        .expect("open store");
        assert_eq!(
            store.diagnostics().index_topology,
            label,
            "diagnostics should expose the real topology label for `{label}`"
        );
        store.close().expect("close store");
    }
}

#[test]
fn diagnostics_reports_tile_count_only_for_tiled_topologies() {
    let cases = [
        ("aos", IndexTopology::aos(), false),
        ("scan", IndexTopology::scan(), false),
        ("entity-local", IndexTopology::entity_local(), false),
        ("tiled", IndexTopology::tiled(), true),
        ("all", IndexTopology::all(), true),
    ];

    for (label, topology, expects_tiles) in cases {
        let dir = TempDir::new().expect("temp dir");
        let store = Store::open(
            StoreConfig::new(dir.path())
                .with_index_topology(topology)
                .with_enable_checkpoint(false)
                .with_enable_mmap_index(false),
        )
        .expect("open store");

        let coord = Coordinate::new("entity:tile-proof", "scope:tile-proof").expect("coord");
        for i in 0..130 {
            store
                .append(
                    &coord,
                    EventKind::custom(0x1, 1),
                    &serde_json::json!({ "i": i }),
                )
                .expect("append");
        }
        store.sync().expect("sync");

        let diagnostics = store.diagnostics();
        if expects_tiles {
            assert!(
                diagnostics.tile_count > 0,
                "topology `{label}` should report live AoSoA64 tile usage once populated"
            );
        } else {
            assert_eq!(
                diagnostics.tile_count, 0,
                "topology `{label}` should not report tiled overlay cost"
            );
        }

        store.close().expect("close store");
    }
}

#[test]
fn store_config_debug_uses_topology_not_legacy_layout_terms() {
    let debug = format!(
        "{:?}",
        StoreConfig::new("tmp").with_index_topology(IndexTopology::all())
    );
    assert!(
        debug.contains("topology: IndexTopology"),
        "debug output should name the live topology type"
    );
    assert!(
        !debug.contains("layout:"),
        "debug output must not expose removed layout naming"
    );
    assert!(
        !debug.contains("views:"),
        "debug output must not expose removed view naming"
    );
}
