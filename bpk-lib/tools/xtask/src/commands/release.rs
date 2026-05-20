use super::ci;
use crate::docs;
use crate::publish::RELEASE_CHAIN;
use crate::util::{cargo, cargo_target_dir, repo_root, run};
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
    let smoke_root = cargo_target_dir()?.join("consumer-smoke");
    if smoke_root.exists() {
        fs::remove_dir_all(&smoke_root).context("clear target/consumer-smoke")?;
    }

    let packaged_root = smoke_root.join("packaged");
    let consumer_root = smoke_root.join("consumer");
    fs::create_dir_all(&packaged_root).context("create packaged crate dir")?;
    fs::create_dir_all(consumer_root.join("src")).context("create consumer src dir")?;

    let support_archive = package_crate(&root, "batpak-macros-support", &[])?;
    let macros_archive = package_crate(
        &root,
        "batpak-macros",
        &[("batpak-macros-support", "crates/macros-support")],
    )?;
    let bench_support_archive = package_crate(&root, "batpak-bench-support", &[])?;
    let batpak_archive = package_crate(
        &root,
        "batpak",
        &[
            ("batpak-macros-support", "crates/macros-support"),
            ("batpak-macros", "crates/macros"),
            ("batpak-bench-support", "crates/bench-support"),
        ],
    )?;

    let support_name = unpack_crate(&packaged_root, &support_archive)?;
    let macros_name = unpack_crate(&packaged_root, &macros_archive)?;
    let bench_support_name = unpack_crate(&packaged_root, &bench_support_archive)?;
    let unpacked_name = unpack_crate(&packaged_root, &batpak_archive)?;

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
             batpak = {{ path = \"../packaged/{unpacked_name}\", features = [\"blake3\"] }}\n\
             serde = {{ version = \"1\", features = [\"derive\"] }}\n\
             \n\
             [patch.crates-io]\n\
             batpak-macros-support = {{ path = \"../packaged/{support_name}\" }}\n\
             batpak-macros = {{ path = \"../packaged/{macros_name}\" }}\n\
             batpak-bench-support = {{ path = \"../packaged/{bench_support_name}\" }}\n"
        ),
    )
    .context("write consumer smoke manifest")?;
    fs::write(
        consumer_root.join("src").join("main.rs"),
        "use batpak::prelude::*;\n\
         \n\
         #[derive(serde::Serialize, serde::Deserialize, EventPayload)]\n\
         #[batpak(category = 0xF, type_id = 1)]\n\
         struct ConsumerSmokePayload {\n\
         \x20   value: String,\n\
         }\n\
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
         \x20   validate_event_payload_registry()?;\n\
         \x20   let store = Store::open(config)?;\n\
         \x20   let coord = Coordinate::new(\"consumer:smoke\", \"scope:packaged\")?;\n\
         \x20   let receipt = store.append_typed(&coord, &ConsumerSmokePayload { value: \"payload\".into() })?;\n\
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
        .env(
            "CARGO_TARGET_DIR",
            cargo_target_dir()?.join("consumer-smoke-build"),
        )
        .args(["run", "--quiet"]);
    run(cargo_run)
}

pub(crate) fn release(args: ReleaseArgs) -> Result<()> {
    ci()?;
    // 0.7.6: semver-checks runs in STRICT mode in the release path.
    // Any breaking-change tripwire fails the release; advisory mode was
    // a 0.7.5 carry-over that silently shipped breakage when the tool
    // was installed but its findings were swallowed.
    crate::public_api::semver_check(crate::SemverCheckArgs { strict: true })?;
    crate::public_api::public_api(crate::PublicApiArgs {
        strict: true,
        check_baseline: true,
        bless_baseline: false,
    })?;
    consumer_smoke()?;
    docs::docs(DocsArgs { open: false })?;
    if args.dry_run {
        let root = repo_root()?;
        for package in RELEASE_CHAIN {
            let mut publish = Command::new("cargo");
            publish
                .current_dir(&root)
                .args(["publish", "-p", package, "--dry-run", "--allow-dirty"])
                .arg("--config")
                .arg("patch.crates-io.batpak-macros-support.path=\"crates/macros-support\"")
                .arg("--config")
                .arg("patch.crates-io.batpak-macros.path=\"crates/macros\"")
                .arg("--config")
                .arg("patch.crates-io.batpak-bench-support.path=\"crates/bench-support\"")
                .arg("--config")
                .arg("patch.crates-io.syncbat-macros.path=\"crates/syncbat-macros\"")
                .arg("--config")
                .arg("patch.crates-io.batpak.path=\"crates/core\"")
                .arg("--config")
                .arg("patch.crates-io.syncbat.path=\"crates/syncbat\"");
            run(publish)?;
        }
        Ok(())
    } else {
        bail!("release without --dry-run is intentionally disabled in xtask")
    }
}

fn package_crate(root: &Path, package: &str, patches: &[(&str, &str)]) -> Result<PathBuf> {
    let mut cargo_package = Command::new("cargo");
    cargo_package.current_dir(root).args([
        "package",
        "-p",
        package,
        "--allow-dirty",
        "--no-verify",
    ]);
    for (name, path) in patches {
        cargo_package
            .arg("--config")
            .arg(format!("patch.crates-io.{name}.path=\"{path}\""));
    }
    run(cargo_package)?;
    latest_packaged_crate(&cargo_target_dir()?.join("package"), package)
}

fn unpack_crate(packaged_root: &Path, archive: &Path) -> Result<String> {
    let mut unpack = Command::new("tar");
    unpack.current_dir(packaged_root).arg("xf").arg(archive);
    run(unpack)?;
    archive
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".crate"))
        .map(str::to_owned)
        .with_context(|| format!("derive unpacked crate dir from {}", archive.display()))
}

fn latest_packaged_crate(package_dir: &Path, package: &str) -> Result<PathBuf> {
    let mut latest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(package_dir)
        .with_context(|| format!("read packaged crate directory {}", package_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !path.is_file() || !is_package_archive(file_name, package) {
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
        .with_context(|| format!("could not locate packaged {package} .crate archive"))
}

fn is_package_archive(file_name: &str, package: &str) -> bool {
    let Some(rest) = file_name
        .strip_prefix(package)
        .and_then(|rest| rest.strip_prefix('-'))
        .and_then(|rest| rest.strip_suffix(".crate"))
    else {
        return false;
    };
    rest.chars().next().is_some_and(|ch| ch.is_ascii_digit())
}
