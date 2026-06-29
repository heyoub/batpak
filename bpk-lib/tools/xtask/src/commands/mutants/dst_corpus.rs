//! Graduated DST corpus augmentation for mutation lanes (#64-C).
//!
//! `traceability/seam_registry.yaml` is the source of truth for which critical
//! seams must run the BatPak corpus tests additively in the per-mutant workload.

use anyhow::{bail, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::lanes::{critical_mutation_lanes, MutationLane, MutationScope, MutationTestAugment};

pub(super) const DST_CORPUS_TEST_PACKAGE: &str = "batpak";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Load seam slugs declaring `dst_coverage: true` from `traceability/seam_registry.yaml`.
pub(super) fn load_dst_coverage_seams() -> Result<BTreeSet<String>> {
    let registry_path = workspace_root().join("traceability/seam_registry.yaml");
    let text = std::fs::read_to_string(&registry_path).map_err(|error| {
        anyhow::anyhow!(
            "read {} for dst corpus mutation wiring: {error}",
            registry_path.display()
        )
    })?;

    let mut covered = BTreeSet::new();
    let mut current_slug: Option<String> = None;
    let mut current_dst = false;

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("- slug:") {
            if let Some(slug) = current_slug.take() {
                if current_dst {
                    covered.insert(slug);
                }
            }
            current_slug = Some(rest.trim().to_owned());
            current_dst = false;
            continue;
        }
        if let Some(rest) = line.strip_prefix("  dst_coverage:") {
            current_dst = rest.trim() == "true";
        }
    }

    if let Some(slug) = current_slug {
        if current_dst {
            covered.insert(slug);
        }
    }

    if covered.is_empty() {
        bail!(
            "seam_registry.yaml must declare at least one dst_coverage: true seam for hybrid mutation wiring"
        );
    }

    Ok(covered)
}

/// Fail closed when the registry names a `dst_coverage: true` seam that has no
/// corresponding critical mutation lane in xtask.
pub(super) fn assert_registry_dst_seams_are_graded() -> Result<()> {
    let dst_seams = load_dst_coverage_seams()?;
    let critical_slugs: BTreeSet<String> = critical_mutation_lanes()
        .iter()
        .map(|lane| lane.slug.clone())
        .collect();
    assert_dst_seams_have_critical_lanes(&dst_seams, &critical_slugs)
}

fn assert_dst_seams_have_critical_lanes(
    dst_seams: &BTreeSet<String>,
    critical_slugs: &BTreeSet<String>,
) -> Result<()> {
    for slug in dst_seams {
        if !critical_slugs.contains(slug) {
            bail!(
                "dst_coverage seam `{slug}` has no critical mutation lane; hybrid mutation wiring is fail-closed"
            );
        }
    }
    Ok(())
}

/// Attach graduated-corpus augmentation to every critical lane in `lanes` whose
/// slug is marked `dst_coverage: true` in the registry.
pub(super) fn apply_graduated_dst_corpus_augmentation(lanes: &mut [MutationLane]) -> Result<()> {
    assert_registry_dst_seams_are_graded()?;
    let dst_seams = load_dst_coverage_seams()?;
    for lane in lanes
        .iter_mut()
        .filter(|lane| lane.scope == MutationScope::CriticalSeam && dst_seams.contains(&lane.slug))
    {
        if !lane
            .test_augments
            .contains(&MutationTestAugment::GraduatedDstCorpus)
        {
            lane.test_augments
                .push(MutationTestAugment::GraduatedDstCorpus);
        }
        if !lane.test_packages.contains(&DST_CORPUS_TEST_PACKAGE) {
            lane.test_packages.push(DST_CORPUS_TEST_PACKAGE);
        }
    }
    validate_dst_corpus_wiring(lanes, &dst_seams)
}

/// Fail closed when a lane in this plan carries corpus augmentation inconsistent
/// with the registry. Only lanes present in `lanes` are checked.
pub(super) fn validate_dst_corpus_wiring(
    lanes: &[MutationLane],
    dst_seams: &BTreeSet<String>,
) -> Result<()> {
    for lane in lanes
        .iter()
        .filter(|lane| lane.scope == MutationScope::CriticalSeam)
    {
        let covered = dst_seams.contains(&lane.slug);
        let augmented = lane
            .test_augments
            .contains(&MutationTestAugment::GraduatedDstCorpus);
        let has_test_package = lane.test_packages.contains(&DST_CORPUS_TEST_PACKAGE);
        if covered {
            if !augmented {
                bail!(
                    "critical seam `{}` declares dst_coverage but lane lacks GraduatedDstCorpus augmentation",
                    lane.slug
                );
            }
            if !has_test_package {
                bail!(
                    "critical seam `{}` declares dst_coverage but lane lacks --test-package {DST_CORPUS_TEST_PACKAGE}",
                    lane.slug
                );
            }
        } else if augmented {
            bail!(
                "critical seam `{}` must not carry GraduatedDstCorpus augmentation without dst_coverage: true",
                lane.slug
            );
        } else if has_test_package {
            bail!(
                "critical seam `{}` must not add --test-package {DST_CORPUS_TEST_PACKAGE} without dst_coverage: true",
                lane.slug
            );
        }
    }

    Ok(())
}

#[cfg(test)]
pub(super) fn assert_dst_seams_have_critical_lanes_for_test(
    dst_seams: &BTreeSet<String>,
    critical_slugs: &BTreeSet<String>,
) -> Result<()> {
    assert_dst_seams_have_critical_lanes(dst_seams, critical_slugs)
}
