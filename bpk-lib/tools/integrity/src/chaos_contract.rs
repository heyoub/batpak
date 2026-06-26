//! INV-CHAOS-LINUX-ONLY: the privileged dm-flakey chaos harness is compiled
//! only for Linux and only under dangerous-test-hooks.

use crate::repo_surface::ensure;
use crate::source_cache::SourceCache;
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const CHAOS_ENTRYPOINTS: &[&str] = &[
    "crates/core/tests/chaos.rs",
    "crates/core/tests/chaos/mod.rs",
];

pub(crate) fn check(repo_root: &Path, source_cache: &mut SourceCache) -> Result<BTreeSet<PathBuf>> {
    let mut inputs = BTreeSet::new();
    for rel in CHAOS_ENTRYPOINTS {
        let path = repo_root.join(rel);
        inputs.insert(path.clone());
        let content = source_cache
            .read_to_string(&path)
            .with_context(|| format!("read {rel}"))?;
        check_chaos_entrypoint(rel, &content)?;
    }
    Ok(inputs)
}

fn check_chaos_entrypoint(rel: &str, content: &str) -> Result<()> {
    ensure(
        content.contains("#![cfg(target_os = \"linux\")]"),
        format!(
            "chaos-linux-only-contract (INV-CHAOS-LINUX-ONLY): {rel} must carry \
             #![cfg(target_os = \"linux\")] so dm-flakey proofs cannot run off Linux"
        ),
    )?;
    ensure(
        content.contains("#![cfg(feature = \"dangerous-test-hooks\")]"),
        format!(
            "chaos-linux-only-contract (INV-CHAOS-LINUX-ONLY): {rel} must carry \
             #![cfg(feature = \"dangerous-test-hooks\")] so privileged chaos hooks are opt-in"
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chaos_contract_rejects_missing_linux_or_feature_cfg() {
        assert!(
            check_chaos_entrypoint(
                "crates/core/tests/chaos.rs",
                "#![cfg(feature = \"dangerous-test-hooks\")]\nmod chaos;\n",
            )
            .is_err(),
            "missing Linux cfg must be rejected"
        );
        assert!(
            check_chaos_entrypoint(
                "crates/core/tests/chaos.rs",
                "#![cfg(target_os = \"linux\")]\nmod chaos;\n",
            )
            .is_err(),
            "missing dangerous-test-hooks cfg must be rejected"
        );
    }

    #[test]
    fn chaos_contract_accepts_linux_dangerous_entrypoint() {
        check_chaos_entrypoint(
            "crates/core/tests/chaos.rs",
            "#![cfg(target_os = \"linux\")]\n#![cfg(feature = \"dangerous-test-hooks\")]\nmod chaos;\n",
        )
        .expect("linux + dangerous-test-hooks cfgs present");
    }

    #[test]
    fn chaos_entrypoints_stay_under_core_tests() {
        for rel in CHAOS_ENTRYPOINTS {
            assert!(
                rel.starts_with("crates/core/tests/chaos"),
                "unexpected chaos entrypoint: {rel}"
            );
        }
    }
}
