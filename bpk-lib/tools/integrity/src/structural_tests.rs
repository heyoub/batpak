//! End-to-end RED fixtures for the structural source-lint gates (gauntlet
//! tool-qualification, P1-3). Each test builds a minimal temp tree that PASSES
//! the gate (green baseline), then plants the specific violation and asserts the
//! gate's `check(..)` returns `Err` (red). Both halves are required so the test
//! cannot pass vacuously: neutralizing the planted violation must turn the test
//! red. These are the anti-vacuous fixtures named by `gate_registry.rs` that earn
//! each gate its blocking authority.

use super::{
    check_allow_justifications_over, check_inline_test_island_pressure_over,
    check_no_dead_code_silencers_over, check_rust_file_size_pressure_over,
};
use crate::source_cache::SourceCache;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_repo(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "batpak-structural-lints-{name}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("create temp repo");
    path
}

fn write_file(repo: &Path, rel: &str, body: &str) -> PathBuf {
    let path = repo.join(rel);
    fs::create_dir_all(path.parent().expect("parent dir")).expect("create dirs");
    fs::write(&path, body).expect("write fixture file");
    path
}

/// Build an `allow` attribute string at RUNTIME via `format!`. Assembling it from
/// parts (rather than writing the literal token in a source string) keeps the
/// raw bytes of THIS fixture file free of the `#[allow(` token — otherwise the
/// repo's own text-based allow-justification lint (build.rs `check_allow_justifications`
/// over `tools/integrity/src/`) would flag these planted fixtures as real rogue
/// silencers. The runtime fixture bytes are identical to a literal `#[allow(<inner>)]`.
fn allow_attr(inner: &str) -> String {
    format!("#[{}({})]", "allow", inner)
}

/// An empty dead-code-silencer allowlist so `check_no_dead_code_silencers_over`
/// can load it (the gate requires the file to exist).
fn write_empty_dead_code_allowlist(repo: &Path) {
    write_file(
        repo,
        "traceability/dead_code_silencer_allowlist.yaml",
        "[]\n",
    );
}

/// A one-entry invariants catalog so `// justifies: ... INV-TEST-FIXTURE` anchors
/// resolve in `check_allow_justifications_over`.
fn write_invariants_catalog(repo: &Path) {
    write_file(
        repo,
        "traceability/invariants.yaml",
        "- id: INV-TEST-FIXTURE\n",
    );
}

// --- Gate 1: file-size-pressure --------------------------------------------

#[test]
fn file_size_pressure_rejects_oversized_production_file() {
    let repo = temp_repo("file-size");
    let rel = "crates/macros/src/synthetic.rs";

    // GREEN: a file at the absolute cap passes.
    let at_cap = (0..super::DEFAULT_LINE_BUDGET)
        .map(|line| format!("// line {line}\n"))
        .collect::<String>();
    let path = write_file(&repo, rel, &at_cap);
    let mut cache = SourceCache::new(&repo);
    check_rust_file_size_pressure_over(&repo, &[path.clone()], &mut cache)
        .expect("at-cap production file is accepted");

    // RED: one nonblank line over the cap fails.
    let over_cap = (0..super::DEFAULT_LINE_BUDGET + 1)
        .map(|line| format!("// line {line}\n"))
        .collect::<String>();
    write_file(&repo, rel, &over_cap);
    let mut cache = SourceCache::new(&repo);
    let err = check_rust_file_size_pressure_over(&repo, &[path], &mut cache)
        .expect_err("oversized production file is rejected");
    assert!(err.to_string().contains("file size pressure"), "{err:?}");

    fs::remove_dir_all(repo).expect("remove temp repo");
}

// --- Gate 2: inline-test-island-pressure -----------------------------------

/// Build a production file containing one inline `mod tests` island whose body
/// has `body_lines` nonblank statement lines.
fn file_with_test_island(body_lines: usize) -> String {
    let mut out = String::from("pub fn production() {}\n\n#[cfg(test)]\nmod tests {\n");
    for line in 0..body_lines {
        out.push_str(&format!("    fn helper_{line}() {{}}\n"));
    }
    out.push_str("}\n");
    out
}

