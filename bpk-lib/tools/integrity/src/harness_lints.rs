use crate::repo_surface::{resolve_repo_or_core_path, rust_files};
use crate::source_cache::SourceCache;
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
    "Runtime And Boundary Harness",
    "Structural Harness",
];

const REQUIRED_FIELDS: &[&str] = &[
    "Harness pattern",
    "Status",
    "Location",
    "Command used",
    "Line/function coverage delta",
    "Mutation delta",
    "Remaining known blind spots",
];

const VALID_STATUS: &[&str] = &["green", "amber", "red", "unmeasured"];

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
        target: "normalize scenario headers by 0.7.6 correction cut",
    },
    HeaderDebt {
        path: "tests/chaos/scenarios/single_append_written.rs",
        reason: "chaos scenario has prose header but not canonical fields",
        target: "normalize scenario headers by 0.7.6 correction cut",
    },
    HeaderDebt {
        path: "tests/chaos/scenarios/smoke.rs",
        reason: "minimal chaos smoke predates module-header doctrine",
        target: "add header when smoke scenario changes",
    },
    HeaderDebt {
        path: "tests/chaos_testing.rs",
        reason: "legacy chaos suite has partial header only",
        target: "split or normalize by 0.7.6 correction cut",
    },
    HeaderDebt {
        path: "tests/cold_start_recovery.rs",
        reason: "legacy cold-start recovery suite predates module-header doctrine",
        target: "add header when recovery matrix changes",
    },
    HeaderDebt {
        path: "tests/control_plane_surface.rs",
        reason: "large control-plane suite predates module-header doctrine",
        target: "split by writer-control seam by 0.7.6 correction cut",
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
        target: "add missing CATCHES/SEEDED by 0.7.6 correction cut",
    },
    HeaderDebt {
        path: "tests/fuzz_chaos_feedback.rs",
        reason: "fuzz-chaos suite has partial header only",
        target: "add missing CATCHES/SEEDED by 0.7.6 correction cut",
    },
    HeaderDebt {
        path: "tests/index_filter_composition.rs",
        reason: "oracle suite predates module-header doctrine",
        target: "add header when query oracle changes",
    },
    HeaderDebt {
        path: "tests/perf_gates.rs",
        reason: "perf gate suite has partial header only",
        target: "add missing CATCHES/SEEDED by 0.7.6 correction cut",
    },
    HeaderDebt {
        path: "tests/projection_cache.rs",
        reason: "cache suite has partial header only",
        target: "split and normalize by 0.7.6 correction cut",
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
        target: "split by seam by 0.7.6 correction cut",
    },
];

const OVERSIZE_HARNESS_ALLOWLIST: &[OversizeDebt] = &[
    OversizeDebt {
        path: "tests/chaos_testing.rs",
        max_lines: 1017,
        reason: "legacy chaos matrix remains intact until split",
        target: "split low-level byte corruption cases by 0.7.6 correction cut",
    },
    OversizeDebt {
        path: "tests/control_plane_surface.rs",
        max_lines: 1055,
        reason: "control-plane proofs share fixtures today",
        target: "split ticket/fence/pressure seams by 0.7.6 correction cut",
    },
    OversizeDebt {
        path: "tests/cursor_durability.rs",
        max_lines: 578,
        reason: "cursor checkpoint lifecycle matrix remains coupled",
        target: "split checkpoint corruption vs delivery progress by 0.7.6 correction cut",
    },
    OversizeDebt {
        path: "tests/durable_frontier_semantics.rs",
        max_lines: 1044,
        reason: "durable frontier semantic phases still share setup",
        target: "split lifecycle/frontier cases by 0.7.6 correction cut",
    },
    OversizeDebt {
        path: "tests/durable_frontier_waits.rs",
        max_lines: 597,
        reason: "wait and gate API phases share controlled projection fixtures",
        target: "split wait surfaces from append-gate surfaces by 0.7.6 correction cut",
    },
    OversizeDebt {
        path: "tests/fuzz_chaos_feedback.rs",
        max_lines: 757,
        reason: "fuzz-chaos policy matrix remains single-file",
        target: "split generators from policy assertions by 0.7.6 correction cut",
    },
    OversizeDebt {
        path: "tests/perf_gates.rs",
        max_lines: 1350,
        reason: "hardware-dependent gates share calibration constants",
        target: "split gate families by 0.7.6 correction cut",
    },
    OversizeDebt {
        path: "tests/projection_cache.rs",
        max_lines: 1213,
        reason: "cache corruption and freshness matrix share cache fixtures",
        target: "split freshness modes from corruption shapes by 0.7.6 correction cut",
    },
    OversizeDebt {
        path: "tests/raw_projection_mode.rs",
        max_lines: 923,
        reason: "raw/derived projection equivalence matrix shares event fixtures",
        target: "split replay lane families by 0.7.6 correction cut",
    },
    OversizeDebt {
        path: "tests/segment_scan_hardening.rs",
        max_lines: 713,
        reason: "segment corruption shapes share frame-building helpers; bumped 709 -> 713 after SegmentId::from_filename integration test landed",
        target: "split helper module from case table by 0.7.6 correction cut",
    },
    OversizeDebt {
        path: "tests/store_advanced.rs",
        max_lines: 1675,
        reason: "legacy omnibus store suite is being burned down over time",
        target: "move cursor/lifecycle remnants to focused suites by 0.7.6 correction cut",
    },
];

