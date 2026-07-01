//! Mutation-exclusion anchor gate.
//!
//! Every `cargo-mutants` `--exclude-re` regex in `lanes.rs` removes a mutant from
//! the scoring denominator, so a regex that matches NOTHING is the worst kind of
//! gauntlet rot: it looks like a reviewed equivalence proof while silently
//! excluding zero mutants (or, worse, masking a real survivor under a stale
//! path). This gate makes that failure mode impossible to merge.
//!
//! For every exclusion regex we extract the source file it anchors to and the
//! symbol it claims to mutate, then assert:
//!   1. at least one tracked file matches the regex's file anchor, and
//!   2. at least one of those files actually CONTAINS that symbol.
//!
//! This is the check that catches the real bug found during the 0.9.0 triage: an
//! exclusion anchored to `crates/core/src/store/config.rs` for the mutated
//! `IndexTopology::aos` function — which is defined in `config/types.rs`, not
//! `config.rs`. `config.rs` exists (file check alone passes) but does NOT contain
//! `aos` (symbol check fails), so the exclusion was vacuous.
//!
//! This gate is deterministic and shells out to nothing. The complementary
//! syntax-exact anchor (sg `--exclude-re` patterns must match exactly one AST
//! site) runs in the `cargo xtask ast-grep` lane, where `sg` is already invoked.

use crate::docs_catalog::file_declares_test_fn;
use crate::repo_surface::{ensure, relative, resolve_repo_or_core_path, tracked_repo_files};
use crate::source_cache::SourceCache;
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::Path;

const LANES_RS_REL: &str = "tools/xtask/src/commands/mutants/lanes.rs";

/// Why an exclusion is legitimate. Only `Equivalent` requires a behavioral
/// witness test; the rest are justified mechanically (recursion abort, cfg
/// gating) or are behavior-free by construction (diagnostic-only emission).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExclusionCategory {
    /// Observationally equivalent under first-order mutation. MUST cite a witness
    /// test that exercises the site and would fail if the mutant changed behavior.
    Equivalent,
    /// The mutant only flips a diagnostic (`tracing`/log) emission; there is no
    /// behavioral observable, so no witness test can exist.
    DiagnosticOnly,
    /// The mutant rewrites control flow into non-termination (recursion to stack
    /// abort); caught by cargo-mutants' timeout, not an assertion.
    TimeoutAbort,
    /// A cfg-gated variant not compiled on the CI runner — cargo-mutants can
    /// neither apply nor test it there.
    NotCompiled,
}

/// A registered, categorized mutation exclusion. This is the authority meta-gate
/// defers to: an exclusion that appears here (and whose witness the structural
/// gate resolves) is a mechanically-proven denominator change, not an unapproved
/// weakening — so it needs no human `GAUNTLET-WEAKEN-OK` trailer.
struct RegisteredExclusion {
    /// Exact `--exclude-re` string as it appears in `lanes.rs` (raw content).
    regex: &'static str,
    category: ExclusionCategory,
    /// `path::fn` of the witness test. Required for `Equivalent`, `None` otherwise.
    witness: Option<&'static str>,
    reason: &'static str,
}