#[test]
fn inline_test_island_pressure_rejects_oversized_island() {
    let repo = temp_repo("island");
    let rel = "crates/macros/src/synthetic.rs";

    // GREEN: a small inline `mod tests` island is well within the budget.
    let path = write_file(&repo, rel, &file_with_test_island(10));
    let mut cache = SourceCache::new(&repo);
    check_inline_test_island_pressure_over(&repo, &[path.clone()], &mut cache)
        .expect("small inline test island is accepted");

    // RED: an island whose body exceeds the absolute budget fails.
    write_file(
        &repo,
        rel,
        &file_with_test_island(super::DEFAULT_TEST_ISLAND_BUDGET + 5),
    );
    let mut cache = SourceCache::new(&repo);
    let err = check_inline_test_island_pressure_over(&repo, &[path], &mut cache)
        .expect_err("oversized inline test island is rejected");
    assert!(
        err.to_string()
            .contains("oversized inline `mod tests` island"),
        "{err:?}"
    );

    fs::remove_dir_all(repo).expect("remove temp repo");
}

// --- Gate 3: dead-code-silencers -------------------------------------------

#[test]
fn dead_code_silencers_reject_dead_code_allow() {
    let repo = temp_repo("dead-code");
    write_empty_dead_code_allowlist(&repo);
    let rel = "crates/macros/src/synthetic.rs";

    // GREEN: a sibling `unused_imports` allow is not a dead_code silencer.
    let path = write_file(
        &repo,
        rel,
        &format!(
            "{}\nuse std::fmt;\npub fn production() {{}}\n",
            allow_attr("unused_imports")
        ),
    );
    let mut cache = SourceCache::new(&repo);
    check_no_dead_code_silencers_over(&repo, &[path.clone()], &mut cache)
        .expect("sibling unused_imports allow is accepted");

    // RED: a `dead_code` silencer is forbidden and not allowlisted.
    write_file(
        &repo,
        rel,
        &format!("{}\npub fn production() {{}}\n", allow_attr("dead_code")),
    );
    let mut cache = SourceCache::new(&repo);
    let err = check_no_dead_code_silencers_over(&repo, &[path], &mut cache)
        .expect_err("dead_code silencer is rejected");
    assert!(
        err.to_string()
            .contains("dead_code silencers are not tolerated"),
        "{err:?}"
    );

    fs::remove_dir_all(repo).expect("remove temp repo");
}

// --- Gate 4: allow-justifications ------------------------------------------

#[test]
fn allow_justifications_rejects_unanchored_allow() {
    let repo = temp_repo("allow-justifications");
    write_invariants_catalog(&repo);
    let rel = "crates/macros/src/synthetic.rs";

    // GREEN: an allow attribute with a >=5-word justifies line carrying a
    // resolvable INV anchor is accepted.
    let justified = format!(
        "// justifies: narrowing cast is bounded by INV-TEST-FIXTURE invariant catalog entry\n\
         {}\n\
         pub fn production() -> u8 {{ 0 }}\n",
        allow_attr("clippy::cast_possible_truncation")
    );
    let path = write_file(&repo, rel, &justified);
    let mut cache = SourceCache::new(&repo);
    check_allow_justifications_over(&repo, &[path.clone()], &mut cache)
        .expect("anchored, prose-bearing justifies is accepted");

    // RED: the same allow with NO `// justifies:` line fails.
    write_file(
        &repo,
        rel,
        &format!(
            "{}\npub fn production() -> u8 {{ 0 }}\n",
            allow_attr("clippy::cast_possible_truncation")
        ),
    );
    let mut cache = SourceCache::new(&repo);
    let err = check_allow_justifications_over(&repo, &[path], &mut cache)
        .expect_err("unjustified lint suppression is rejected");
    assert!(
        err.to_string().contains("unjustified lint suppression"),
        "{err:?}"
    );

    fs::remove_dir_all(repo).expect("remove temp repo");
}