#[derive(Clone, Default)]
struct LedgerEntry {
    title: String,
    section: String,
    line: usize,
    pattern: Option<String>,
    status: Option<String>,
    fields: BTreeSet<String>,
    locations: Vec<String>,
    commands: Vec<String>,
}

pub fn check(
    repo_root: &Path,
    tracked_files: &[PathBuf],
    source_cache: &mut SourceCache,
) -> Result<()> {
    let tracked = tracked_set(repo_root, tracked_files);
    let entries = parse_ledger(repo_root)?;
    check_entries(repo_root, &tracked, &entries, source_cache)?;
    let ledger_rust_files = ledger_rust_files(&entries);
    check_module_headers(repo_root, &ledger_rust_files, source_cache)?;
    check_line_caps(repo_root, &ledger_rust_files, source_cache)?;
    check_no_silent_repo_fixture_skips(repo_root, tracked_files, source_cache)?;
    check_no_tombstone_ignores(repo_root, tracked_files, source_cache)?;
    Ok(())
}

fn parse_ledger(repo_root: &Path) -> Result<Vec<LedgerEntry>> {
    let path = repo_root
        .parent()
        .unwrap_or(repo_root)
        .join("archive/legacy-docs/041_TESTING_LEDGER.md");
    let content = fs::read_to_string(&path).context("read 041_TESTING_LEDGER.md")?;
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
                        "041_TESTING_LEDGER.md:{line_no}: unknown harness section `{current_section}`"
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
            if field == "Status" {
                entry.status = line
                    .split_once(':')
                    .map(|(_, value)| value.trim().to_owned())
                    .filter(|value| !value.is_empty());
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
        "041_TESTING_LEDGER.md has no invariant entries",
    )?;
    Ok(entries)
}

