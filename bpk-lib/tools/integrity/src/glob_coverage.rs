//! Vacuous-glob killer (GAUNT-FAULT-3 sub-item, slug `mutation-glob-coverage`).
//!
//! Every path/glob in the mutation seam registries
//! (`tools/xtask/src/commands/mutants/lanes.rs`, the `*_MUTANT_FILES` consts that
//! `critical_mutation_seams()` and the repo-wide lanes resolve to) must match at
//! least one real tracked file. A typo'd glob matches nothing, produces zero
//! mutants, and (because the smoke lanes are diff-scoped — see policy.rs) yields a
//! VACUOUS PASS in cloud mutation runs. This check turns that silent hole into a
//! hard build failure: a glob matching no tracked file is reported.
//!
//! The seam registry is read as SOURCE TEXT (not via a cross-crate dependency):
//! we parse `lanes.rs`, collect every `const NAME_MUTANT_FILES: &[&str] = &[ .. ]`
//! block, and extract the quoted glob literals. `*_MUTANT_EXCLUDE_RES` /
//! `*_MUTANT` regex consts are deliberately NOT scanned — they hold regexes, not
//! globs. Keeping the extraction text-based keeps the integrity tool free of an
//! xtask build dependency while still validating the exact literals xtask ships.

use crate::repo_surface::{ensure, relative, tracked_repo_files};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Repo-relative path to the mutation seam registry whose globs are validated.
pub(crate) const LANES_RS_REL: &str = "tools/xtask/src/commands/mutants/lanes.rs";

/// Globs that are KNOWN to match no tracked file today and are explicitly waived
/// so the gate lands green over the current `lanes.rs` (which is owned elsewhere
/// and read-only to this gate). Each entry is a real finding: a stale seam glob
/// left behind by a module refactor, silently producing zero mutants. The waiver
/// shrinks as `lanes.rs` is fixed — a waived glob that now matches a file is
/// reported (anti-rot), and a NEW dead glob not on this list still fails hard.
///
/// This list is now EMPTY: the two stale seam globs
/// (`crates/syncbat/src/register_store.rs`, `crates/netbat/src/transport.rs`)
/// were repointed at their directory-module forms (`register_store/**/*.rs`,
/// `transport/**/*.rs`) in lanes.rs, so they match tracked files again and no
/// longer need a waiver. Any NEW dead glob still fails the gate hard.
pub(crate) const KNOWN_DEAD_GLOBS: &[&str] = &[];

/// Production entry: validate every `*_MUTANT_FILES` glob in the live `lanes.rs`
/// against the live tracked-file set.
pub(crate) fn check(repo_root: &Path) -> Result<()> {
    let lanes_src = std::fs::read_to_string(repo_root.join(LANES_RS_REL))
        .with_context(|| format!("read {LANES_RS_REL}"))?;
    let globs = extract_mutant_file_globs(&lanes_src);
    ensure(
        !globs.is_empty(),
        format!(
            "structural-check (mutation-glob-coverage): no `*_MUTANT_FILES` glob literals found in \
             {LANES_RS_REL}. The extractor or the registry shape changed — a registry with zero \
             validated globs would let every typo'd seam pass vacuously."
        ),
    )?;
    let tracked = tracked_repo_files(repo_root)?;
    check_globs_with_waivers(repo_root, &globs, &tracked, KNOWN_DEAD_GLOBS)
}

/// Testable core: assert every glob in `globs` matches ≥1 file in `tracked`,
/// tolerating the explicitly-`waived` dead globs. A waived glob that NOW matches
/// a file is reported (anti-rot), so the waiver cannot rot; a non-waived dead
/// glob fails hard. A RED fixture drives a synthetic glob list (including a
/// nonexistent path, `waived` empty) against the real tracked tree.
pub(crate) fn check_globs_with_waivers(
    repo_root: &Path,
    globs: &[String],
    tracked: &[PathBuf],
    waived: &[&str],
) -> Result<()> {
    let tracked_rel: Vec<String> = tracked
        .iter()
        .map(|path| relative(repo_root, path))
        .collect();
    let matches_any = |glob: &str| tracked_rel.iter().any(|file| glob_matches(glob, file));
    let mut dead: Vec<&str> = Vec::new();
    let mut stale_waivers: Vec<&str> = Vec::new();
    for glob in globs {
        let waived_here = waived.contains(&glob.as_str());
        match (matches_any(glob), waived_here) {
            (true, _) => {}
            (false, true) => {}
            (false, false) => dead.push(glob.as_str()),
        }
    }
    for waiver in waived {
        if matches_any(waiver) {
            stale_waivers.push(waiver);
        }
    }
    ensure(
        stale_waivers.is_empty(),
        format!(
            "structural-check (mutation-glob-coverage): {} waived dead-glob(s) now match a tracked \
             file — remove them from KNOWN_DEAD_GLOBS:\n  {}",
            stale_waivers.len(),
            stale_waivers.join("\n  ")
        ),
    )?;
    ensure(
        dead.is_empty(),
        format!(
            "structural-check (mutation-glob-coverage): {} mutation-seam glob(s) in {LANES_RS_REL} \
             match NO tracked file — each would silently produce zero mutants (a vacuous PASS):\n  {}\n\
             Fix the typo or remove the dead glob. Every `*_MUTANT_FILES` entry must cover real \
             source. [GAUNT-FAULT-3 vacuous-glob killer]",
            dead.len(),
            dead.join("\n  ")
        ),
    )
}

