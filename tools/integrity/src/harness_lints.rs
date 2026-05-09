use crate::repo_surface::resolve_repo_or_core_path;
use anyhow::{bail, Context, Result};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

const VALID_PATTERNS: &[&str] = &[
    "Fault-Injection Harness",
    "Equivalence Harness",
    "Property Harness",
    "State-Machine Harness",
    "Oracle Harness",
];

const REQUIRED_FIELDS: &[&str] = &[
    "Harness pattern",
    "Location",
    "Command used",
    "Line/function coverage delta",
    "Mutation delta",
    "Remaining known blind spots",
];

const APPROVED_COMMAND_PREFIXES: &[&str] = &[
    "cargo test",
    "BATPAK_RUN_CHAOS=1 cargo test",
    "CARGO_INCREMENTAL=0 cargo mutants",
    "cargo xtask",
];

struct HeaderDebt {
    path: &'static str,
    reason: &'static str,
    target: &'static str,
}

struct OversizeDebt {
    path: &'static str,
    max_lines: usize,
    reason: &'static str,
    target: &'static str,
}

const HEADER_DEBT_ALLOWLIST: &[HeaderDebt] = &[
    HeaderDebt {
        path: "tests/chaos.rs",
        reason: "legacy chaos entrypoint predates module-header doctrine",
        target: "add header when chaos module is next touched",
    },
    HeaderDebt {
        path: "tests/chaos/dm_flakey.rs",
        reason: "privileged dm-flakey helper predates module-header doctrine",
        target: "split helper/header during next chaos hardening pass",
    },
    HeaderDebt {
        path: "tests/chaos/scenarios/batch_commit_written.rs",
        reason: "chaos scenario has prose header but not canonical fields",
        target: "normalize scenario headers by v0.8.0",
    },
    HeaderDebt {
        path: "tests/chaos/scenarios/single_append_written.rs",
        reason: "chaos scenario has prose header but not canonical fields",
        target: "normalize scenario headers by v0.8.0",
    },
    HeaderDebt {
        path: "tests/chaos/scenarios/smoke.rs",
        reason: "minimal chaos smoke predates module-header doctrine",
        target: "add header when smoke scenario changes",
    },
    HeaderDebt {
        path: "tests/chaos_testing.rs",
        reason: "legacy chaos suite has partial header only",
        target: "split or normalize by v0.8.0",
    },
    HeaderDebt {
        path: "tests/cold_start_recovery.rs",
        reason: "legacy cold-start recovery suite predates module-header doctrine",
        target: "add header when recovery matrix changes",
    },
    HeaderDebt {
        path: "tests/control_plane_surface.rs",
        reason: "large control-plane suite predates module-header doctrine",
        target: "split by writer-control seam by v0.8.0",
    },
    HeaderDebt {
        path: "tests/derive_event_sourced_errors.rs",
        reason: "trybuild wrapper predates module-header doctrine",
        target: "normalize compile-fail wrapper headers together",
    },
    HeaderDebt {
        path: "tests/derive_event_sourced_generic.rs",
        reason: "derive generic parity suite predates module-header doctrine",
        target: "add header with next derive-surface edit",
    },
    HeaderDebt {
        path: "tests/derive_event_sourced_parity.rs",
        reason: "derive parity suite predates module-header doctrine",
        target: "add header with next derive-surface edit",
    },
    HeaderDebt {
        path: "tests/derive_eventpayload_errors.rs",
        reason: "trybuild wrapper predates module-header doctrine",
        target: "normalize compile-fail wrapper headers together",
    },
    HeaderDebt {
        path: "tests/derive_multi_event_reactor_errors.rs",
        reason: "trybuild wrapper predates module-header doctrine",
        target: "normalize compile-fail wrapper headers together",
    },
    HeaderDebt {
        path: "tests/deterministic_concurrency.rs",
        reason: "concurrency suite predates module-header doctrine",
        target: "add header when schedule matrix changes",
    },
    HeaderDebt {
        path: "tests/durable_frontier_waits.rs",
        reason: "durable wait suite has partial header only",
        target: "add missing CATCHES/SEEDED by v0.8.0",
    },
    HeaderDebt {
        path: "tests/fuzz_chaos_feedback.rs",
        reason: "fuzz-chaos suite has partial header only",
        target: "add missing CATCHES/SEEDED by v0.8.0",
    },
    HeaderDebt {
        path: "tests/index_filter_composition.rs",
        reason: "oracle suite predates module-header doctrine",
        target: "add header when query oracle changes",
    },
    HeaderDebt {
        path: "tests/mmap_cold_start.rs",
        reason: "mmap parity suite predates module-header doctrine",
        target: "add header when mmap path changes",
    },
    HeaderDebt {
        path: "tests/perf_gates.rs",
        reason: "perf gate suite has partial header only",
        target: "add missing CATCHES/SEEDED by v0.8.0",
    },
    HeaderDebt {
        path: "tests/projection_cache.rs",
        reason: "cache suite has partial header only",
        target: "split and normalize by v0.8.0",
    },
    HeaderDebt {
        path: "tests/replay_consistency.rs",
        reason: "replay parity suite predates module-header doctrine",
        target: "add header when replay matrix changes",
    },
    HeaderDebt {
        path: "tests/segment_scan_hardening.rs",
        reason: "segment hardening suite predates module-header doctrine",
        target: "add header when corruption matrix changes",
    },
    HeaderDebt {
        path: "tests/store_advanced.rs",
        reason: "legacy omnibus suite has partial header only",
        target: "split by seam by v0.8.0",
    },
];