fn check_entries(
    repo_root: &Path,
    tracked: &BTreeSet<String>,
    entries: &[LedgerEntry],
    source_cache: &mut SourceCache,
) -> Result<()> {
    for entry in entries {
        ensure(
            VALID_PATTERNS.contains(&entry.section.as_str()),
            format!(
                "041_TESTING_LEDGER.md:{}: invariant `{}` appears outside a valid harness section",
                entry.line, entry.title
            ),
        )?;
        for required in REQUIRED_FIELDS {
            ensure(
                entry.fields.contains(*required),
                format!(
                    "041_TESTING_LEDGER.md:{}: invariant `{}` missing `{required}`",
                    entry.line, entry.title
                ),
            )?;
        }
        let Some(pattern) = entry.pattern.as_deref() else {
            bail!(
                "041_TESTING_LEDGER.md:{}: invariant `{}` missing backticked harness pattern",
                entry.line,
                entry.title
            );
        };
        let Some(status) = entry.status.as_deref() else {
            bail!(
                "041_TESTING_LEDGER.md:{}: invariant `{}` missing `Status`",
                entry.line,
                entry.title
            );
        };
        ensure(
            VALID_STATUS.contains(&status),
            format!(
                "041_TESTING_LEDGER.md:{}: invariant `{}` Status `{status}` not in {{green,amber,red,unmeasured}}",
                entry.line, entry.title
            ),
        )?;
        ensure(
            pattern == entry.section,
            format!(
                "041_TESTING_LEDGER.md:{}: invariant `{}` pattern `{pattern}` must match section `{}`",
                entry.line, entry.title, entry.section
            ),
        )?;
        ensure(
            !entry.locations.is_empty(),
            format!(
                "041_TESTING_LEDGER.md:{}: invariant `{}` has no locations",
                entry.line, entry.title
            ),
        )?;
        ensure(
            !entry.commands.is_empty(),
            format!(
                "041_TESTING_LEDGER.md:{}: invariant `{}` has no commands",
                entry.line, entry.title
            ),
        )?;
        for path in &entry.locations {
            let workspace_path = workspace_relative_location(path);
            ensure(
                resolve_repo_or_core_path(repo_root, path).exists(),
                format!(
                    "041_TESTING_LEDGER.md:{}: location `{path}` does not exist",
                    entry.line
                ),
            )?;
            ensure(
                tracked.contains(workspace_path.as_str())
                    || tracked.contains(&format!("crates/core/{workspace_path}")),
                format!(
                    "041_TESTING_LEDGER.md:{}: location `{path}` is not git-tracked",
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
                    "041_TESTING_LEDGER.md:{}: command `{command}` must start with an approved repo command",
                    entry.line
                ),
            )?;
            check_cargo_test_filter_targets_existing_test(repo_root, entry, command, source_cache)?;
        }
    }
    Ok(())
}

fn check_cargo_test_filter_targets_existing_test(
    repo_root: &Path,
    entry: &LedgerEntry,
    command: &str,
    source_cache: &mut SourceCache,
) -> Result<()> {
    let Some((target, filter)) = cargo_test_target_and_filter(command) else {
        return Ok(());
    };
    let target_path = resolve_repo_or_core_path(repo_root, format!("tests/{target}.rs"));
    ensure(
        target_path.exists(),
        format!(
            "041_TESTING_LEDGER.md:{}: command `{command}` names missing integration test target `{target}`",
            entry.line
        ),
    )?;
    let tests = test_names_for_target(repo_root, target, source_cache)?;
    ensure(
        tests.iter().any(|name| name.contains(filter)),
        format!(
            "041_TESTING_LEDGER.md:{}: command `{command}` filter `{filter}` matches zero #[test] functions in tests/{target}.rs or tests/{target}/",
            entry.line
        ),
    )
}

fn cargo_test_target_and_filter(command: &str) -> Option<(&str, &str)> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    let target_pos = parts.iter().position(|part| *part == "--test")?;
    let target = *parts.get(target_pos + 1)?;
    let mut cursor = target_pos + 2;
    while let Some(part) = parts.get(cursor) {
        if *part == "--" {
            return None;
        }
        if flag_takes_value(part) {
            cursor += 2;
            continue;
        }
        if part.starts_with('-') || part.contains('=') {
            cursor += 1;
            continue;
        }
        return Some((target, part));
    }
    None
}

fn flag_takes_value(part: &str) -> bool {
    matches!(
        part,
        "--features"
            | "--profile"
            | "--package"
            | "-p"
            | "--manifest-path"
            | "--target-dir"
            | "--target"
    )
}

fn test_names_for_target(
    repo_root: &Path,
    target: &str,
    source_cache: &mut SourceCache,
) -> Result<BTreeSet<String>> {
    let target_path = resolve_repo_or_core_path(repo_root, format!("tests/{target}.rs"));
    let mut tests = test_names_from_file(&target_path, "", source_cache)?;
    let target_dir = resolve_repo_or_core_path(repo_root, format!("tests/{target}"));
    let mut nested = rust_files(&target_dir);
    nested.sort();
    for path in &nested {
        let module_prefix = nested_module_prefix(&target_dir, path);
        tests.extend(test_names_from_file(path, &module_prefix, source_cache)?);
    }
    Ok(tests)
}

