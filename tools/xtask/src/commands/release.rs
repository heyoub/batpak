use super::ci;
use crate::docs;
use crate::util::{cargo, repo_root, run};
use crate::{DocsArgs, ReleaseArgs};
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) fn quickstart() -> Result<()> {
    cargo(["run", "--example", "quickstart"])
}

pub(crate) fn consumer_smoke() -> Result<()> {
    let root = repo_root()?;
    let smoke_root = root.join("target").join("consumer-smoke");
    if smoke_root.exists() {
        fs::remove_dir_all(&smoke_root).context("clear target/consumer-smoke")?;
    }

    let packaged_root = smoke_root.join("packaged");
    let consumer_root = smoke_root.join("consumer");
    fs::create_dir_all(&packaged_root).context("create packaged crate dir")?;
    fs::create_dir_all(consumer_root.join("src")).context("create consumer src dir")?;

    let mut cargo_package = Command::new("cargo");
    cargo_package
        .current_dir(&root)
        .args(["package", "--allow-dirty", "--no-verify"]);
    run(cargo_package)?;

    let archive = latest_packaged_crate(&root.join("target").join("package"))?;
    let mut unpack = Command::new("tar");
    unpack.current_dir(&packaged_root).arg("xf").arg(&archive);
    run(unpack)?;

    let unpacked_name = unpacked_package_dir(&packaged_root)?;

    fs::write(
        consumer_root.join("Cargo.toml"),
        format!(
            "[package]\n\
             name = \"batpak-consumer-smoke\"\n\
             version = \"0.1.0\"\n\
             edition = \"2021\"\n\
             publish = false\n\
             \n\
             [workspace]\n\
             \n\
             [dependencies]\n\
             batpak = {{ path = \"../packaged/{unpacked_name}\", features = [\"blake3\"] }}\n"
        ),
    )
    .context("write consumer smoke manifest")?;
    fs::write(
        consumer_root.join("src").join("main.rs"),
        "use batpak::prelude::*;\n\
         \n\
         fn main() -> Result<(), Box<dyn std::error::Error>> {\n\
         \x20   let dir = std::env::temp_dir().join(format!(\"batpak-consumer-smoke-{}\", std::process::id()));\n\
         \x20   if dir.exists() {\n\
         \x20       std::fs::remove_dir_all(&dir)?;\n\
         \x20   }\n\
         \x20   std::fs::create_dir_all(&dir)?;\n\
         \n\
         \x20   let config = StoreConfig::new(&dir)\n\
         \x20       .with_sync_every_n_events(1)\n\
         \x20       .with_sync_mode(SyncMode::SyncData);\n\
         \x20   let store = Store::open(config)?;\n\
         \x20   let coord = Coordinate::new(\"consumer:smoke\", \"scope:packaged\")?;\n\
         \x20   let receipt = store.append(&coord, EventKind::custom(0xF, 1), &\"payload\")?;\n\
         \x20   let fetched = store.get(receipt.event_id)?;\n\
         \x20   assert_eq!(fetched.coordinate.scope(), \"scope:packaged\");\n\
         \x20   store.close()?;\n\
         \x20   std::fs::remove_dir_all(&dir)?;\n\
         \x20   Ok(())\n\
         }\n",
    )
    .context("write consumer smoke source")?;

    let mut cargo_run = Command::new("cargo");
    cargo_run
        .current_dir(&consumer_root)
        .args(["run", "--quiet"]);
    run(cargo_run)
}

pub(crate) fn release(args: ReleaseArgs) -> Result<()> {
    ci()?;
    consumer_smoke()?;
    docs::docs(DocsArgs { open: false })?;
    if args.dry_run {
        cargo(["publish", "--dry-run", "--allow-dirty"])
    } else {
        bail!("release without --dry-run is intentionally disabled in xtask")
    }
}

fn latest_packaged_crate(package_dir: &Path) -> Result<PathBuf> {
    let mut latest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(package_dir)
        .with_context(|| format!("read packaged crate directory {}", package_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !path.is_file() || !file_name.starts_with("batpak-") || !file_name.ends_with(".crate") {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .with_context(|| format!("read modified time for {}", path.display()))?;
        match &latest {
            Some((current, _)) if modified <= *current => {}
            _ => latest = Some((modified, path)),
        }
    }

    latest
        .map(|(_, path)| path)
        .context("could not locate packaged batpak .crate archive")
}

fn unpacked_package_dir(packaged_root: &Path) -> Result<String> {
    let mut unpacked = None;
    for entry in fs::read_dir(packaged_root)
        .with_context(|| format!("read unpacked package dir {}", packaged_root.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if entry.path().join("Cargo.toml").is_file() {
            unpacked = Some(entry.file_name().to_string_lossy().into_owned());
            break;
        }
    }

    unpacked.context("could not locate unpacked batpak package directory")
}
