use super::{ensure, relative};
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

struct BoundaryTerm {
    token: &'static str,
    reason: &'static str,
}

const CORE_LAYER_LEAKS: &[BoundaryTerm] = &[
    BoundaryTerm {
        token: "syncbat",
        reason: "runtime layer name belongs outside batpak core",
    },
    BoundaryTerm {
        token: "Syncbat",
        reason: "runtime layer type names belong outside batpak core",
    },
    BoundaryTerm {
        token: "contract.external_v1",
        reason: "ExtProfile profile wire validation belongs outside batpak core",
    },
    BoundaryTerm {
        token: "authority_required",
        reason: "authority claims are caller policy input, not substrate law",
    },
    BoundaryTerm {
        token: "External-Profile",
        reason: "ExtProfile semantics stay outside batpak core",
    },
    BoundaryTerm {
        token: "ExternalProfile",
        reason: "ExtProfile profile types stay outside batpak core",
    },
];

pub(super) fn check(repo_root: &Path, tracked_files: &[PathBuf]) -> Result<()> {
    for path in tracked_files {
        if !is_core_production_rust(repo_root, path) {
            continue;
        }
        let content =
            fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        for term in core_layer_leaks(&content) {
            ensure(
                false,
                format!(
                    "batpak core layer leak in {}: `{}` ({})",
                    relative(repo_root, path),
                    term.token,
                    term.reason
                ),
            )?;
        }
    }
    Ok(())
}

fn is_core_production_rust(repo_root: &Path, path: &Path) -> bool {
    let rel = relative(repo_root, path);
    rel.starts_with("crates/core/src/") && rel.ends_with(".rs")
}

fn core_layer_leaks(content: &str) -> Vec<&'static BoundaryTerm> {
    CORE_LAYER_LEAKS
        .iter()
        .filter(|term| content.contains(term.token))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{core_layer_leaks, is_core_production_rust};
    use std::path::Path;

    #[test]
    fn detects_runtime_and_protocol_terms() {
        let content = "pub struct SyncbatCore;\nconst PROFILE: &str = \"contract.external_v1\";";
        let leaks = core_layer_leaks(content);
        let tokens: Vec<&str> = leaks.iter().map(|leak| leak.token).collect();
        assert!(tokens.contains(&"Syncbat"));
        assert!(tokens.contains(&"contract.external_v1"));
    }

    #[test]
    fn ignores_plain_substrate_terms() {
        let content = "Store AppendReceipt GateSet Pipeline opaque extension cargo";
        assert!(core_layer_leaks(content).is_empty());
    }

    #[test]
    fn scans_only_core_production_rust() {
        let root = Path::new("/repo");
        assert!(is_core_production_rust(
            root,
            Path::new("/repo/crates/core/src/store/mod.rs")
        ));
        assert!(!is_core_production_rust(
            root,
            Path::new("/repo/crates/core/tests/substrate_additions.rs")
        ));
        assert!(!is_core_production_rust(
            root,
            Path::new("/repo/crates/syncbat/src/lib.rs")
        ));
    }
}