/// Pull every quoted string literal out of every `const <NAME>_MUTANT_FILES`
/// array body in `source`. Robust to multi-line array literals; ignores comments
/// and any const whose name does not end in `_MUTANT_FILES` (so the regex
/// `*_EXCLUDE_RES` / `*_MUTANT` consts are skipped).
pub(crate) fn extract_mutant_file_globs(source: &str) -> Vec<String> {
    let mut globs = Vec::new();
    let lines: Vec<&str> = source.lines().collect();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if is_mutant_files_const_start(line) {
            // Accumulate the array body until the closing `];`.
            let mut body = String::new();
            let mut cursor = index;
            loop {
                let cur = lines[cursor];
                body.push_str(strip_line_comment(cur));
                body.push('\n');
                if cur.contains("];") {
                    break;
                }
                cursor += 1;
                if cursor >= lines.len() {
                    break;
                }
            }
            globs.extend(extract_quoted(&body));
            index = cursor + 1;
            continue;
        }
        index += 1;
    }
    globs
}

/// True when `line` declares a `const <NAME>_MUTANT_FILES: ... = &[` (the array
/// open may be on this line or a following one; we only need the declaration).
fn is_mutant_files_const_start(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") {
        return false;
    }
    let Some(rest) = trimmed.strip_prefix("pub(super) const ").or_else(|| {
        trimmed
            .strip_prefix("pub(crate) const ")
            .or_else(|| trimmed.strip_prefix("const "))
    }) else {
        return false;
    };
    let Some(name_end) = rest.find(':') else {
        return false;
    };
    rest[..name_end].trim_end().ends_with("_MUTANT_FILES")
}

/// Drop a trailing `// ...` line comment so quoted text inside comments is not
/// harvested as a glob. (Glob literals never contain `//`, so this is safe.)
fn strip_line_comment(line: &str) -> &str {
    match line.find("//") {
        Some(pos) => &line[..pos],
        None => line,
    }
}

/// Collect every `"..."` literal in `text` (no escaped-quote handling needed —
/// path globs never contain an escaped quote).
fn extract_quoted(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        if ch == '"' {
            let start = idx + 1;
            let mut end = start;
            for (j, c) in text[start..].char_indices() {
                if c == '"' {
                    end = start + j;
                    break;
                }
            }
            if end > start {
                out.push(text[start..end].to_owned());
            }
            // Advance past the closing quote.
            while let Some(&(j, _)) = chars.peek() {
                if j <= end {
                    chars.next();
                } else {
                    break;
                }
            }
        }
    }
    out
}

/// Match a cargo-mutants `--file` style glob against a repo-relative path.
/// Supports `**` (any number of path segments, including zero) and `*` (any run
/// of non-`/` characters). Both pattern and path use `/` separators.
pub(crate) fn glob_matches(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').collect();
    let seg: Vec<&str> = path.split('/').collect();
    segments_match(&pat, &seg)
}

fn segments_match(pat: &[&str], seg: &[&str]) -> bool {
    match pat.split_first() {
        None => seg.is_empty(),
        Some((&"**", rest)) => {
            // `**` consumes zero or more leading segments.
            (0..=seg.len()).any(|skip| segments_match(rest, &seg[skip..]))
        }
        Some((&first, rest)) => {
            if seg.is_empty() {
                return false;
            }
            segment_glob_matches(first, seg[0]) && segments_match(rest, &seg[1..])
        }
    }
}

/// Match a single path segment against a single pattern segment containing zero
/// or more `*` wildcards (each `*` matches any run of non-`/` characters).
fn segment_glob_matches(pat: &str, seg: &str) -> bool {
    if !pat.contains('*') {
        return pat == seg;
    }
    let parts: Vec<&str> = pat.split('*').collect();
    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !seg[pos..].starts_with(part) {
                return false;
            }
            pos += part.len();
        } else if i == parts.len() - 1 {
            if !seg[pos..].ends_with(part) {
                return false;
            }
        } else {
            match seg[pos..].find(part) {
                Some(found) => pos += found + part.len(),
                None => return false,
            }
        }
    }
    true
}