fn nested_module_prefix(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let mut prefix_parts = relative
        .with_extension("")
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            std::path::Component::Prefix(_)
            | std::path::Component::RootDir
            | std::path::Component::CurDir
            | std::path::Component::ParentDir => None,
        })
        .collect::<Vec<_>>();
    if prefix_parts.last().is_some_and(|part| part == "mod") {
        prefix_parts.pop();
    }
    if prefix_parts.is_empty() {
        String::new()
    } else {
        format!("{}::", prefix_parts.join("::"))
    }
}

fn test_names_from_file(
    path: &Path,
    module_prefix: &str,
    source_cache: &mut SourceCache,
) -> Result<BTreeSet<String>> {
    let file = source_cache
        .parse_rust(path)
        .with_context(|| format!("parse {}", path.display()))?;
    Ok(test_function_names(&file.items, module_prefix))
}

fn test_function_names(items: &[syn::Item], module_prefix: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for item in items {
        if let syn::Item::Fn(function) = item {
            if has_test_attr(&function.attrs) {
                names.insert(format!("{module_prefix}{}", function.sig.ident));
            }
        } else if let syn::Item::Mod(module) = item {
            if let Some((_, nested)) = &module.content {
                let nested_prefix = format!("{module_prefix}{}::", module.ident);
                names.extend(test_function_names(nested, &nested_prefix));
            }
        }
    }
    names
}

fn has_test_attr(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| attr.path().is_ident("test"))
}

fn check_module_headers(
    repo_root: &Path,
    rust_files: &BTreeSet<String>,
    source_cache: &mut SourceCache,
) -> Result<()> {
    let allowlist = header_allowlist()?;
    for path in rust_files {
        let resolved = resolve_repo_or_core_path(repo_root, path);
        let content = source_cache
            .read_to_string(&resolved)
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

fn check_line_caps(
    repo_root: &Path,
    rust_files: &BTreeSet<String>,
    source_cache: &mut SourceCache,
) -> Result<()> {
    let allowlist = oversize_allowlist()?;
    for path in rust_files {
        let resolved = resolve_repo_or_core_path(repo_root, path);
        let content = source_cache
            .read_to_string(&resolved)
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
        .filter_map(|path| {
            let path = core_relative_location(path);
            (path.ends_with(".rs") && path.starts_with("tests/")).then_some(path)
        })
        .collect()
}

fn workspace_relative_location(path: &str) -> String {
    path.strip_prefix("bpk-lib/").unwrap_or(path).to_owned()
}

fn core_relative_location(path: &str) -> String {
    let path = path.strip_prefix("bpk-lib/").unwrap_or(path);
    path.strip_prefix("crates/core/").unwrap_or(path).to_owned()
}

trait DebtEntry: 'static {
    fn path(&self) -> &'static str;
    fn reason(&self) -> &'static str;
    fn target(&self) -> &'static str;
}

impl DebtEntry for HeaderDebt {
    fn path(&self) -> &'static str {
        self.path
    }
    fn reason(&self) -> &'static str {
        self.reason
    }
    fn target(&self) -> &'static str {
        self.target
    }
}

impl DebtEntry for OversizeDebt {
    fn path(&self) -> &'static str {
        self.path
    }
    fn reason(&self) -> &'static str {
        self.reason
    }
    fn target(&self) -> &'static str {
        self.target
    }
}

/// Validate a static debt allowlist: every entry must carry a non-empty reason
/// and target, and no path may appear twice. `noun` names the debt kind in
/// error messages (e.g. "header debt", "oversize debt").
fn validate_debt_allowlist<T: DebtEntry>(
    entries: &'static [T],
    noun: &str,
) -> Result<HashMap<&'static str, &'static T>> {
    let mut map = HashMap::new();
    for debt in entries {
        ensure(
            !debt.reason().is_empty(),
            format!("{noun} `{}` missing reason", debt.path()),
        )?;
        ensure(
            !debt.target().is_empty(),
            format!("{noun} `{}` missing target", debt.path()),
        )?;
        ensure(
            map.insert(debt.path(), debt).is_none(),
            format!("duplicate {noun} `{}`", debt.path()),
        )?;
    }
    Ok(map)
}