// --- Gate 5: function-complexity (GAUNT-CPLX-6) -----------------------------

/// A production fixture with one within-budget fn and one fn that blows every
/// budget (long body, deep nesting, many branches).
fn complexity_fixture(over: bool) -> String {
    let mut body = String::from("pub fn ok_small() -> u32 {\n    let x = 1;\n    x + 1\n}\n");
    if over {
        // >120 nonblank lines, nesting >5, and many decision points.
        body.push_str("pub fn over_budget(n: u32) -> u32 {\n    let mut total = 0;\n");
        body.push_str("    if n > 0 {\n        if n > 1 {\n            if n > 2 {\n");
        body.push_str("                if n > 3 {\n                    if n > 4 {\n");
        body.push_str("                        if n > 5 { total += 1; }\n");
        body.push_str(
            "                    }\n                }\n            }\n        }\n    }\n",
        );
        for line in 0..130 {
            body.push_str(&format!(
                "    if total > {line} && total < {} || total == {line} {{ total += 1; }}\n",
                line + 1
            ));
        }
        body.push_str("    total\n}\n");
    }
    body
}

#[test]
fn function_complexity_rejects_over_budget_function() {
    let repo = temp_repo("complexity");
    let rel = "crates/macros/src/synthetic.rs";

    // GREEN: a within-budget-only file passes with an empty allowlist.
    let path = write_file(&repo, rel, &complexity_fixture(false));
    let mut cache = SourceCache::new(&repo);
    let measured =
        crate::complexity::collect_functions(&repo, &[path.clone()], &mut cache).expect("collect");
    crate::complexity::enforce(&measured, &[]).expect("within-budget fns pass");

    // RED: add an over-budget fn; with no allowlist it is reported.
    let path = write_file(&repo, rel, &complexity_fixture(true));
    let mut cache = SourceCache::new(&repo);
    let measured =
        crate::complexity::collect_functions(&repo, &[path], &mut cache).expect("collect over");
    let over_key = measured
        .iter()
        .find(|(k, _)| k.ends_with("::over_budget"))
        .expect("over_budget fn measured");
    assert!(
        over_key.1.lines > crate::complexity::FN_LINE_BUDGET,
        "{over_key:?}"
    );
    let err = crate::complexity::enforce(&measured, &[]).expect_err("over-budget fn is rejected");
    assert!(err.to_string().contains("function-complexity"), "{err:?}");
    assert!(err.to_string().contains("over_budget"), "{err:?}");

    // GREEN again: pinning the over-budget fn at its measured values passes.
    let pin = crate::complexity::RatchetEntry {
        key: over_key.0.clone(),
        lines: over_key.1.lines,
        nesting: over_key.1.nesting,
        cyclomatic: over_key.1.cyclomatic,
    };
    crate::complexity::enforce(&measured, std::slice::from_ref(&pin))
        .expect("pinned over-budget fn passes the ratchet");

    fs::remove_dir_all(repo).expect("remove temp repo");
}

// --- Gate 6: no-wallclock-asserts (GAUNT-FLAKE-7) ---------------------------

/// Build a correctness-test fixture string. When `wallclock`, it pairs a clock
/// start with an elapsed assertion. The `Instant::now` / `.elapsed` tokens are
/// assembled at runtime so this fixture file's own bytes do not carry a literal
/// wall-clock-assert that the detector (run over tools/integrity in some lanes)
/// could pick up.
fn wallclock_fixture(wallclock: bool) -> String {
    let now = format!("Instant::{}()", "now");
    let elapsed = format!(".{}()", "elapsed");
    if wallclock {
        format!(
            "#[test]\nfn timed() {{\n    let start = {now};\n    do_work();\n    \
             assert!(start{elapsed}.as_millis() < 50);\n}}\n"
        )
    } else {
        format!(
            "#[test]\nfn logical() {{\n    let start = {now};\n    let _ = start;\n    \
             assert_eq!(2 + 2, 4);\n}}\n"
        )
    }
}

