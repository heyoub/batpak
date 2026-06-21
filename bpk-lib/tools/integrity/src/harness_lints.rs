use crate::repo_surface::{load_yaml, resolve_repo_or_core_path, rust_files};
use crate::source_cache::SourceCache;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};
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
        target: "normalize scenario headers during harness cleanup",
    },
    HeaderDebt {
        path: "tests/chaos/scenarios/single_append_written.rs",
        reason: "chaos scenario has prose header but not canonical fields",
        target: "normalize scenario headers during harness cleanup",
    },
    HeaderDebt {
        path: "tests/chaos/scenarios/smoke.rs",
        reason: "minimal chaos smoke predates module-header doctrine",
        target: "add header when smoke scenario changes",
    },
    HeaderDebt {
        path: "tests/cold_start_recovery.rs",
        reason: "legacy cold-start recovery suite predates module-header doctrine",
        target: "add header when recovery matrix changes",
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
        path: "tests/index_filter_composition.rs",
        reason: "oracle suite predates module-header doctrine",
        target: "add header when query oracle changes",
    },
    HeaderDebt {
        path: "tests/replay_consistency.rs",
        reason: "replay parity suite predates module-header doctrine",
        target: "add header when replay matrix changes",
    },
    HeaderDebt {
        path: "tests/store_advanced.rs",
        reason: "legacy omnibus suite has partial header only",
        target: "split by seam during harness cleanup",
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

/// Raw record shape for `traceability/testing_ledger.yaml`. Each entry is one
/// doctrine-bearing harness suite. Optional scalars stay `Option` so the schema
/// lints below can name a specific missing field instead of failing at parse.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LedgerRecord {
    title: String,
    section: String,
    pattern: Option<String>,
    status: Option<String>,
    #[serde(default)]
    invariants: Vec<String>,
    #[serde(default)]
    locations: Vec<String>,
    #[serde(default)]
    commands: Vec<String>,
    coverage_delta: Option<String>,
    mutation_delta: Option<String>,
    blind_spots: Option<String>,
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

pub(crate) fn ledger_path(repo_root: &Path) -> PathBuf {
    repo_root.join("traceability/testing_ledger.yaml")
}

/// One ledger entry's citation surface, shared with the invariant bridge and the
/// architecture-ir projection so the ledger has a single deserialization home.
pub(crate) struct LedgerCitations {
    pub title: String,
    /// 1-based record position, used to anchor diagnostics and waiver names.
    pub line: usize,
    pub invariants: Vec<String>,
}

/// Read the testing ledger and project each entry's title and cited catalog
/// invariants. Other integrity gates use this instead of re-parsing the file.
pub(crate) fn load_ledger_citations(repo_root: &Path) -> Result<Vec<LedgerCitations>> {
    let path = ledger_path(repo_root);
    let records: Vec<LedgerRecord> = load_yaml(&path).context("read testing_ledger.yaml")?;
    Ok(records
        .into_iter()
        .enumerate()
        .map(|(index, record)| LedgerCitations {
            title: record.title,
            line: index + 1,
            invariants: record.invariants,
        })
        .collect())
}

fn parse_ledger(repo_root: &Path) -> Result<Vec<LedgerEntry>> {
    let path = ledger_path(repo_root);
    let records: Vec<LedgerRecord> = load_yaml(&path).context("read testing_ledger.yaml")?;
    let mut entries = Vec::with_capacity(records.len());
    for (index, record) in records.into_iter().enumerate() {
        // 1-based record position so diagnostics point at a real ledger entry
        // even though the source is now a flat YAML sequence.
        let line = index + 1;
        ensure(
            VALID_PATTERNS.contains(&record.section.as_str()),
            format!(
                "testing_ledger.yaml:{line}: unknown harness section `{}`",
                record.section
            ),
        )?;
        entries.push(into_entry(record, line));
    }
    ensure(
        !entries.is_empty(),
        "testing_ledger.yaml has no invariant entries",
    )?;
    Ok(entries)
}

/// Translate a deserialized ledger record into the `LedgerEntry` the schema
/// lints consume. `fields` records which required fields were actually present
/// so `check_entries` can name a specific missing field.
fn into_entry(record: LedgerRecord, line: usize) -> LedgerEntry {
    let mut fields = BTreeSet::new();
    if record.pattern.is_some() {
        fields.insert("Harness pattern".to_owned());
    }
    if record.status.is_some() {
        fields.insert("Status".to_owned());
    }
    if !record.locations.is_empty() {
        fields.insert("Location".to_owned());
    }
    if !record.commands.is_empty() {
        fields.insert("Command used".to_owned());
    }
    if record.coverage_delta.is_some() {
        fields.insert("Line/function coverage delta".to_owned());
    }
    if record.mutation_delta.is_some() {
        fields.insert("Mutation delta".to_owned());
    }
    if record.blind_spots.is_some() {
        fields.insert("Remaining known blind spots".to_owned());
    }
    LedgerEntry {
        title: record.title,
        section: record.section,
        line,
        pattern: record.pattern,
        status: record.status,
        fields,
        locations: record.locations,
        commands: record.commands,
    }
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
                "testing_ledger.yaml:{}: invariant `{}` appears outside a valid harness section",
                entry.line, entry.title
            ),
        )?;
        for required in REQUIRED_FIELDS {
            ensure(
                entry.fields.contains(*required),
                format!(
                    "testing_ledger.yaml:{}: invariant `{}` missing `{required}`",
                    entry.line, entry.title
                ),
            )?;
        }
        let Some(pattern) = entry.pattern.as_deref() else {
            bail!(
                "testing_ledger.yaml:{}: invariant `{}` missing `Harness pattern`",
                entry.line,
                entry.title
            );
        };
        let Some(status) = entry.status.as_deref() else {
            bail!(
                "testing_ledger.yaml:{}: invariant `{}` missing `Status`",
                entry.line,
                entry.title
            );
        };
        ensure(
            VALID_STATUS.contains(&status),
            format!(
                "testing_ledger.yaml:{}: invariant `{}` Status `{status}` not in {{green,amber,red,unmeasured}}",
                entry.line, entry.title
            ),
        )?;
        ensure(
            pattern == entry.section,
            format!(
                "testing_ledger.yaml:{}: invariant `{}` pattern `{pattern}` must match section `{}`",
                entry.line, entry.title, entry.section
            ),
        )?;
        ensure(
            !entry.locations.is_empty(),
            format!(
                "testing_ledger.yaml:{}: invariant `{}` has no locations",
                entry.line, entry.title
            ),
        )?;
        ensure(
            !entry.commands.is_empty(),
            format!(
                "testing_ledger.yaml:{}: invariant `{}` has no commands",
                entry.line, entry.title
            ),
        )?;
        for path in &entry.locations {
            let workspace_path = workspace_relative_location(path);
            ensure(
                resolve_repo_or_core_path(repo_root, path).exists(),
                format!(
                    "testing_ledger.yaml:{}: location `{path}` does not exist",
                    entry.line
                ),
            )?;
            ensure(
                tracked.contains(workspace_path.as_str())
                    || tracked.contains(&format!("crates/core/{workspace_path}")),
                format!(
                    "testing_ledger.yaml:{}: location `{path}` is not git-tracked",
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
                    "testing_ledger.yaml:{}: command `{command}` must start with an approved repo command",
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
            "testing_ledger.yaml:{}: command `{command}` names missing integration test target `{target}`",
            entry.line
        ),
    )?;
    let tests = test_names_for_target(repo_root, target, source_cache)?;
    ensure(
        tests.iter().any(|name| name.contains(filter)),
        format!(
            "testing_ledger.yaml:{}: command `{command}` filter `{filter}` matches zero #[test] functions in tests/{target}.rs or tests/{target}/",
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

/// Absolute, non-overridable line cap for a doctrine-bearing harness file. A
/// harness over this cap must be split, never bumped: there is no per-file
/// oversize allowlist anymore ("split, don't bump").
const HARNESS_LINE_CAP: usize = 500;

fn check_line_caps(
    repo_root: &Path,
    rust_files: &BTreeSet<String>,
    source_cache: &mut SourceCache,
) -> Result<()> {
    for path in rust_files {
        let resolved = resolve_repo_or_core_path(repo_root, path);
        let content = source_cache
            .read_to_string(&resolved)
            .with_context(|| format!("read {path}"))?;
        let line_count = content.lines().count();
        ensure(
            line_count <= HARNESS_LINE_CAP,
            format!(
                "doctrine-bearing harness `{path}` has {line_count} lines, exceeding the absolute cap {HARNESS_LINE_CAP}; split it. The cap is non-overridable — there is no oversize harness allowlist anymore."
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
    use std::fs;
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

    fn write_ledger(repo: &Path, body: &str) {
        let ledger = ledger_path(repo);
        fs::create_dir_all(ledger.parent().expect("ledger parent")).expect("create ledger dir");
        fs::write(ledger, body).expect("write ledger");
    }

    #[test]
    fn parse_ledger_rejects_unknown_harness_section() {
        let repo = temp_repo("ledger-parent");
        write_ledger(
            &repo,
            r"- title: INV-BAD
  section: Not A Real Pattern
",
        );

        let err = parse_ledger(&repo)
            .err()
            .expect("expected unknown section rejection");
        assert!(
            err.to_string().contains("unknown harness section"),
            "unexpected error: {err:?}"
        );

        fs::remove_dir_all(repo).expect("remove temp repo");
    }

    #[test]
    fn parse_ledger_collects_locations_and_commands() {
        let repo = temp_repo("ledger-parse");
        write_ledger(
            &repo,
            r"- title: INV-PARSE
  section: Property Harness
  pattern: Property Harness
  status: green
  invariants:
    - INV-PARSE
  locations:
    - tests/synthetic.rs
  commands:
    - cargo test --test synthetic
  coverage_delta: n/a
  mutation_delta: n/a
  blind_spots: n/a
",
        );

        let entries = parse_ledger(&repo).expect("ledger parses");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "INV-PARSE");
        // The lone record sits at 1-based position 1 so diagnostics point at the
        // real ledger entry.
        assert_eq!(entries[0].line, 1);
        assert_eq!(entries[0].locations, vec!["tests/synthetic.rs"]);
        assert_eq!(entries[0].commands, vec!["cargo test --test synthetic"]);

        fs::remove_dir_all(repo).expect("remove temp repo");
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
        // A leading env assignment shifts `--test` off index 2 so the cursor must
        // be `target_pos + 2` (not `* 2`); at target_pos=3 those differ (5 vs 6)
        // and the wrong arithmetic would walk past the filter and return None.
        assert_eq!(
            cargo_test_target_and_filter("CARGO_INCREMENTAL=0 cargo test --test synthetic proof"),
            Some(("synthetic", "proof"))
        );
        assert!(flag_takes_value("--features"));
        assert!(!flag_takes_value("--locked"));
    }

    #[test]
    fn location_path_normalizers_strip_workspace_and_core_prefixes() {
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
        fs::create_dir_all(fixture_path.parent().expect("fixture path has parent"))
            .expect("create tests dir");
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
            err.to_string().contains("missing `Harness pattern`"),
            "{err:?}"
        );

        fs::remove_dir_all(repo).expect("remove temp repo");
    }

    #[test]
    fn test_names_for_target_collects_nested_module_tests() {
        let repo = temp_repo("nested-tests");
        let target = repo.join("tests/nested.rs");
        fs::create_dir_all(target.parent().expect("target path has parent"))
            .expect("create tests dir");
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

    #[test]
    fn check_propagates_ledger_validation_failure() {
        // Pins the top-level `check` orchestrator: a malformed ledger must make
        // `check` itself return Err, not just the inner helpers. Guards against
        // the whole body being short-circuited to Ok(()).
        let repo = temp_repo("check-e2e");
        write_ledger(
            &repo,
            r"- title: INV-BAD
  section: Not A Real Pattern
",
        );
        let mut source_cache = SourceCache::new(&repo);

        let err = check(&repo, &[], &mut source_cache)
            .expect_err("check must propagate ledger validation failure");
        assert!(
            err.to_string().contains("unknown harness section"),
            "{err:?}"
        );

        fs::remove_dir_all(repo).expect("remove temp repo");
    }

    #[test]
    fn check_module_headers_rejects_header_missing_only_seeded() {
        // PROVES + CATCHES present but SEEDED absent must still be rejected.
        // Pins the second `&&` in the completeness conjunction: turning it into
        // `||` would wrongly treat this header as complete.
        let repo = temp_repo("missing-seeded");
        let path = "tests/synthetic.rs";
        fs::create_dir_all(repo.join("tests")).expect("create tests dir");
        fs::write(
            repo.join(path),
            "//! PROVES: synthetic proof.\n//! CATCHES: synthetic regression.\n",
        )
        .expect("write header missing SEEDED");
        let files = BTreeSet::from([path.to_owned()]);
        let mut source_cache = SourceCache::new(&repo);

        let err = check_module_headers(&repo, &files, &mut source_cache)
            .expect_err("header missing SEEDED rejected");
        assert!(err.to_string().contains("PROVES/CATCHES/SEEDED"), "{err:?}");

        fs::remove_dir_all(repo).expect("remove temp repo");
    }

    #[test]
    fn check_no_silent_repo_fixture_skips_allows_plain_return() {
        // A core test that uses `return;` but never references the packaged
        // fixture marker is fine. Pins the `&&`: an `||` would falsely flag
        // every test containing a bare `return;`.
        let repo = temp_repo("plain-return");
        let path = repo.join("crates/core/tests/plain.rs");
        fs::create_dir_all(path.parent().expect("path has parent")).expect("create tests dir");
        fs::write(&path, "fn helper() {\n    return;\n}\n").expect("write plain return test");
        let tracked = vec![path];
        let mut source_cache = SourceCache::new(&repo);

        check_no_silent_repo_fixture_skips(&repo, &tracked, &mut source_cache)
            .expect("plain return without fixture marker is allowed");

        fs::remove_dir_all(repo).expect("remove temp repo");
    }

    #[test]
    fn header_allowlist_validates_and_exposes_every_entry() {
        // Pins `header_allowlist`: collapsing its body to an empty map would
        // silently disarm the allowlist (stale entries undetected, debt-bearing
        // harnesses forced to fail). Every static entry must round-trip.
        let allowlist = header_allowlist().expect("header allowlist validates");
        assert_eq!(allowlist.len(), HEADER_DEBT_ALLOWLIST.len());
        assert!(!allowlist.is_empty());
        for debt in HEADER_DEBT_ALLOWLIST {
            assert!(allowlist.contains_key(debt.path), "missing {}", debt.path);
        }
    }

    #[test]
    fn check_line_caps_is_non_overridable_at_the_absolute_cap() {
        // RED-equivalent: a harness one line over the cap is rejected, and there
        // is no allowlist path to exempt it anymore ("split, don't bump").
        let repo = temp_repo("line-cap-absolute");
        let path = "tests/over_cap.rs";
        fs::create_dir_all(repo.join("tests")).expect("create tests dir");
        let over = (0..super::HARNESS_LINE_CAP + 1)
            .map(|line| format!("// line {line}\n"))
            .collect::<String>();
        fs::write(repo.join(path), over).expect("write oversize harness");
        let files = BTreeSet::from([path.to_owned()]);
        let mut source_cache = SourceCache::new(&repo);
        let err = check_line_caps(&repo, &files, &mut source_cache).expect_err("over-cap rejected");
        assert!(err.to_string().contains("non-overridable"), "{err:?}");

        // GREEN: a harness at exactly the cap passes.
        let at_cap = (0..super::HARNESS_LINE_CAP)
            .map(|line| format!("// line {line}\n"))
            .collect::<String>();
        fs::write(repo.join(path), at_cap).expect("write at-cap harness");
        let mut fresh_cache = SourceCache::new(&repo);
        check_line_caps(&repo, &files, &mut fresh_cache).expect("at-cap harness accepted");

        fs::remove_dir_all(repo).expect("remove temp repo");
    }
}