const OVERSIZE_HARNESS_ALLOWLIST: &[OversizeDebt] = &[
    OversizeDebt {
        path: "tests/chaos_testing.rs",
        max_lines: 1017,
        reason: "legacy chaos matrix remains intact until split",
        target: "split low-level byte corruption cases by v0.8.0",
    },
    OversizeDebt {
        path: "tests/control_plane_surface.rs",
        max_lines: 1055,
        reason: "control-plane proofs share fixtures today",
        target: "split ticket/fence/pressure seams by v0.8.0",
    },
    OversizeDebt {
        path: "tests/cursor_durability.rs",
        max_lines: 578,
        reason: "cursor checkpoint lifecycle matrix remains coupled",
        target: "split checkpoint corruption vs delivery progress by v0.8.0",
    },
    OversizeDebt {
        path: "tests/durable_frontier_semantics.rs",
        max_lines: 1044,
        reason: "durable frontier semantic phases still share setup",
        target: "split lifecycle/frontier cases by v0.8.0",
    },
    OversizeDebt {
        path: "tests/durable_frontier_waits.rs",
        max_lines: 597,
        reason: "wait and gate API phases share controlled projection fixtures",
        target: "split wait surfaces from append-gate surfaces by v0.8.0",
    },
    OversizeDebt {
        path: "tests/fuzz_chaos_feedback.rs",
        max_lines: 757,
        reason: "fuzz-chaos policy matrix remains single-file",
        target: "split generators from policy assertions by v0.8.0",
    },
    OversizeDebt {
        path: "tests/perf_gates.rs",
        max_lines: 1345,
        reason: "hardware-dependent gates share calibration constants",
        target: "split gate families by v0.8.0",
    },
    OversizeDebt {
        path: "tests/projection_cache.rs",
        max_lines: 1213,
        reason: "cache corruption and freshness matrix share cache fixtures",
        target: "split freshness modes from corruption shapes by v0.8.0",
    },
    OversizeDebt {
        path: "tests/raw_projection_mode.rs",
        max_lines: 923,
        reason: "raw/derived projection equivalence matrix shares event fixtures",
        target: "split replay lane families by v0.8.0",
    },
    OversizeDebt {
        path: "tests/segment_scan_hardening.rs",
        max_lines: 663,
        reason: "segment corruption shapes share frame-building helpers",
        target: "split helper module from case table by v0.8.0",
    },
    OversizeDebt {
        path: "tests/store_advanced.rs",
        max_lines: 1675,
        reason: "legacy omnibus store suite is being burned down over time",
        target: "move cursor/lifecycle remnants to focused suites by v0.8.0",
    },
];

