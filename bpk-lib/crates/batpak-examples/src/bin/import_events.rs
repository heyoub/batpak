//! # import_events
//!
//! **Teaches:** re-apply events from a source store into a destination with import provenance.
//!
//! Run: `cargo run -p batpak-examples --bin import_events`

use batpak::prelude::*;
use batpak::store::{
    provenance_from_extensions, ImportOptions, ImportSelector, Store, StoreConfig,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();

    let source_dir = tempfile::tempdir()?;
    let source = Store::open(StoreConfig::new(source_dir.path()))?;
    let dest_dir = tempfile::tempdir()?;
    let dest = Store::open(StoreConfig::new(dest_dir.path()))?;

    let coord = Coordinate::new("entity:import-demo", "scope:example")?;
    let kind = EventKind::custom(0xF, 0x02);
    let receipt = source.append(&coord, kind, &serde_json::json!({ "label": "alpha" }))?;

    let options = ImportOptions::new("example-source")?;
    let report = dest.import_events(&source, &ImportSelector::all(), &options)?;
    let _ = writeln!(
        out,
        "imported {} deduplicated {} source event {:032x}",
        report.imported,
        report.deduplicated,
        u128::from(receipt.event_id),
    );

    let imported = dest.by_entity("entity:import-demo");
    if let Some(entry) = imported.first() {
        if let Some(body) = provenance_from_extensions(entry.receipt_extensions()) {
            let _ = writeln!(
                out,
                "provenance namespace {} source_seq {}",
                body.source_namespace.as_str(),
                body.source_global_sequence,
            );
        }
    }

    let replay = dest.import_events(&source, &ImportSelector::all(), &options)?;
    assert_eq!(replay.imported, 0);
    assert_eq!(replay.deduplicated, 1);

    source.close()?;
    dest.close()?;
    Ok(())
}
