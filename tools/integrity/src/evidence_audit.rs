//! Static checks for deterministic evidence / report bodies (tooling lane).
//!
//! Complements runtime doctrine tests in `tests/evidence_report_family.rs` with
//! repo-local structural assertions that are cheap to run in pre-push loops.

use anyhow::{bail, Context, Result};
use regex::Regex;
use std::fs;
use std::path::Path;

/// `(tracked path, public struct name)` anchors for bodies that must carry a
/// `schema_version` field near the top of the struct definition.
const SCHEMA_VERSION_BODY_ANCHORS: &[(&str, &str)] = &[
    ("src/schema.rs", "SchemaSnapshotReportBody"),
    ("src/store/chain_walk.rs", "ChainWalkReportBody"),
    (
        "src/store/subscriber_frontier.rs",
        "SubscriberFrontierReportBody",
    ),
    ("src/store/projection_run.rs", "ProjectionRunReportBody"),
    ("src/store/read_walk.rs", "ReadWalkReportBody"),
    (
        "src/store/store_resource_report.rs",
        "StoreResourceReportBody",
    ),
    ("src/store/compaction_report.rs", "CompactionReportBody"),
    ("src/store/backup_envelope.rs", "BackupManifestBody"),
    ("src/store/backup_envelope.rs", "RestoreProofReportBody"),
    ("src/reservation.rs", "ReservationLedgerReportBody"),
    ("src/reservation.rs", "ReservationReconciliationReportBody"),
    ("src/transition.rs", "StateTransitionReportBody"),
    ("src/registry.rs", "RegistryDriftReportBody"),
    ("src/registry.rs", "RegistryVerificationReport"),
];

/// Substrings that must not appear in public evidence-related export surface
/// (`prelude.rs` + `store` facade re-exports in `src/store/mod.rs`).
const FORBIDDEN_PUBLIC_SUBSTRINGS: &[&str] = &[
    "capability",
    "criticality",
    "budget",
    "portkind",
    "mcp",
    "a2a",
    "extprofile",
    "capsule",
    "websocket",
    "sandbox",
    "deployment",
];

pub fn run(repo_root: &Path) -> Result<()> {
    for &(rel, struct_name) in SCHEMA_VERSION_BODY_ANCHORS {
        let path = repo_root.join(rel);
        let text = fs::read_to_string(&path)
            .with_context(|| format!("evidence-audit: read {}", path.display()))?;
        let needle = format!("pub struct {struct_name}");
        let Some(idx) = text.find(&needle) else {
            bail!(
                "evidence-audit: {} missing `{needle}` anchor",
                path.display()
            );
        };
        let tail = &text[idx..text.len().min(idx + 1600)];
        if !tail.contains("pub schema_version:") {
            bail!(
                "evidence-audit: {} `{struct_name}` must declare `pub schema_version:` near struct start",
                path.display()
            );
        }
    }

    let prelude = fs::read_to_string(repo_root.join("src/prelude.rs"))
        .context("evidence-audit: read src/prelude.rs")?;
    let store_mod = fs::read_to_string(repo_root.join("src/store/mod.rs"))
        .context("evidence-audit: read src/store/mod.rs")?;
    let blob = format!("{prelude}\n{store_mod}");
    for word in FORBIDDEN_PUBLIC_SUBSTRINGS {
        // Word-boundary match so identifiers like `fd_budget` do not trip the
        // `budget` hygiene token from the evidence-family doctrine tests.
        let re = Regex::new(&format!(r"(?i)\b{}\b", regex::escape(word)))
            .with_context(|| format!("compile forbidden-word regex for `{word}`"))?;
        if re.is_match(&blob) {
            bail!(
                "evidence-audit: forbidden downstream vocabulary `{word}` found in prelude/store export blob"
            );
        }
    }

    println!("evidence-audit: ok");
    Ok(())
}