#[derive(Default)]
struct LedgerEntry {
    title: String,
    section: String,
    line: usize,
    pattern: Option<String>,
    fields: BTreeSet<String>,
    locations: Vec<String>,
    commands: Vec<String>,
}

pub fn check(repo_root: &Path, tracked_files: &[PathBuf]) -> Result<()> {
    let tracked = tracked_set(repo_root, tracked_files);
    let entries = parse_ledger(repo_root)?;
    check_entries(repo_root, &tracked, &entries)?;
    let ledger_rust_files = ledger_rust_files(&entries);
    check_module_headers(repo_root, &ledger_rust_files)?;
    check_line_caps(repo_root, &ledger_rust_files)?;
    Ok(())
}

fn parse_ledger(repo_root: &Path) -> Result<Vec<LedgerEntry>> {
    let path = repo_root.join("HARNESS_LEDGER.md");
    let content = fs::read_to_string(&path).context("read HARNESS_LEDGER.md")?;
    let mut current_section = String::new();
    let mut entries = Vec::new();
    let mut current: Option<LedgerEntry> = None;
    let mut active_block: Option<&'static str> = None;

    for (index, line) in content.lines().enumerate() {
        let line_no = index + 1;
        if let Some(section) = line.strip_prefix("## ") {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current_section = section.trim().to_owned();
            if current_section != "Harness Ledger" {
                ensure(
                    VALID_PATTERNS.contains(&current_section.as_str()),
                    format!(
                        "HARNESS_LEDGER.md:{line_no}: unknown harness section `{current_section}`"
                    ),
                )?;
            }
            active_block = None;
            continue;
        }

        if let Some(title) = line.strip_prefix("### Invariant: ") {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(LedgerEntry {
                title: title.trim().to_owned(),
                section: current_section.clone(),
                line: line_no,
                ..LedgerEntry::default()
            });
            active_block = None;
            continue;
        }

        let Some(entry) = current.as_mut() else {
            continue;
        };

        if let Some(field) = field_name(line) {
            entry.fields.insert(field.to_owned());
            active_block = match field {
                "Location" => Some("location"),
                "Command used" => Some("command"),
                _ => None,
            };
            if field == "Harness pattern" {
                entry.pattern = backtick_value(line).map(str::to_owned);
            }
            continue;
        }

        if line.starts_with("- ") {
            active_block = None;
        }

        match active_block {
            Some("location") => {
                if let Some(path) = backtick_value(line) {
                    entry.locations.push(path.to_owned());
                }
            }
            Some("command") => {
                if let Some(command) = list_item(line) {
                    entry.commands.push(command.to_owned());
                }
            }
            _ => {}
        }
    }
    if let Some(entry) = current {
        entries.push(entry);
    }
    ensure(
        !entries.is_empty(),
        "HARNESS_LEDGER.md has no invariant entries",
    )?;
    Ok(entries)
}