#[test]
fn no_wallclock_asserts_rejects_elapsed_assertion() {
    let repo = temp_repo("wallclock");
    let rel = "crates/core/tests/synthetic_timing.rs";

    // GREEN: a test that starts a clock but asserts a logical value is clean.
    let path = write_file(&repo, rel, &wallclock_fixture(false));
    let mut cache = SourceCache::new(&repo);
    let offenders =
        crate::wallclock::collect_offenders(&repo, &[path.clone()], &mut cache).expect("collect");
    assert!(
        offenders.is_empty(),
        "logical-assert test is not an offender: {offenders:?}"
    );
    crate::wallclock::enforce(&offenders, &[]).expect("no offenders pass");

    // RED: an elapsed-duration assertion is reported.
    let path = write_file(&repo, rel, &wallclock_fixture(true));
    let mut cache = SourceCache::new(&repo);
    let offenders =
        crate::wallclock::collect_offenders(&repo, &[path], &mut cache).expect("collect offender");
    assert_eq!(
        offenders.len(),
        1,
        "wall-clock test detected: {offenders:?}"
    );
    let err =
        crate::wallclock::enforce(&offenders, &[]).expect_err("wall-clock assert is rejected");
    assert!(err.to_string().contains("no-wallclock-asserts"), "{err:?}");

    // GREEN again: allowlisting the offender passes.
    let pin = crate::wallclock::AllowEntry {
        key: offenders[0].clone(),
    };
    crate::wallclock::enforce(&offenders, std::slice::from_ref(&pin))
        .expect("allowlisted offender passes");

    fs::remove_dir_all(repo).expect("remove temp repo");
}

// --- Gate 7: mutation-glob-coverage (GAUNT-FAULT-3 vacuous-glob killer) -----

#[test]
fn glob_coverage_rejects_nonexistent_mutation_glob() {
    use crate::repo_surface::{repo_root, tracked_repo_files};
    let repo = repo_root().expect("repo root");
    let tracked = tracked_repo_files(&repo).expect("tracked files");

    // GREEN: a glob that matches a real tracked source file passes.
    let live = vec!["crates/core/src/store/**/*.rs".to_string()];
    crate::glob_coverage::check_globs_with_waivers(&repo, &live, &tracked, &[])
        .expect("a real glob matches tracked files");

    // RED: a typo'd glob matching nothing is reported (vacuous-PASS killer).
    let dead = vec!["crates/core/src/NONEXISTENT.rs".to_string()];
    let err = crate::glob_coverage::check_globs_with_waivers(&repo, &dead, &tracked, &[])
        .expect_err("a dead glob is rejected");
    assert!(
        err.to_string().contains("mutation-glob-coverage"),
        "{err:?}"
    );
    assert!(err.to_string().contains("NONEXISTENT"), "{err:?}");

    // The live lanes.rs registry is non-empty (the extractor really fires).
    let lanes =
        fs::read_to_string(repo.join(crate::glob_coverage::LANES_RS_REL)).expect("read lanes.rs");
    let globs = crate::glob_coverage::extract_mutant_file_globs(&lanes);
    assert!(
        globs.len() > 10,
        "extracted {} globs from lanes.rs",
        globs.len()
    );
}

#[test]
fn glob_matcher_handles_doublestar_and_single_star() {
    use crate::glob_coverage::glob_matches;
    assert!(glob_matches(
        "crates/core/src/store/**/*.rs",
        "crates/core/src/store/a/b/c.rs"
    ));
    assert!(glob_matches(
        "crates/core/src/store/**/*.rs",
        "crates/core/src/store/x.rs"
    ));
    assert!(glob_matches(
        "crates/core/src/guard/*.rs",
        "crates/core/src/guard/g.rs"
    ));
    assert!(!glob_matches(
        "crates/core/src/guard/*.rs",
        "crates/core/src/guard/sub/g.rs"
    ));
    assert!(glob_matches("a/b/c.rs", "a/b/c.rs"));
    assert!(!glob_matches("a/b/c.rs", "a/b/d.rs"));
}
