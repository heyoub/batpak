//! `cargo xtask export-ts-manifest` — render the BatPAK TypeScript SDK
//! manifest by calling [`hbat::manifest::descriptors`] and wrapping the
//! result with protocol-level metadata.
//!
//! The shape of the emitted JSON is the source-of-truth for the
//! TypeScript codegen package. See the plan in
//! `/root/.claude/plans/yes-this-is-the-warm-finch.md` and the contract
//! documented at the top of `crates/hbat/src/manifest.rs`.

use crate::ExportTsManifestArgs;
use anyhow::{Context, Result};
use serde::Serialize;
use std::fs;

/// Manifest version emitted in the JSON envelope. Bumping this is a
/// protocol-level change; the TypeScript codegen will refuse to consume
/// an unsupported version.
const MANIFEST_VERSION: u32 = hbat::manifest::MANIFEST_VERSION;

/// `rmp-serde` version that the substrate canonical encoder is locked to.
/// Mirrors the `=` pin in `bpk-lib/crates/core/Cargo.toml`. Bumping this
/// requires an ADR-0019 review on the Rust side and a coordinated codec
/// review on the TS side.
const RMP_SERDE_VERSION: &str = "1.3.1";

/// NETBAT protocol version this manifest binds to. ADR-0030 reserves
/// `NETBAT/2 STREAM` for a future substrate workstream — explicitly out
/// of scope for the Phase 0 manifest.
const NETBAT_VERSION: &str = "NETBAT/1";

/// `batpak` package version pinned to the workspace release line. The
/// TS codegen does not gate on this today, but it is recorded so the
/// generated TS can advertise which BatPAK family snapshot it was
/// generated against.
const BATPAK_VERSION: &str = "0.7.6";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BatpakTsManifest {
    manifest_version: u32,
    netbat_version: &'static str,
    batpak_version: &'static str,
    canonical_encoding: CanonicalEncoding,
    #[serde(flatten)]
    snapshot: hbat::manifest::ManifestSnapshot,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalEncoding {
    kind: &'static str,
    rmp_serde_version: &'static str,
}

pub(crate) fn export_ts_manifest(args: &ExportTsManifestArgs) -> Result<()> {
    let manifest = BatpakTsManifest {
        manifest_version: MANIFEST_VERSION,
        netbat_version: NETBAT_VERSION,
        batpak_version: BATPAK_VERSION,
        canonical_encoding: CanonicalEncoding {
            kind: "named-field-msgpack",
            rmp_serde_version: RMP_SERDE_VERSION,
        },
        snapshot: hbat::manifest::descriptors(),
    };

    let json =
        serde_json::to_string_pretty(&manifest).context("serialize BatPAK TS manifest to JSON")?;

    if let Some(parent) = args.out.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| {
                format!("create manifest parent directory {}", parent.display())
            })?;
        }
    }

    // Append a trailing newline so editors with newline-at-EOF policies
    // and `git diff` are both happy.
    let mut content = json;
    content.push('\n');
    fs::write(&args.out, content).with_context(|| format!("write {}", args.out.display()))?;
    println!("export-ts-manifest: wrote {}", args.out.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_serializes_with_top_level_metadata_and_snapshot() {
        let manifest = BatpakTsManifest {
            manifest_version: MANIFEST_VERSION,
            netbat_version: NETBAT_VERSION,
            batpak_version: BATPAK_VERSION,
            canonical_encoding: CanonicalEncoding {
                kind: "named-field-msgpack",
                rmp_serde_version: RMP_SERDE_VERSION,
            },
            snapshot: hbat::manifest::descriptors(),
        };
        let json = serde_json::to_value(&manifest).expect("serialize manifest");

        assert_eq!(json["manifestVersion"], MANIFEST_VERSION);
        assert_eq!(json["netbatVersion"], NETBAT_VERSION);
        assert_eq!(json["batpakVersion"], BATPAK_VERSION);
        assert_eq!(json["canonicalEncoding"]["kind"], "named-field-msgpack");
        assert_eq!(
            json["canonicalEncoding"]["rmpSerdeVersion"],
            RMP_SERDE_VERSION
        );
        // 0.7.6 ships 6 events (heartbeat req/ack, bank.commit req/ack,
        // event.get req/ack) and 3 operations (heartbeat, bank.commit,
        // event.get).
        assert_eq!(
            json["events"].as_array().expect("events is an array").len(),
            6
        );
        assert_eq!(
            json["operations"]
                .as_array()
                .expect("operations is an array")
                .len(),
            3
        );
        let op_names: Vec<&str> = json["operations"]
            .as_array()
            .expect("operations array")
            .iter()
            .map(|op| op["name"].as_str().expect("op name is string"))
            .collect();
        assert!(op_names.contains(&"system.heartbeat"));
        assert!(op_names.contains(&"bank.commit"));
        assert!(op_names.contains(&"event.get"));
        for op in json["operations"].as_array().expect("operations array") {
            assert_eq!(op["errorFixture"]["code"], "unknown_operation");
        }
    }
}