fn check_entries(
    repo_root: &Path,
    tracked: &BTreeSet<String>,
    entries: &[LedgerEntry],
) -> Result<()> {
    for entry in entries {
        ensure(
            VALID_PATTERNS.contains(&entry.section.as_str()),
            format!(
                "HARNESS_LEDGER.md:{}: invariant `{}` appears outside a valid harness section",
                entry.line, entry.title
            ),
        )?;
        for required in REQUIRED_FIELDS {
            ensure(
                entry.fields.contains(*required),
                format!(
                    "HARNESS_LEDGER.md:{}: invariant `{}` missing `{required}`",
                    entry.line, entry.title
                ),
            )?;
        }
        let Some(pattern) = entry.pattern.as_deref() else {
            bail!(
                "HARNESS_LEDGER.md:{}: invariant `{}` missing backticked harness pattern",
                entry.line,
                entry.title
            );
        };
        ensure(
            pattern == entry.section,
            format!(
                "HARNESS_LEDGER.md:{}: invariant `{}` pattern `{pattern}` must match section `{}`",
                entry.line, entry.title, entry.section
            ),
        )?;
        ensure(
            !entry.locations.is_empty(),
            format!(
                "HARNESS_LEDGER.md:{}: invariant `{}` has no locations",
                entry.line, entry.title
            ),
        )?;
        ensure(
            !entry.commands.is_empty(),
            format!(
                "HARNESS_LEDGER.md:{}: invariant `{}` has no commands",
                entry.line, entry.title
            ),
        )?;
        for path in &entry.locations {
            ensure(
                resolve_repo_or_core_path(repo_root, path).exists(),
                format!(
                    "HARNESS_LEDGER.md:{}: location `{path}` does not exist",
                    entry.line
                ),
            )?;
            ensure(
                tracked.contains(path) || tracked.contains(&format!("crates/core/{path}")),
                format!(
                    "HARNESS_LEDGER.md:{}: location `{path}` is not git-tracked",
                    entry.line
                ),
            )?;
        }
        for command in &entry.commands {
            ensure(
                APPROVED_COMMAND_PREFIXES
                    .iter()
                    .any(|prefix| command.starts_with(prefix)),
                format!(
                    "HARNESS_LEDGER.md:{}: command `{command}` must start with an approved repo command",
                    entry.line
                ),
            )?;
        }
    }
    Ok(())
}

fn check_module_headers(repo_root: &Path, rust_files: &BTreeSet<String>) -> Result<()> {
    let allowlist = header_allowlist()?;
    for path in rust_files {
        let content = fs::read_to_string(resolve_repo_or_core_path(repo_root, path))
            .with_context(|| format!("read {path}"))?;
        let header = content.lines().take(40).collect::<Vec<_>>().join("\n");
        let complete =
            header.contains("PROVES:") && header.contains("CATCHES:") && header.contains("SEEDED:");
        if complete {
            ensure(
                !allowlist.contains_key(path.as_str()),
                format!("harness header allowlist entry for `{path}` is stale; remove it"),
            )?;
        } else {
            ensure(
                allowlist.contains_key(path.as_str()),
                format!("doctrine-bearing harness `{path}` must declare PROVES/CATCHES/SEEDED in its first 40 lines"),
            )?;
        }
    }
    Ok(())
}

fn check_line_caps(repo_root: &Path, rust_files: &BTreeSet<String>) -> Result<()> {
    let allowlist = oversize_allowlist()?;
    for path in rust_files {
        let content = fs::read_to_string(resolve_repo_or_core_path(repo_root, path))
            .with_context(|| format!("read {path}"))?;
        let line_count = content.lines().count();
        if line_count <= 500 {
            ensure(
                !allowlist.contains_key(path.as_str()),
                format!("oversize harness allowlist entry for `{path}` is stale; remove it"),
            )?;
            continue;
        }
        let Some(debt) = allowlist.get(path.as_str()) else {
            bail!("doctrine-bearing harness `{path}` has {line_count} lines; split it or add an explicit capped debt entry");
        };
        ensure(
            line_count <= debt.max_lines,
            format!(
                "oversize harness `{path}` grew from cap {} to {line_count} lines; split it before adding more",
                debt.max_lines
            ),
        )?;
    }
    Ok(())
}

fn ledger_rust_files(entries: &[LedgerEntry]) -> BTreeSet<String> {
    entries
        .iter()
        .flat_map(|entry| entry.locations.iter())
        .filter(|path| path.ends_with(".rs") && path.starts_with("tests/"))
        .cloned()
        .collect()
}

fn header_allowlist() -> Result<HashMap<&'static str, &'static HeaderDebt>> {
    let mut map = HashMap::new();
    for debt in HEADER_DEBT_ALLOWLIST {
        ensure(
            !debt.reason.is_empty(),
            format!("header debt `{}` missing reason", debt.path),
        )?;
        ensure(
            !debt.target.is_empty(),
            format!("header debt `{}` missing target", debt.path),
        )?;
        ensure(
            map.insert(debt.path, debt).is_none(),
            format!("duplicate header debt `{}`", debt.path),
        )?;
    }
    Ok(map)
}

