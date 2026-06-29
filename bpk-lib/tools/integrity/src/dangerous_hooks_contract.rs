//! INV-DANGEROUS-TEST-HOOKS-NONDEFAULT: dangerous test hooks stay out of
//! default production builds and their public surfaces stay feature-gated.

use crate::repo_surface::ensure;
use crate::source_cache::SourceCache;
use anyhow::{Context, Result};
use cargo_metadata::MetadataCommand;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const FEATURE: &str = "dangerous-test-hooks";

struct GuardedNeedle {
    rel: &'static str,
    needle: &'static str,
    label: &'static str,
}

const GUARDED_NEEDLES: &[GuardedNeedle] = &[
    GuardedNeedle {
        rel: "crates/core/src/lib.rs",
        needle: "pub mod __fuzz;",
        label: "__fuzz public module",
    },
    GuardedNeedle {
        rel: "crates/core/src/lib.rs",
        needle: "pub mod __sim {",
        label: "__sim public module",
    },
    GuardedNeedle {
        rel: "crates/core/src/store/mod.rs",
        needle: "pub mod fault;",
        label: "store::fault module",
    },
    GuardedNeedle {
        rel: "crates/core/src/store/mod.rs",
        needle: "pub use fault::{",
        label: "store::fault re-export",
    },
    GuardedNeedle {
        rel: "crates/core/src/store/config.rs",
        needle: "pub fn with_fault_injector",
        label: "StoreConfig::with_fault_injector",
    },
    GuardedNeedle {
        rel: "crates/core/src/store/error.rs",
        needle: "FaultInjected(String),",
        label: "StoreError::FaultInjected",
    },
];

pub(crate) fn check(repo_root: &Path, source_cache: &mut SourceCache) -> Result<BTreeSet<PathBuf>> {
    let mut inputs = BTreeSet::new();
    let manifest = repo_root.join("Cargo.toml");
    inputs.insert(manifest.clone());
    check_feature_metadata(repo_root)?;

    for guarded in GUARDED_NEEDLES {
        let path = repo_root.join(guarded.rel);
        inputs.insert(path.clone());
        let content = source_cache
            .read_to_string(&path)
            .with_context(|| format!("read {}", guarded.rel))?;
        check_guarded_needles(guarded.rel, &content, std::slice::from_ref(guarded))?;
    }
    Ok(inputs)
}

fn check_feature_metadata(repo_root: &Path) -> Result<()> {
    let metadata = MetadataCommand::new()
        .manifest_path(repo_root.join("Cargo.toml"))
        .no_deps()
        .exec()
        .context("read Cargo metadata for dangerous hooks feature contract")?;
    let package = metadata
        .packages
        .iter()
        .find(|package| package.name == "batpak")
        .context("Cargo metadata must contain root batpak package")?;
    let declared_features = package.features.keys().map(String::as_str);
    let default_features = package
        .features
        .get("default")
        .context("batpak package must declare default features")?
        .iter()
        .map(String::as_str);
    check_feature_sets(declared_features, default_features)
}

fn check_feature_sets<'a>(
    declared_features: impl IntoIterator<Item = &'a str>,
    default_features: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    let declared = declared_features.into_iter().collect::<BTreeSet<_>>();
    let default = default_features.into_iter().collect::<BTreeSet<_>>();
    ensure(
        declared.contains(FEATURE),
        "dangerous-hooks-contract (INV-DANGEROUS-TEST-HOOKS-NONDEFAULT): \
         batpak must declare the dangerous-test-hooks feature explicitly",
    )?;
    ensure(
        !default.contains(FEATURE),
        "dangerous-hooks-contract (INV-DANGEROUS-TEST-HOOKS-NONDEFAULT): \
         default features must not include dangerous-test-hooks",
    )
}

fn check_guarded_needles(rel: &str, content: &str, needles: &[GuardedNeedle]) -> Result<()> {
    let lines = content.lines().collect::<Vec<_>>();
    for needle in needles {
        let Some(line_index) = lines.iter().position(|line| line.contains(needle.needle)) else {
            anyhow::bail!(
                "dangerous-hooks-contract (INV-DANGEROUS-TEST-HOOKS-NONDEFAULT): \
                 {} missing `{}` in {rel}",
                needle.label,
                needle.needle
            );
        };
        ensure(
            has_feature_cfg_before(&lines, line_index),
            format!(
                "dangerous-hooks-contract (INV-DANGEROUS-TEST-HOOKS-NONDEFAULT): \
                 {} in {rel}:{} is not guarded by #[cfg(feature = \"{FEATURE}\")]",
                needle.label,
                line_index + 1
            ),
        )?;
    }
    Ok(())
}

fn has_feature_cfg_before(lines: &[&str], line_index: usize) -> bool {
    let start = line_index.saturating_sub(6);
    lines[start..line_index].iter().any(|line| {
        line.contains("#[cfg(feature = \"dangerous-test-hooks\")]")
            || line.contains("#[cfg(any(test, feature = \"dangerous-test-hooks\"))]")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dangerous_hooks_default_feature_and_cfg_contract_rejects_planted_exposure() {
        assert!(
            check_feature_sets(
                ["blake3", "dangerous-test-hooks"],
                ["blake3", "dangerous-test-hooks"],
            )
            .is_err(),
            "dangerous-test-hooks in default features must be rejected"
        );

        let red = "pub mod fault;\n";
        assert!(
            check_guarded_needles(
                "crates/core/src/store/mod.rs",
                red,
                &[GuardedNeedle {
                    rel: "crates/core/src/store/mod.rs",
                    needle: "pub mod fault;",
                    label: "store::fault module",
                }],
            )
            .is_err(),
            "an ungated dangerous hook surface must be rejected"
        );
    }

    #[test]
    fn dangerous_hooks_cfg_contract_accepts_feature_gated_surface() {
        check_feature_sets(["blake3", "dangerous-test-hooks"], ["blake3"])
            .expect("feature declared and absent from defaults");
        check_guarded_needles(
            "crates/core/src/store/mod.rs",
            "#[cfg(feature = \"dangerous-test-hooks\")]\npub mod fault;\n",
            &[GuardedNeedle {
                rel: "crates/core/src/store/mod.rs",
                needle: "pub mod fault;",
                label: "store::fault module",
            }],
        )
        .expect("feature-gated dangerous hook surface");
    }
}