/// The witnessed exclusion registry. Lockstep-checked against `lanes.rs`: every
/// `--exclude-re` there must appear here with a category (and an `Equivalent`
/// entry must carry a resolvable witness test), and every entry here must still
/// be live in `lanes.rs`. This makes a mutation exclusion mechanically
/// accountable instead of resting on a human weaken-stamp.
const REGISTRY: &[RegisteredExclusion] = &[
    RegisteredExclusion {
        regex: r"crates/core/src/store/config/types\.rs:.*replace IndexTopology::aos -> Self with Default::default\(\)",
        category: ExclusionCategory::TimeoutAbort,
        witness: None,
        reason: "Default for IndexTopology IS aos(); rewriting aos() to Default::default() recurses to a stack abort, caught by timeout — not a behavior change.",
    },
    RegisteredExclusion {
        regex: r"crates/core/src/store/import\.rs:.*replace < with == in import_events",
        category: ExclusionCategory::Equivalent,
        witness: Some("crates/core/tests/import_events.rs::import_events_reimport_is_noop_and_preserves_raw_payload_bytes"),
        reason: "Fresh appends always land > pre_import_frontier and dups are pre-filtered, so the post-append `< frontier` arm is unreachable under first-order mutation; `<`/`==`/`<=` classify identically.",
    },
    RegisteredExclusion {
        regex: r"crates/core/src/store/import\.rs:.*replace < with <= in import_events",
        category: ExclusionCategory::Equivalent,
        witness: Some("crates/core/tests/import_events.rs::import_events_reimport_is_noop_and_preserves_raw_payload_bytes"),
        reason: "Same unreachable post-append dedup arm as the `< -> ==` mutant; no receipt in the loop is ever <= pre_import_frontier.",
    },
    RegisteredExclusion {
        regex: r"crates/core/src/store/import\.rs:.*replace \|\| with && in import_key_already_present",
        category: ExclusionCategory::Equivalent,
        witness: Some("crates/core/tests/import_events.rs::import_events_reimport_is_noop_and_preserves_raw_payload_bytes"),
        reason: "The post-append reclassification backstops a broken pre-filter: under `|| -> &&` dups reach append_batch, collapse to their old (< frontier) sequence, and are still counted deduplicated — counts unchanged.",
    },
    RegisteredExclusion {
        regex: r"crates/core/src/store/import\.rs:.*replace ImportSelector::all -> Self with Default::default",
        category: ExclusionCategory::TimeoutAbort,
        witness: None,
        reason: "Default for ImportSelector IS all(); rewriting all() to Default::default() recurses to a stack abort, caught by timeout — not a behavior change.",
    },
    RegisteredExclusion {
        regex: r"fs\.rs:2[3-6][0-9]:.*reflink_impl",
        category: ExclusionCategory::NotCompiled,
        witness: None,
        reason: "macOS/non-linux cfg variants of reflink_impl are not compiled on the Linux CI runner, so cargo-mutants cannot apply or test them there.",
    },
    RegisteredExclusion {
        regex: r"file_classification\.rs:.*replace match guard segment_id.as_u64\(\) == active_segment_id with true in StoreFileKind::fork_strategy",
        category: ExclusionCategory::Equivalent,
        witness: Some("crates/core/tests/store_fork_isolation.rs::fork_report_records_concrete_strategy_counts_and_nonzero_digests"),
        reason: "active_segment_id is always the max live segment id, so no segment has id > active; `== active` and `true` (>= active) select the same arm for every reachable segment.",
    },
    RegisteredExclusion {
        regex: r"crates/core/src/store/projection/flow/mod\.rs:.*delete ! in execute_full_replay",
        category: ExclusionCategory::DiagnosticOnly,
        witness: None,
        reason: "The `!` only guards a `tracing::debug!` emission in execute_full_replay; deleting it changes no functional behavior, so no behavioral witness can exist.",
    },
];

/// True when `regex` is a registered, categorized exclusion. meta-gate calls this
/// to decide whether an added `--exclude-re` is a governed, witnessed denominator
/// change (allowed) versus an unregistered one (flagged as a weakening). The
/// structural `mutation-exclusion-registry` gate separately proves the witness
/// resolves, so registry membership is a sufficient meta-gate signal.
pub(crate) fn is_registered(regex: &str) -> bool {
    REGISTRY.iter().any(|entry| entry.regex == regex)
}

/// One mutation-exclusion regex, decomposed into the parts the gate validates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExclusionAnchor {
    /// The raw regex string, for diagnostics.
    pub(crate) regex: String,
    /// The file anchor (regex prefix up to the first `:`), `\.`-unescaped, e.g.
    /// `crates/core/src/store/import.rs` or the bare `fs.rs`.
    pub(crate) file_suffix: String,
    /// The most specific symbol the mutant description names, e.g. `import_events`
    /// or `aos`. `None` when no identifier could be extracted (itself a failure).
    pub(crate) symbol: Option<String>,
}