fn header_allowlist() -> Result<HashMap<&'static str, &'static HeaderDebt>> {
    validate_debt_allowlist(HEADER_DEBT_ALLOWLIST, "header debt")
}

fn oversize_allowlist() -> Result<HashMap<&'static str, &'static OversizeDebt>> {
    validate_debt_allowlist(OVERSIZE_HARNESS_ALLOWLIST, "oversize debt")
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

fn check_no_silent_repo_fixture_skips(
    repo_root: &Path,
    tracked_files: &[PathBuf],
    source_cache: &mut SourceCache,
) -> Result<()> {
    for path in tracked_files {
        let rel = relative(repo_root, path);
        if !rel.starts_with("crates/core/tests/") || !rel.ends_with(".rs") {
            continue;
        }
        let content = source_cache
            .read_to_string(path)
            .with_context(|| format!("read {rel}"))?;
        if content.contains(".cargo_vcs_info.json") && content.contains("return;") {
            bail!(
                "{rel}: packaged-source fixture tests must fail loudly when required fixtures are absent, not silently return under .cargo_vcs_info.json"
            );
        }
    }
    Ok(())
}

fn check_no_tombstone_ignores(
    repo_root: &Path,
    tracked_files: &[PathBuf],
    source_cache: &mut SourceCache,
) -> Result<()> {
    for path in tracked_files {
        let rel = relative(repo_root, path);
        if !rel.starts_with("crates/core/tests/") || !rel.ends_with(".rs") {
            continue;
        }
        let content = source_cache
            .read_to_string(path)
            .with_context(|| format!("read {rel}"))?;
        for (index, line) in content.lines().enumerate() {
            if line.contains("#[ignore = \"SUPERSEDED:") {
                bail!(
                    "{rel}:{}: superseded test tombstones must be deleted or replaced by an active proof",
                    index + 1
                );
            }
        }
    }
    Ok(())
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
            status: Some("green".to_owned()),
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
        let mut source_cache = SourceCache::new(&repo);

        check_entries(&repo, &tracked, &entries, &mut source_cache).expect("valid ledger entry");
        check_module_headers(&repo, &ledger_rust_files(&entries), &mut source_cache)
            .expect("valid header");
        check_line_caps(&repo, &ledger_rust_files(&entries), &mut source_cache)
            .expect("valid line cap");

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
        let mut source_cache = SourceCache::new(&repo);

        let err = check_entries(&repo, &tracked, &entries, &mut source_cache)
            .expect_err("missing field rejected");
        assert!(
            err.to_string().contains("missing `Mutation delta`"),
            "unexpected error: {err:?}"
        );

        fs::remove_dir_all(repo).expect("remove temp repo");
    }

    fn write_ledger(parent: &Path, body: &str) {
        let ledger_dir = parent.join("archive/legacy-docs");
        fs::create_dir_all(&ledger_dir).expect("create ledger dir");
        fs::write(ledger_dir.join("041_TESTING_LEDGER.md"), body).expect("write ledger");
    }

    #[test]
    fn parse_ledger_rejects_unknown_harness_section() {
        let parent = temp_repo("ledger-parent");
        write_ledger(
            &parent,
            r"## Not A Real Pattern
### Invariant: INV-BAD
",
        );
        let repo = parent.join("core");
        fs::create_dir_all(&repo).expect("create core repo");

        let result = parse_ledger(&repo);
        let err = match result {
            Ok(_) => panic!("expected unknown section rejection"),
            Err(error) => error,
        };
        assert!(
            err.to_string().contains("unknown harness section"),
            "unexpected error: {err:?}"
        );

        fs::remove_dir_all(parent).expect("remove temp parent");
    }

    #[test]
    fn parse_ledger_collects_locations_and_commands() {
        let parent = temp_repo("ledger-parse");
        write_ledger(
            &parent,
            r"## Property Harness
### Invariant: INV-PARSE
- Harness pattern: `Property Harness`
- Status: green
- Location:
  - `tests/synthetic.rs`
- Command used:
  - cargo test --test synthetic
- Line/function coverage delta: n/a
- Mutation delta: n/a
- Remaining known blind spots: n/a
",
        );
        let repo = parent.join("core");
        fs::create_dir_all(&repo).expect("create core repo");

        let entries = parse_ledger(&repo).expect("ledger parses");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "INV-PARSE");
        assert_eq!(entries[0].locations, vec!["tests/synthetic.rs"]);
        assert_eq!(entries[0].commands, vec!["cargo test --test synthetic"]);

        fs::remove_dir_all(parent).expect("remove temp parent");
    }

    #[test]
    fn check_entries_rejects_invalid_status_and_pattern_mismatch() {
        let repo = temp_repo("status");
        let location = "tests/synthetic.rs";
        fs::create_dir_all(repo.join("tests")).expect("create tests dir");
        fs::write(
            repo.join(location),
            "//! PROVES: synthetic proof.\n//! CATCHES: synthetic regression.\n//! SEEDED: deterministic.\n\
             #[test]\nfn synthetic_proof() {}\n",
        )
        .expect("write synthetic test");
        let tracked = BTreeSet::from([location.to_owned()]);
        let mut source_cache = SourceCache::new(&repo);

        let mut entry = complete_entry(location);
        entry.status = Some("purple".to_owned());
        let err = check_entries(&repo, &tracked, &[entry.clone()], &mut source_cache)
            .expect_err("invalid status rejected");
        assert!(err.to_string().contains("Status `purple`"), "{err:?}");

        entry.status = Some("green".to_owned());
        entry.pattern = Some("Oracle Harness".to_owned());
        entry.section = "Property Harness".to_owned();
        let err = check_entries(&repo, &tracked, &[entry], &mut source_cache)
            .expect_err("pattern mismatch rejected");
        assert!(err.to_string().contains("pattern"), "{err:?}");

        fs::remove_dir_all(repo).expect("remove temp repo");
    }

    #[test]
    fn check_entries_rejects_empty_locations_commands_and_bad_command_prefix() {
        let repo = temp_repo("empty-fields");
        let location = "tests/synthetic.rs";
        fs::create_dir_all(repo.join("tests")).expect("create tests dir");
        fs::write(
            repo.join(location),
            "//! PROVES: x\n//! CATCHES: y\n//! SEEDED: z\n",
        )
        .expect("write synthetic test");
        let tracked = BTreeSet::from([location.to_owned()]);
        let mut source_cache = SourceCache::new(&repo);
        let mut entry = complete_entry(location);

        entry.locations.clear();
        let err = check_entries(&repo, &tracked, &[entry.clone()], &mut source_cache)
            .expect_err("empty locations rejected");
        assert!(err.to_string().contains("no locations"), "{err:?}");

        entry.locations = vec![location.to_owned()];
        entry.commands.clear();
        let err = check_entries(&repo, &tracked, &[entry.clone()], &mut source_cache)
            .expect_err("empty commands rejected");
        assert!(err.to_string().contains("no commands"), "{err:?}");

        entry.commands = vec!["npm test".to_owned()];
        let err = check_entries(&repo, &tracked, &[entry], &mut source_cache)
            .expect_err("unapproved command rejected");
        assert!(err.to_string().contains("approved repo command"), "{err:?}");

        fs::remove_dir_all(repo).expect("remove temp repo");
    }

    #[test]
    fn check_entries_rejects_missing_location_and_bad_cargo_test_filter() {
        let repo = temp_repo("location-filter");
        let location = "tests/synthetic.rs";
        fs::create_dir_all(repo.join("tests")).expect("create tests dir");
        fs::write(
            repo.join(location),
            "//! PROVES: synthetic proof.\n//! CATCHES: synthetic regression.\n//! SEEDED: deterministic.\n\
             #[test]\nfn synthetic_proof() {}\n",
        )
        .expect("write synthetic test");
        let tracked = BTreeSet::from([location.to_owned()]);
        let mut source_cache = SourceCache::new(&repo);
        let mut entry = complete_entry(location);

        entry.locations = vec!["tests/missing.rs".to_owned()];
        let err = check_entries(&repo, &tracked, &[entry.clone()], &mut source_cache)
            .expect_err("missing location rejected");
        assert!(err.to_string().contains("does not exist"), "{err:?}");

        entry.locations = vec![location.to_owned()];
        entry.commands = vec!["cargo test --test synthetic no_such_filter".to_owned()];
        let err = check_entries(&repo, &tracked, &[entry], &mut source_cache)
            .expect_err("bad filter rejected");
        assert!(err.to_string().contains("matches zero"), "{err:?}");

        fs::remove_dir_all(repo).expect("remove temp repo");
    }

    #[test]
    fn cargo_test_target_and_filter_parsing_handles_flags_and_filters() {
        assert_eq!(
            cargo_test_target_and_filter("cargo test --test synthetic synthetic_proof"),
            Some(("synthetic", "synthetic_proof"))
        );
        assert_eq!(
            cargo_test_target_and_filter(
                "cargo test --test synthetic --features native -- synthetic_proof"
            ),
            None
        );
        assert_eq!(
            cargo_test_target_and_filter("cargo test --test synthetic --package batpak proof"),
            Some(("synthetic", "proof"))
        );
        assert!(flag_takes_value("--features"));
        assert!(!flag_takes_value("--locked"));
    }

    #[test]
    fn helper_parsers_extract_field_names_and_list_items() {
        assert_eq!(
            field_name("- Harness pattern: `Property Harness`"),
            Some("Harness pattern")
        );
        assert_eq!(
            backtick_value("- Location: `tests/synthetic.rs`"),
            Some("tests/synthetic.rs")
        );
        assert_eq!(
            list_item("- cargo test --test synthetic"),
            Some("cargo test --test synthetic")
        );
        assert_eq!(
            workspace_relative_location("bpk-lib/crates/core/tests/x.rs"),
            "crates/core/tests/x.rs"
        );
        assert_eq!(
            core_relative_location("bpk-lib/crates/core/tests/x.rs"),
            "tests/x.rs"
        );
    }

    #[test]
    fn check_module_headers_requires_canonical_fields_or_allowlist() {
        let repo = temp_repo("headers");
        let path = "tests/synthetic.rs";
        fs::create_dir_all(repo.join("tests")).expect("create tests dir");
        fs::write(repo.join(path), "fn main() {}\n").expect("write headerless test");
        let files = BTreeSet::from([path.to_owned()]);
        let mut source_cache = SourceCache::new(&repo);

        let err = check_module_headers(&repo, &files, &mut source_cache)
            .expect_err("headerless harness rejected");
        assert!(err.to_string().contains("PROVES/CATCHES/SEEDED"), "{err:?}");

        fs::write(
            repo.join(path),
            "//! PROVES: synthetic proof.\n//! CATCHES: synthetic regression.\n//! SEEDED: deterministic.\n",
        )
        .expect("write complete header");
        let mut fresh_cache = SourceCache::new(&repo);
        check_module_headers(&repo, &files, &mut fresh_cache).expect("complete header accepted");

        fs::remove_dir_all(repo).expect("remove temp repo");
    }

    #[test]
    fn check_line_caps_rejects_unlisted_oversize_harness() {
        let repo = temp_repo("line-cap");
        let path = "tests/synthetic.rs";
        fs::create_dir_all(repo.join("tests")).expect("create tests dir");
        let body = (0..501)
            .map(|line| format!("// line {line}\n"))
            .collect::<String>();
        fs::write(repo.join(path), body).expect("write oversize harness");
        let files = BTreeSet::from([path.to_owned()]);
        let mut source_cache = SourceCache::new(&repo);

        let err = check_line_caps(&repo, &files, &mut source_cache).expect_err("oversize rejected");
        assert!(err.to_string().contains("501 lines"), "{err:?}");

        fs::remove_dir_all(repo).expect("remove temp repo");
    }

    #[test]
    fn check_no_silent_repo_fixture_skips_and_tombstone_ignores() {
        let repo = temp_repo("fixture-tombstone");
        let fixture_path = repo.join("crates/core/tests/fixture.rs");
        let tombstone_path = repo.join("crates/core/tests/tombstone.rs");
        fs::create_dir_all(fixture_path.parent().unwrap()).expect("create tests dir");
        fs::write(
            &fixture_path,
            "fn packaged_fixture() {\n    if std::path::Path::new(\".cargo_vcs_info.json\").exists() {\n        return;\n    }\n}\n",
        )
        .expect("write fixture test");
        fs::write(
            &tombstone_path,
            "#[ignore = \"SUPERSEDED: old test\"]\nfn dead() {}\n",
        )
        .expect("write tombstone test");
        let tracked = vec![fixture_path, tombstone_path];
        let mut source_cache = SourceCache::new(&repo);

        let err = check_no_silent_repo_fixture_skips(&repo, &tracked, &mut source_cache)
            .expect_err("silent fixture skip rejected");
        assert!(err.to_string().contains(".cargo_vcs_info.json"), "{err:?}");

        let err = check_no_tombstone_ignores(&repo, &tracked, &mut source_cache)
            .expect_err("tombstone ignore rejected");
        assert!(err.to_string().contains("superseded"), "{err:?}");

        fs::remove_dir_all(repo).expect("remove temp repo");
    }

    #[test]
    fn check_module_headers_rejects_partial_canonical_header() {
        let repo = temp_repo("partial-header");
        let path = "tests/synthetic.rs";
        fs::create_dir_all(repo.join("tests")).expect("create tests dir");
        fs::write(repo.join(path), "//! PROVES: only proves field.\n")
            .expect("write partial header");
        let files = BTreeSet::from([path.to_owned()]);
        let mut source_cache = SourceCache::new(&repo);

        let err = check_module_headers(&repo, &files, &mut source_cache)
            .expect_err("partial header rejected");
        assert!(err.to_string().contains("PROVES/CATCHES/SEEDED"), "{err:?}");

        fs::remove_dir_all(repo).expect("remove temp repo");
    }

    #[test]
    fn check_entries_rejects_untracked_location_and_missing_pattern() {
        let repo = temp_repo("untracked");
        let location = "tests/synthetic.rs";
        fs::create_dir_all(repo.join("tests")).expect("create tests dir");
        fs::write(
            repo.join(location),
            "//! PROVES: synthetic proof.\n//! CATCHES: synthetic regression.\n//! SEEDED: deterministic.\n",
        )
        .expect("write synthetic test");
        let tracked = BTreeSet::<String>::new();
        let mut source_cache = SourceCache::new(&repo);
        let mut entry = complete_entry(location);

        let err = check_entries(&repo, &tracked, &[entry.clone()], &mut source_cache)
            .expect_err("untracked location rejected");
        assert!(err.to_string().contains("not git-tracked"), "{err:?}");

        entry.pattern = None;
        let tracked = BTreeSet::from([location.to_owned()]);
        let err = check_entries(&repo, &tracked, &[entry], &mut source_cache)
            .expect_err("missing pattern rejected");
        assert!(
            err.to_string()
                .contains("missing backticked harness pattern"),
            "{err:?}"
        );

        fs::remove_dir_all(repo).expect("remove temp repo");
    }

    #[test]
    fn test_names_for_target_collects_nested_module_tests() {
        let repo = temp_repo("nested-tests");
        let target = repo.join("tests/nested.rs");
        fs::create_dir_all(target.parent().unwrap()).expect("create tests dir");
        fs::write(
            target,
            "#[cfg(test)]\nmod cases {\n    #[test]\n    fn nested_proof() {}\n}\n",
        )
        .expect("write nested module");
        let mut source_cache = SourceCache::new(&repo);

        let names =
            test_names_for_target(&repo, "nested", &mut source_cache).expect("collect names");
        assert!(
            names.iter().any(|name| name.contains("nested_proof")),
            "{names:?}"
        );

        fs::remove_dir_all(repo).expect("remove temp repo");
    }

    #[test]
    fn check_cargo_test_filter_rejects_missing_integration_target() {
        let repo = temp_repo("missing-target");
        let mut entry = complete_entry("tests/synthetic.rs");
        entry.commands = vec!["cargo test --test missing_target filter".to_owned()];
        let mut source_cache = SourceCache::new(&repo);

        let err = check_cargo_test_filter_targets_existing_test(
            &repo,
            &entry,
            &entry.commands[0],
            &mut source_cache,
        )
        .expect_err("missing integration target rejected");
        assert!(
            err.to_string().contains("missing integration test target"),
            "{err:?}"
        );

        fs::remove_dir_all(repo).expect("remove temp repo");
    }
}