fn oversize_allowlist() -> Result<HashMap<&'static str, &'static OversizeDebt>> {
    let mut map = HashMap::new();
    for debt in OVERSIZE_HARNESS_ALLOWLIST {
        ensure(
            !debt.reason.is_empty(),
            format!("oversize debt `{}` missing reason", debt.path),
        )?;
        ensure(
            !debt.target.is_empty(),
            format!("oversize debt `{}` missing target", debt.path),
        )?;
        ensure(
            map.insert(debt.path, debt).is_none(),
            format!("duplicate oversize debt `{}`", debt.path),
        )?;
    }
    Ok(map)
}

fn field_name(line: &str) -> Option<&str> {
    REQUIRED_FIELDS
        .iter()
        .copied()
        .find(|field| line.starts_with(&format!("- {field}:")))
}

fn backtick_value(line: &str) -> Option<&str> {
    let start = line.find('`')?;
    let rest = &line[start + 1..];
    let end = rest.find('`')?;
    Some(&rest[..end])
}

fn list_item(line: &str) -> Option<&str> {
    line.trim_start()
        .strip_prefix("- ")
        .map(str::trim)
        .map(|item| item.trim_matches('`'))
}

fn tracked_set(repo_root: &Path, tracked_files: &[PathBuf]) -> BTreeSet<String> {
    tracked_files
        .iter()
        .map(|path| relative(repo_root, path))
        .collect()
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn ensure(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        bail!(message.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_repo(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "batpak-harness-lints-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp repo");
        path
    }

    fn complete_entry(location: &str) -> LedgerEntry {
        LedgerEntry {
            title: "INV-TEST".to_owned(),
            section: "Property Harness".to_owned(),
            line: 12,
            pattern: Some("Property Harness".to_owned()),
            fields: REQUIRED_FIELDS
                .iter()
                .map(|field| (*field).to_owned())
                .collect(),
            locations: vec![location.to_owned()],
            commands: vec!["cargo test --test synthetic".to_owned()],
        }
    }

    #[test]
    fn synthetic_well_formed_ledger_entry_is_accepted() {
        let repo = temp_repo("accept");
        let location = "tests/synthetic.rs";
        fs::create_dir_all(repo.join("tests")).expect("create tests dir");
        fs::write(
            repo.join(location),
            "//! PROVES: synthetic proof.\n//! CATCHES: synthetic regression.\n//! SEEDED: deterministic.\n",
        )
        .expect("write synthetic test");
        let tracked = BTreeSet::from([location.to_owned()]);
        let entries = vec![complete_entry(location)];

        check_entries(&repo, &tracked, &entries).expect("valid ledger entry");
        check_module_headers(&repo, &ledger_rust_files(&entries)).expect("valid header");
        check_line_caps(&repo, &ledger_rust_files(&entries)).expect("valid line cap");

        fs::remove_dir_all(repo).expect("remove temp repo");
    }

    #[test]
    fn synthetic_malformed_ledger_entry_is_rejected() {
        let repo = temp_repo("reject");
        let location = "tests/synthetic.rs";
        fs::create_dir_all(repo.join("tests")).expect("create tests dir");
        fs::write(repo.join(location), "//! PROVES: only one field.\n")
            .expect("write synthetic test");
        let tracked = BTreeSet::from([location.to_owned()]);
        let mut entry = complete_entry(location);
        entry.fields.remove("Mutation delta");
        let entries = vec![entry];

        let err = check_entries(&repo, &tracked, &entries).expect_err("missing field rejected");
        assert!(
            err.to_string().contains("missing `Mutation delta`"),
            "unexpected error: {err:?}"
        );

        fs::remove_dir_all(repo).expect("remove temp repo");
    }
}