/// Production entry: validate every exclusion regex in the live `lanes.rs`.
pub(crate) fn check(repo_root: &Path) -> Result<()> {
    let lanes_src = std::fs::read_to_string(repo_root.join(LANES_RS_REL))
        .with_context(|| format!("read {LANES_RS_REL}"))?;
    let anchors = extract_exclusion_anchors(&lanes_src);
    ensure(
        !anchors.is_empty(),
        format!(
            "structural-check (mutation-exclusion-registry): no exclusion regexes found in \
             {LANES_RS_REL}. The extractor or the registry shape changed — a registry with zero \
             validated exclusions would let every vacuous/stale exclusion pass unchecked."
        ),
    )?;
    let tracked = tracked_repo_files(repo_root)?;
    let tracked_rel: Vec<String> = tracked
        .iter()
        .map(|path| relative(repo_root, path))
        .collect();
    validate_anchors(repo_root, &anchors, &tracked_rel)?;
    validate_registry(repo_root, &anchors)
}

/// Lockstep the live `lanes.rs` exclusions against the witnessed `REGISTRY`, then
/// prove each entry is properly categorized: an `Equivalent` exclusion must cite
/// a witness test that resolves to a real `#[test]`; the other categories carry a
/// non-empty reason and no witness. This is what lets meta-gate trust the
/// registry instead of demanding a human weaken-stamp.
pub(crate) fn validate_registry(repo_root: &Path, anchors: &[ExclusionAnchor]) -> Result<()> {
    let lane_regexes: BTreeSet<&str> = anchors.iter().map(|a| a.regex.as_str()).collect();
    let registry_regexes: BTreeSet<&str> = REGISTRY.iter().map(|entry| entry.regex).collect();
    let unregistered: Vec<&str> = lane_regexes
        .difference(&registry_regexes)
        .copied()
        .collect();
    ensure(
        unregistered.is_empty(),
        format!(
            "structural-check (mutation-exclusion-registry): {} exclusion(s) in {LANES_RS_REL} have \
             no categorized registry entry. Every `--exclude-re` must be registered (category + \
             witness) so meta-gate can trust it without a human weaken-stamp:\n  {}",
            unregistered.len(),
            unregistered.join("\n  ")
        ),
    )?;
    let stale: Vec<&str> = registry_regexes
        .difference(&lane_regexes)
        .copied()
        .collect();
    ensure(
        stale.is_empty(),
        format!(
            "structural-check (mutation-exclusion-registry): {} REGISTRY entry/entries are no longer \
             present in {LANES_RS_REL} (stale registration):\n  {}",
            stale.len(),
            stale.join("\n  ")
        ),
    )?;
    let mut cache = SourceCache::new(repo_root);
    let mut failures: Vec<String> = Vec::new();
    for entry in REGISTRY {
        if entry.reason.trim().is_empty() {
            failures.push(format!("exclusion has an empty reason: {}", entry.regex));
        }
        match entry.category {
            ExclusionCategory::Equivalent => match entry.witness {
                None => failures.push(format!(
                    "equivalent exclusion cites no witness test (equivalence must be witnessed): {}",
                    entry.regex
                )),
                Some(witness) => {
                    if let Err(why) = witness_resolves(repo_root, &mut cache, witness) {
                        failures.push(format!(
                            "equivalent exclusion `{}` witness `{witness}` {why}",
                            entry.regex
                        ));
                    }
                }
            },
            ExclusionCategory::DiagnosticOnly
            | ExclusionCategory::TimeoutAbort
            | ExclusionCategory::NotCompiled => {
                if entry.witness.is_some() {
                    failures.push(format!(
                        "non-equivalent exclusion should not carry a witness test: {}",
                        entry.regex
                    ));
                }
            }
        }
    }
    ensure(
        failures.is_empty(),
        format!(
            "structural-check (mutation-exclusion-registry): {} registry entry/entries are not \
             properly witnessed/categorized:\n  {}",
            failures.len(),
            failures.join("\n  ")
        ),
    )
}

