use crate::util::{project_root, run};
use anyhow::{bail, Context, Result};
use std::process::Command;

/// Store production calipers (audit-only) plus devops/repo sanity calipers on CI surfaces.
pub(crate) fn ast_grep() -> Result<()> {
    ast_grep_store()?;
    ast_grep_devops()?;
    ast_grep_mutation_anchors()?;
    super::ast_grep_family_version::ast_grep_family_version()?;
    Ok(())
}

/// One AST anchor for a registered mutation exclusion. The exclusion regex in
/// `lanes.rs` removes a cargo-mutants mutant from the scoring denominator; this
/// asserts the mutated construct is a real, UNIQUE syntax site. `sg` matches
/// syntax, not text, so it cannot be fooled by the symbol appearing in a comment
/// or a different expression — the syntactic complement to the deterministic
/// symbol-presence gate in `tools/integrity/src/mutation_exclusion_registry.rs`.
struct MutationAnchor {
    /// Diagnostic label tying the anchor to its exclusion entry.
    label: &'static str,
    /// Repo-relative file the exclusion regex points at.
    file: &'static str,
    /// ast-grep pattern for the mutated construct.
    pattern: &'static str,
    /// Required match count (the anchor must be unambiguous — almost always 1).
    expected: usize,
}

// Every EQUIVALENT/TIMEOUT mutation exclusion in `lanes.rs` anchored to its exact
// AST site. The platform-backend `reflink_impl` exclusion is intentionally absent:
// it is a NOT_COMPILED-ON-RUNNER exclusion (cfg-gated macOS/non-linux variants),
// not a single equivalent construct, so an exact-match anchor does not apply.
const MUTATION_EXCLUSION_ANCHORS: &[MutationAnchor] = &[
    MutationAnchor {
        label: "import-reapply: post-append `<` accounting (covers `< -> ==` and `< -> <=`)",
        file: "bpk-lib/crates/core/src/store/import.rs",
        pattern: "receipt.global_sequence < pre_import_frontier",
        expected: 1,
    },
    MutationAnchor {
        label: "import-reapply: `||` dedup probe (`|| -> &&`)",
        file: "bpk-lib/crates/core/src/store/import.rs",
        pattern: "destination.index.idemp.get(key.as_u128()).is_some() || destination.index.get_by_id(key.as_u128()).is_some()",
        expected: 1,
    },
    MutationAnchor {
        label: "import-reapply: ImportSelector::all recursion (`all -> Default::default`)",
        file: "bpk-lib/crates/core/src/store/import.rs",
        pattern: "pub fn all() -> Self { $$$ }",
        expected: 1,
    },
    MutationAnchor {
        label: "index-topology: aos recursion (`aos -> Default::default`)",
        file: "bpk-lib/crates/core/src/store/config/types.rs",
        pattern: "pub fn aos() -> Self { $$$ }",
        expected: 1,
    },
    MutationAnchor {
        label: "projection-flow: diagnostic guard (`delete !`)",
        file: "bpk-lib/crates/core/src/store/projection/flow/mod.rs",
        pattern: "result.is_none() && !events.is_empty()",
        expected: 1,
    },
    MutationAnchor {
        label: "fork-isolation: active-segment match guard (`== active -> true`)",
        file: "bpk-lib/crates/core/src/store/file_classification.rs",
        pattern: "segment_id.as_u64() == active_segment_id",
        expected: 1,
    },
];

/// Assert every registered mutation-exclusion anchors to exactly one AST site.
/// A drifted pattern (0 matches) means the exclusion is excluding nothing; an
/// ambiguous pattern (>1) means the regex would silently exclude unrelated
/// mutants. Either is a silent gate weakening, so both fail closed.
fn ast_grep_mutation_anchors() -> Result<()> {
    let root = project_root()?;
    let mut failures: Vec<String> = Vec::new();
    for anchor in MUTATION_EXCLUSION_ANCHORS {
        let mut command = Command::new("sg");
        command.current_dir(&root).args([
            "run",
            "--pattern",
            anchor.pattern,
            "--lang",
            "rust",
            "--json=compact",
            anchor.file,
        ]);
        let output = command.output().with_context(|| {
            format!(
                "invoke `sg` for mutation anchor `{}`; install via `cargo xtask setup \
                 --install-tools` or `cargo install ast-grep --locked`",
                anchor.label
            )
        })?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let trimmed = stdout.trim();
        if !output.status.success() && trimmed.is_empty() {
            bail!(
                "sg run failed for mutation anchor `{}`:\n{}",
                anchor.label,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let count = if trimmed.is_empty() {
            0
        } else {
            let parsed: serde_json::Value = serde_json::from_str(trimmed)
                .with_context(|| format!("parse sg json for mutation anchor `{}`", anchor.label))?;
            parsed.as_array().map_or(0, Vec::len)
        };
        if count != anchor.expected {
            failures.push(format!(
                "  `{}`\n    {} matched {count} site(s) in {}, expected {} — the exclusion's AST \
                 anchor is stale (the mutated construct moved or the pattern drifted). Update the \
                 pattern or the exclusion in lanes.rs.",
                anchor.label, anchor.pattern, anchor.file, anchor.expected
            ));
        }
    }
    if !failures.is_empty() {
        bail!(
            "ast-grep mutation-exclusion anchors: {} exclusion(s) do not anchor a unique mutation \
             site. A mutation exclusion that anchors zero or many sites silently changes the \
             mutation-score denominator:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
    Ok(())
}

fn ast_grep_store() -> Result<()> {
    let root = project_root()?;
    let mut command = Command::new("sg");
    command.current_dir(&root).args([
        "scan",
        "--config",
        "bpk-lib/tools/ast-grep/sgconfig.yml",
        "--report-style",
        "short",
        "--globs",
        "bpk-lib/crates/core/src/store/**/*.rs",
        "--globs",
        "!bpk-lib/crates/core/src/store/platform/**",
        "--globs",
        "!**/tests.rs",
        "--globs",
        "!**/test_support.rs",
        "--globs",
        "!**/fixtures/**",
    ]);
    run(command).with_context(|| {
        "run store ast-grep calipers; install `sg` via `cargo xtask setup --install-tools` or `cargo install ast-grep --locked`"
    })
}

fn ast_grep_devops() -> Result<()> {
    let root = project_root()?;
    let mut command = Command::new("sg");
    command.current_dir(&root).args([
        "scan",
        "--config",
        "bpk-lib/tools/ast-grep/sgconfig.yml",
        "--report-style",
        "short",
        "--globs",
        ".github/workflows/ci.yml",
        "--globs",
        "justfile",
    ]);
    run(command)
        .with_context(|| "run devops ast-grep calipers on .github/workflows/ci.yml and justfile")
}
