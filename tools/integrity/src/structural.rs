use crate::repo_surface::{ensure, relative, repo_root, rust_files, tracked_repo_files};
use crate::shared_checks::{
    collect_dead_code_silencer_sites, line_carries_justification,
    load_dead_code_silencer_allowlist, load_known_invariants,
};
use crate::{architecture_lints, ci_parity, harness_lints, public_surface, store_pub_fn_coverage};
use anyhow::{anyhow, bail, Result};
use std::fs;
use std::path::Path;

pub(crate) fn run() -> Result<()> {
    let repo_root = repo_root()?;
    let tracked_files = tracked_repo_files(&repo_root)?;
    architecture_lints::check(&repo_root, &tracked_files)?;
    harness_lints::check(&repo_root, &tracked_files)?;
    check_no_dead_code_silencers(&repo_root)?;
    check_allow_justifications(&repo_root)?;
    public_surface::check(&repo_root)?;
    ci_parity::check(&repo_root)?;
    store_pub_fn_coverage::check(&repo_root)?;
    println!("structural-check: ok");
    Ok(())
}

fn check_no_dead_code_silencers(repo_root: &Path) -> Result<()> {
    let allowlisted = load_dead_code_silencer_allowlist(repo_root).map_err(|err| anyhow!(err))?;
    let mut paths = rust_files(&repo_root.join("src"));
    paths.extend(rust_files(&repo_root.join("tools/xtask/src")));
    paths.extend(rust_files(&repo_root.join("tools/integrity/src")));
    paths.extend(rust_files(&repo_root.join("crates/macros/src")));
    paths.extend(rust_files(&repo_root.join("crates/macros-support/src")));
    paths.extend(rust_files(&repo_root.join("tests")));
    paths.extend(rust_files(&repo_root.join("examples")));
    paths.extend(rust_files(&repo_root.join("benches")));
    paths.push(repo_root.join("build.rs"));
    for path in paths {
        let content = fs::read_to_string(&path)?;
        let sites = collect_dead_code_silencer_sites(&content)
            .map_err(|err| anyhow!("parse {}: {}", relative(repo_root, &path), err))?;
        for site in sites {
            let allowlist_site = format!("{}:{}", relative(repo_root, &path), site.line);
            if allowlisted.contains(&allowlist_site) {
                continue;
            }
            bail!(
                "dead_code silencers are not tolerated in {}:{}:{}.\n\
                 Found `{}`.\n\
                 If code is test-only, use #[cfg(test)]. If it is unused, delete it.\n\
                 If it is shared infrastructure, restructure it so the compiler sees the real ownership surface.\n\
                 If this is the rare legitimate exception, add `{}` to traceability/dead_code_silencer_allowlist.yaml with `reason` and `adr`.",
                relative(repo_root, &path),
                site.line,
                site.column,
                site.rendered,
                allowlist_site,
            );
        }
    }
    Ok(())
}

fn check_allow_justifications(repo_root: &Path) -> Result<()> {
    let known_invariants = load_known_invariants(repo_root).map_err(|err| anyhow!(err))?;
    let mut paths = rust_files(&repo_root.join("src"));
    paths.extend(rust_files(&repo_root.join("tools/xtask/src")));
    paths.extend(rust_files(&repo_root.join("tools/integrity/src")));
    paths.extend(rust_files(&repo_root.join("crates/macros/src")));
    paths.extend(rust_files(&repo_root.join("crates/macros-support/src")));
    paths.extend(rust_files(&repo_root.join("tests")));
    paths.extend(rust_files(&repo_root.join("examples")));
    paths.extend(rust_files(&repo_root.join("benches")));
    paths.push(repo_root.join("build.rs"));
    for path in paths {
        let content = fs::read_to_string(&path)?;
        let lines: Vec<&str> = content.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("#![allow(") || trimmed.starts_with("#[allow(") {
                let justified = line_carries_justification(line, repo_root, &known_invariants)
                    || index
                        .checked_sub(1)
                        .and_then(|prev| lines.get(prev))
                        .map(|prev| line_carries_justification(prev, repo_root, &known_invariants))
                        .unwrap_or(false);
                ensure(
                    justified,
                    format!(
                        "unjustified allow in {}:{} — every #[allow(...)] must carry a `// justifies: <>=5 words + >=1 resolvable anchor>` comment. \
                         An anchor is an INV-id from traceability/invariants.yaml, an ADR-NNNN whose file exists under docs/adr/, \
                         or a concrete repo path (src/..., tests/..., examples/..., crates/macros/..., crates/macros-support/..., benches/..., tools/..., build.rs). \
                         See INV-ALLOW-IS-DESIGN.",
                        relative(repo_root, &path),
                        index + 1
                    ),
                )?;
            }
        }
    }
    Ok(())
}