/// Resolve a `path::fn` witness reference to a real `#[test]` in the tree.
fn witness_resolves(
    repo_root: &Path,
    cache: &mut SourceCache,
    witness: &str,
) -> Result<(), String> {
    let (rel, fn_name) = witness
        .rsplit_once("::")
        .ok_or_else(|| "must be `path::fn`".to_string())?;
    let full = resolve_repo_or_core_path(repo_root, rel);
    if !full.is_file() {
        return Err(format!("points at a missing file {rel}"));
    }
    match file_declares_test_fn(cache, &full, fn_name) {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!("names no `#[test]` fn `{fn_name}` in {rel}")),
        Err(error) => Err(format!("could not parse {rel}: {error}")),
    }
}

/// Testable core: each anchor must resolve to a real file that contains its
/// claimed symbol. A RED fixture drives a synthetic anchor list (including the
/// historical `config.rs`-vs-`config/types.rs` vacuity) against the real tree.
pub(crate) fn validate_anchors(
    repo_root: &Path,
    anchors: &[ExclusionAnchor],
    tracked_rel: &[String],
) -> Result<()> {
    let mut failures: Vec<String> = Vec::new();
    for anchor in anchors {
        let matching: Vec<&String> = tracked_rel
            .iter()
            .filter(|file| file.ends_with(&anchor.file_suffix))
            .collect();
        if matching.is_empty() {
            failures.push(format!(
                "exclusion regex anchors to `{}` but NO tracked file matches that path — vacuous \
                 exclusion (excludes zero mutants):\n    {}",
                anchor.file_suffix, anchor.regex
            ));
            continue;
        }
        let Some(symbol) = anchor.symbol.as_deref() else {
            failures.push(format!(
                "exclusion regex names no extractable mutated symbol — cannot prove it anchors a \
                 real site:\n    {}",
                anchor.regex
            ));
            continue;
        };
        let symbol_seen = matching.iter().any(|file| {
            std::fs::read_to_string(repo_root.join(file))
                .map(|content| contains_identifier(&content, symbol))
                .unwrap_or(false)
        });
        if !symbol_seen {
            let files: Vec<&str> = matching.iter().map(|f| f.as_str()).collect();
            failures.push(format!(
                "exclusion regex claims to mutate `{symbol}` in `{}`, but no matching file \
                 ({}) contains that symbol — the anchor is stale or points at the wrong file \
                 (vacuous exclusion):\n    {}",
                anchor.file_suffix,
                files.join(", "),
                anchor.regex
            ));
        }
    }
    ensure(
        failures.is_empty(),
        format!(
            "structural-check (mutation-exclusion-registry): {} mutation-exclusion regex(es) in \
             {LANES_RS_REL} do not anchor a real mutation site. Each excludes a mutant from the \
             score denominator, so a stale/wrong anchor is a silent gate weakening. Fix the path \
             or the symbol:\n\n{}",
            failures.len(),
            failures.join("\n\n")
        ),
    )
}

/// Extract every exclusion-regex literal from `lanes.rs`. We scope to consts and
/// arrays whose declaration carries a watched marker (`EXCLUDE_RES` or
/// `EQUIVALENT_MUTANT`, the same markers meta_gate watches), then pull every raw
/// string (`r"..."` / `r#"..."#`) from the declaration through its terminator.
pub(crate) fn extract_exclusion_anchors(source: &str) -> Vec<ExclusionAnchor> {
    let mut anchors = Vec::new();
    let mut in_block = false;
    for line in source.lines() {
        let code = strip_line_comment(line);
        if !in_block && (code.contains("EXCLUDE_RES") || code.contains("EQUIVALENT_MUTANT")) {
            // A declaration line carrying a watched marker opens a block. Single
            // `&str` consts close on the same line; arrays close on `];`.
            in_block = true;
        }
        if in_block {
            for regex in extract_raw_strings(code) {
                if let Some(anchor) = parse_anchor(&regex) {
                    anchors.push(anchor);
                }
            }
            // Close on array terminator OR a single-line const (`= r"...";`).
            if code.contains("];") || (code.contains("&str") && code.contains(';')) {
                in_block = false;
            }
        }
    }
    anchors
}

