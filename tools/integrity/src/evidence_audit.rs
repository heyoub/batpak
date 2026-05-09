//! Static checks for deterministic evidence / report bodies (tooling lane).
//!
//! Complements runtime doctrine tests in `tests/evidence_report_family.rs` with
//! repo-local structural assertions that are cheap to run in pre-push loops.

use crate::repo_surface::core_path;
use anyhow::{bail, Context, Result};
use regex::Regex;
use std::fs;
use std::path::Path;
use syn::{Fields, Item, Visibility};

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
    "pcp",
    "capsule",
    "websocket",
    "sandbox",
    "deployment",
];

pub fn run(repo_root: &Path) -> Result<()> {
    for &(rel, struct_name) in SCHEMA_VERSION_BODY_ANCHORS {
        let path = core_path(repo_root, rel);
        let text = fs::read_to_string(&path)
            .with_context(|| format!("evidence-audit: read {}", path.display()))?;
        assert_public_struct_has_public_schema_version(&path, &text, struct_name)?;
    }

    let prelude = fs::read_to_string(core_path(repo_root, "src/prelude.rs"))
        .context("evidence-audit: read src/prelude.rs")?;
    let store_mod = fs::read_to_string(core_path(repo_root, "src/store/mod.rs"))
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

fn assert_public_struct_has_public_schema_version(
    path: &Path,
    text: &str,
    struct_name: &str,
) -> Result<()> {
    let file = syn::parse_file(text).with_context(|| {
        format!(
            "evidence-audit: parse {} while checking `{struct_name}`",
            path.display()
        )
    })?;
    let Some(item_struct) = file.items.iter().find_map(|item| match item {
        Item::Struct(item_struct) if item_struct.ident == struct_name => Some(item_struct),
        _ => None,
    }) else {
        bail!(
            "evidence-audit: {} missing public struct `{struct_name}` anchor",
            path.display()
        );
    };

    if !matches!(item_struct.vis, Visibility::Public(_)) {
        bail!(
            "evidence-audit: {} `{struct_name}` must be a public struct",
            path.display()
        );
    }

    let Fields::Named(fields) = &item_struct.fields else {
        bail!(
            "evidence-audit: {} `{struct_name}` must use named fields and declare `pub schema_version`",
            path.display()
        );
    };
    let has_public_schema_version = fields.named.iter().any(|field| {
        field
            .ident
            .as_ref()
            .is_some_and(|ident| ident == "schema_version")
            && matches!(field.vis, Visibility::Public(_))
    });
    if !has_public_schema_version {
        bail!(
            "evidence-audit: {} `{struct_name}` must declare a public `schema_version` field",
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::assert_public_struct_has_public_schema_version;
    use std::path::Path;

    #[test]
    fn schema_version_check_uses_named_public_struct_field() {
        let source = r#"
pub struct ReportBody {
    pub schema_version: u16,
}
"#;

        assert!(assert_public_struct_has_public_schema_version(
            Path::new("fixture.rs"),
            source,
            "ReportBody",
        )
        .is_ok());
    }

    #[test]
    fn schema_version_check_rejects_comments_and_other_structs() {
        let source = r#"
// pub struct ReportBody { pub schema_version: u16 }
pub struct OtherBody {
    pub schema_version: u16,
}
pub struct ReportBody {
    pub value: u16,
}
"#;

        let err = assert_public_struct_has_public_schema_version(
            Path::new("fixture.rs"),
            source,
            "ReportBody",
        )
        .expect_err("must reject missing field on named struct");
        assert!(
            err.to_string().contains("public `schema_version` field"),
            "wrong error: {err:#}"
        );
    }
}