/// Decompose a raw exclusion regex into (file_suffix, symbol). Returns `None`
/// when the string has no `:` separator (not a mutation-exclusion regex shape).
pub(crate) fn parse_anchor(regex: &str) -> Option<ExclusionAnchor> {
    let colon = regex.find(':')?;
    let file_suffix = regex[..colon].replace("\\.", ".");
    // Reject anything that does not look like a source-file anchor.
    if !file_suffix.ends_with(".rs") {
        return None;
    }
    let description = &regex[colon + 1..];
    Some(ExclusionAnchor {
        regex: regex.to_string(),
        file_suffix,
        symbol: extract_symbol(description),
    })
}

/// Pull the most specific mutated symbol from a cargo-mutants description tail.
/// Priority: the identifier after ` in ` (cargo-mutants' "in `<fn>`" suffix), then
/// the method of a `Type::method ->` return-type mutant, then the last bare
/// identifier (e.g. `reflink_impl`).
fn extract_symbol(description: &str) -> Option<String> {
    if let Some(pos) = description.rfind(" in ") {
        if let Some(ident) = first_qualified_ident(&description[pos + 4..]) {
            return Some(ident);
        }
    }
    if let Some(arrow) = description.find("->") {
        if let Some(ident) = last_qualified_ident(&description[..arrow]) {
            return Some(ident);
        }
    }
    last_bare_ident(description)
}

/// Final `::`-segment of the first `Type::method` (or bare ident) token.
fn first_qualified_ident(text: &str) -> Option<String> {
    let token = text
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
        .find(|t| !t.is_empty())?;
    last_segment(token)
}

/// Final `::`-segment of the last `Type::method` token before `->`.
fn last_qualified_ident(text: &str) -> Option<String> {
    let token = text
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
        .rfind(|t| t.contains("::") || is_ident(t))?;
    last_segment(token)
}

/// The last plain identifier in the text (regex meta stripped).
fn last_bare_ident(text: &str) -> Option<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .rfind(|t| is_ident(t))
        .map(str::to_string)
}

fn last_segment(token: &str) -> Option<String> {
    token.rsplit("::").find(|s| is_ident(s)).map(str::to_string)
}

fn is_ident(token: &str) -> bool {
    !token.is_empty()
        && token
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
        && token.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// True when `ident` appears in `content` as a whole identifier (not a substring
/// of a longer identifier — so `aos` does not match inside `chaos`).
pub(crate) fn contains_identifier(content: &str, ident: &str) -> bool {
    let bytes = content.as_bytes();
    let mut from = 0;
    while let Some(rel) = content[from..].find(ident) {
        let start = from + rel;
        let end = start + ident.len();
        let before_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
        let after_ok = end == bytes.len() || !is_ident_byte(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Extract `r"..."` and `r#"..."#` raw-string contents from one line. Shared
/// with meta_gate so it can recover an added exclusion regex from a diff line and
/// check it against [`is_registered`].
pub(crate) fn extract_raw_strings(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'r' && i + 1 < bytes.len() && (bytes[i + 1] == b'"' || bytes[i + 1] == b'#')
        {
            // Count leading '#'s.
            let mut hashes = 0;
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] == b'#' {
                hashes += 1;
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'"' {
                let body_start = j + 1;
                let closer: String = std::iter::once('"')
                    .chain(std::iter::repeat_n('#', hashes))
                    .collect();
                if let Some(rel) = line[body_start..].find(&closer) {
                    out.push(line[body_start..body_start + rel].to_string());
                    i = body_start + rel + closer.len();
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

fn strip_line_comment(line: &str) -> &str {
    match line.find("//") {
        Some(idx) => &line[..idx],
        None => line,
    }
}

#[cfg(test)]
#[path = "mutation_exclusion_registry_tests.rs"]
mod tests;
