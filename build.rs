#![allow(clippy::panic)]

use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use syn::visit::Visit;

#[derive(Debug, Deserialize)]
struct PubItemAllowlistEntry {
    name: String,
    justification: String,
}

// build.rs runs before every cargo build/check/test. Cannot be skipped.
// It enforces live runtime invariants at build time so agents get English
// errors instead of cryptic compiler failures. See README.md, GUIDE.md, and
// REFERENCE.md for the current truth hierarchy.
fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=src/");
    println!("cargo:rerun-if-changed=traceability/pub_item_allowlist.yaml");

    check_no_tokio_in_deps();
    check_no_banned_patterns();
    check_store_config_field_usage();
    check_allow_justifications();
    check_no_stubs_in_src();
    check_store_surface_honesty();
    check_no_fixed_temp_patterns();
    check_pub_items_have_tests();
}

/// Audit Loop Layer 2 enforcement: no stub markers in production src/.
/// todo!() and unimplemented!() are already denied by clippy, but this
/// catches patterns clippy misses: hardcoded placeholder strings, empty
/// function bodies returning defaults, etc.
fn check_no_stubs_in_src() {
    let stub_patterns = [
        (
            "\"placeholder\"",
            "Placeholder string literal — replace with real implementation",
        ),
        (
            "\"not implemented\"",
            "Stub string — implement the real behavior or return a typed error",
        ),
        (
            "\"not yet implemented\"",
            "Stub string — implement the real behavior",
        ),
    ];

    walk_rs_files(Path::new("src"), &mut |path, contents| {
        let path_str = path.display().to_string();
        for (line_no, line) in contents.lines().enumerate() {
            let lower = line.to_lowercase();
            for (pattern, msg) in &stub_patterns {
                if lower.contains(pattern) {
                    panic!(
                        "STUB DETECTED in {path_str}:{}: {msg}\n\
                         Line: {line}\n\
                         LAW-001: No fake success responses. FM-009: No polite downgrades.",
                        line_no + 1
                    );
                }
            }
        }
    });
}

/// FM-002 Rogue Silence defense: every #[allow(...)] in src/ must have a
/// justification comment on the same or previous line explaining why.
/// Unjustified allows are how agents silence the compiler instead of fixing bugs.
fn check_allow_justifications() {
    walk_rs_files(Path::new("src"), &mut |path, contents| {
        let path_str = path.display().to_string();
        for (line_no, line) in contents.lines().enumerate() {
            let trimmed = line.trim();
            // Skip the crate-level allow at the top of lib.rs
            if trimmed.starts_with("#![allow") {
                continue;
            }
            if trimmed.starts_with("#[allow(") {
                // Check this line and previous line for a justification comment
                let has_justification = trimmed.contains("//")
                    || (line_no > 0
                        && contents
                            .lines()
                            .nth(line_no - 1)
                            .map(|prev| prev.trim().starts_with("//"))
                            .unwrap_or(false));
                if !has_justification {
                    panic!(
                        "ROGUE SILENCE in {path_str}:{}: `{trimmed}`\n\
                         Every #[allow(...)] must have a justification comment on the same\n\
                         or previous line explaining WHY the lint is suppressed.\n\
                         Example: #[allow(clippy::cast_possible_truncation)] // frame_size < u32::MAX\n\
                         See: Big Bang FM-002 (Rogue Silence).",
                        line_no + 1
                    );
                }
            }
        }
    });
}

fn check_no_tokio_in_deps() {
    //Invariant 1: tokio must not appear in [dependencies].
    //Only [dev-dependencies] is allowed.
    let cargo = fs::read_to_string("Cargo.toml").expect("read Cargo.toml");

    //Strategy: find the [dependencies] section, take text until the next
    //section header (line starting with [), check for "tokio".
    //This is deliberately simple string matching — no toml parser dep.
    if let Some(deps_section) = cargo.split("[dependencies]").nth(1) {
        let deps_only = deps_section.split("\n[").next().unwrap_or("");
        if deps_only.contains("tokio") {
            panic!(
                "INVARIANT 1 VIOLATED: tokio found in [dependencies].\n\
                 tokio belongs in [dev-dependencies] only.\n\
                 The library is runtime-agnostic. Fan-out uses Vec<flume::Sender>.\n\
                 See: REFERENCE.md."
            );
        }
    }
}

fn check_no_banned_patterns() {
    //Walk src/**/*.rs, read each file, check for patterns that violate
    //invariants or red flags.
    walk_rs_files(Path::new("src"), &mut |path, contents| {
        let path_str = path.display().to_string();

        //Red flag: no transmute/mem::read/pointer_cast in any src file.
        //All serialization goes through MessagePack.
        for banned in ["transmute", "mem::read", "pointer_cast"] {
            if contents.contains(banned) {
                panic!(
                    "RED FLAG VIOLATED in {path_str}: found `{banned}`.\n\
                     repr(C) is for field ordering, not a wire format.\n\
                     All serialization goes through rmp-serde. Always.\n\
                     See: REFERENCE.md."
                );
            }
        }

        //Invariant 2: no async fn in store module.
        //Store API is sync. Async lives in flume channels.
        if path_str.contains("store") && contents.contains("async fn") {
            panic!(
                "INVARIANT 2 VIOLATED in {path_str}: found `async fn`.\n\
                 Store API is sync. Async callers use spawn_blocking()\n\
                 or flume's recv_async(). See: store/subscription.rs.\n\
                 See: REFERENCE.md."
            );
        }

        // Post-mortem Bug 7: std::thread::spawn() panics on failure.
        // All thread creation must use Builder::new().spawn() for fallible error handling.
        if contents.contains("std::thread::spawn(") {
            panic!(
                "BANNED PATTERN in {path_str}: `std::thread::spawn()` found.\n\
                 Use `std::thread::Builder::new().name(...).spawn()` instead.\n\
                 `thread::spawn` panics on failure; `Builder::spawn` returns Result.\n\
                 See: Bug 7 post-mortem (react_loop panic)."
            );
        }

        // Post-mortem Bug 9: bare .sync() bypasses sync_mode config.
        // In store/ files, require .sync_with_mode() — never bare .sync().
        // The only exception is segment.rs which defines the .sync() method itself.
        if path_str.contains("store") && !path_str.ends_with("segment.rs") {
            for (line_no, line) in contents.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.starts_with("//") || trimmed.starts_with("///") {
                    continue;
                }
                // Match .sync() but not .sync_with_mode() and not self.sync() (Store::sync)
                if trimmed.contains(".sync()")
                    && !trimmed.contains("sync_with_mode")
                    && !trimmed.contains("self.sync()")
                    && !trimmed.contains("force_sync()")
                {
                    panic!(
                        "BANNED PATTERN in {path_str}:{}: bare `.sync()` call.\n\
                         Use `.sync_with_mode(&config.sync.mode)` instead.\n\
                         Bare .sync() hardcodes SyncAll, ignoring the user's config.\n\
                         See: Bug 9 post-mortem (segment rotation bypassed sync.mode).\n\
                         Line: {trimmed}",
                        line_no + 1
                    );
                }
            }
        }

        //Invariant 3: no product concepts in library code.
        //Check struct/enum/fn/type declarations for banned nouns.
        //Skip string literals and comments.
        let banned_nouns = ["trajectory", "artifact", "tenant"];
        //NOTE: "scope" and "agent" are common English words.
        //"turn" and "note" are substrings of "return" and "annotation" —
        //substring matching would false-positive on legitimate Rust code.
        //Only check nouns that are unambiguous product concepts.
        //Strategy: check lines starting with pub/fn/struct/enum/type
        //for WORD-BOUNDARY matches of banned nouns.
        for line in contents.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") || trimmed.starts_with("///") {
                continue; // skip comments
            }
            let is_decl = trimmed.starts_with("pub ")
                || trimmed.starts_with("fn ")
                || trimmed.starts_with("struct ")
                || trimmed.starts_with("enum ")
                || trimmed.starts_with("type ");
            if is_decl {
                let lower = trimmed.to_lowercase();
                for noun in &banned_nouns {
                    //Word boundary check: noun must be preceded by start/underscore/space
                    //and followed by end/underscore/space/(/>. Prevents "return" matching "turn".
                    let has_match =
                        lower
                            .split(|c: char| !c.is_alphanumeric() && c != '_')
                            .any(|word| {
                                word == *noun
                                    || word.starts_with(&format!("{noun}_"))
                                    || word.ends_with(&format!("_{noun}"))
                                    || word.contains(&format!("_{noun}_"))
                            });
                    if has_match {
                        panic!(
                            "INVARIANT 3 VIOLATED in {path_str}: \
                             product concept `{noun}` in declaration:\n  {trimmed}\n\
                             Library vocabulary: coordinate, entity, event, outcome, \
                             gate, region, transition.\n\
                             See: REFERENCE.md."
                        );
                    }
                }
            }
        }
    });
}

fn check_store_surface_honesty() {
    let store_mod =
        fs::read_to_string("src/store/mod.rs").expect("read src/store/mod.rs for surface check");
    if store_mod.contains("pub fn subscribe(") {
        panic!(
            "PUBLIC API HONESTY VIOLATION: src/store/mod.rs still exports `pub fn subscribe(`.\n\
             The lossy broadcast API must be named `subscribe_lossy` so callers cannot\n\
             confuse it with guaranteed delivery."
        );
    }
    if store_mod.contains("pub fn cursor(") {
        panic!(
            "PUBLIC API HONESTY VIOLATION: src/store/mod.rs still exports `pub fn cursor(`.\n\
             The guaranteed replay API must be named `cursor_guaranteed`."
        );
    }
    if store_mod.contains("Freshness::BestEffort") || store_mod.contains("BestEffort") {
        panic!(
            "PUBLIC API HONESTY VIOLATION: stale `Freshness::BestEffort` reference in src/store/mod.rs.\n\
             Use `Freshness::MaybeStale {{ max_stale_ms }}`."
        );
    }

    walk_rs_files(Path::new("src/store"), &mut |path, contents| {
        let path_str = path.display().to_string();
        if contents.contains("test-support") {
            panic!(
                "FEATURE HONESTY VIOLATION in {path_str}: stale `test-support` reference.\n\
                 The explicit risk-bearing feature name is `dangerous-test-hooks`."
            );
        }
    });
}

fn check_no_fixed_temp_patterns() {
    walk_rs_files(Path::new("src/store"), &mut |path, contents| {
        let path_str = path.display().to_string();
        if contents.contains("index.ckpt.tmp") || contents.contains(".tmp_{pid}_{n}") {
            panic!(
                "TEMP FILE HARDENING VIOLATION in {path_str}: fixed temp-file pattern found.\n\
                 Use same-directory `tempfile::NamedTempFile` instead of predictable names."
            );
        }
        if contents.contains("create(true)") && contents.contains("truncate(true)") {
            panic!(
                "TEMP FILE HARDENING VIOLATION in {path_str}: `create(true)` + `truncate(true)` found.\n\
                 This is the symlink-clobber shape the release hardening pass bans in src/store."
            );
        }
    });
}

fn check_store_config_field_usage() {
    // Invariant: every pub field in StoreConfig must be read somewhere in src/.
    // This catches "config field defined but never wired up" bugs like the
    // historical writer.stack_size and sync.mode regressions.
    // This is part of the live configuration completeness contract.
    let config_src = fs::read_to_string("src/store/config.rs")
        .expect("read src/store/config.rs for config check");

    let config_ast = syn::parse_file(&config_src)
        .expect("parse src/store/config.rs for config field usage check");
    let fields = store_config_public_fields(&config_ast);
    if fields.is_empty() {
        return;
    }

    let mut used_fields = BTreeSet::new();
    walk_rs_files(Path::new("src"), &mut |path, contents| {
        if path
            .to_string_lossy()
            .replace('\\', "/")
            .ends_with("src/store/config.rs")
        {
            return;
        }
        let file = syn::parse_file(contents).unwrap_or_else(|err| {
            panic!(
                "CONFIG FIELD USAGE CHECK PARSE FAILURE in {}: {err}",
                path.display()
            )
        });
        let mut collector = StoreConfigFieldAccessCollector::new(&fields);
        collector.visit_file(&file);
        used_fields.extend(collector.found_fields);
    });

    for field in &fields {
        if !used_fields.contains(field) {
            panic!(
                "STORE CONFIG FIELD UNUSED: `{field}` is defined in StoreConfig but never \
                 accessed in any parsed src/ file outside src/store/config.rs.\n\
                 Every config field must be wired to actual behavior.\n\
                 Either use the field or remove it from StoreConfig.\n\
                 See: the historical writer.stack_size / sync.mode bugs that slipped through review."
            );
        }
    }
}

fn store_config_public_fields(file: &syn::File) -> BTreeSet<String> {
    for item in &file.items {
        if let syn::Item::Struct(item_struct) = item {
            if item_struct.ident == "StoreConfig" {
                let mut fields = BTreeSet::new();
                for field in &item_struct.fields {
                    if matches!(field.vis, syn::Visibility::Public(_)) {
                        if let Some(ident) = &field.ident {
                            fields.insert(ident.to_string());
                        }
                    }
                }
                return fields;
            }
        }
    }
    BTreeSet::new()
}

struct StoreConfigFieldAccessCollector<'a> {
    tracked_fields: &'a BTreeSet<String>,
    found_fields: BTreeSet<String>,
}

impl<'a> StoreConfigFieldAccessCollector<'a> {
    fn new(tracked_fields: &'a BTreeSet<String>) -> Self {
        Self {
            tracked_fields,
            found_fields: BTreeSet::new(),
        }
    }
}

impl Visit<'_> for StoreConfigFieldAccessCollector<'_> {
    fn visit_expr_field(&mut self, node: &syn::ExprField) {
        if let syn::Member::Named(ident) = &node.member {
            let field_name = ident.to_string();
            if self.tracked_fields.contains(&field_name) {
                self.found_fields.insert(field_name);
            }
        }
        syn::visit::visit_expr_field(self, node);
    }
}

fn collect_rs_contents(dir: &Path, buf: &mut String, exclude: Option<&str>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs_contents(&path, buf, exclude);
            } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
                if let Some(excl) = exclude {
                    if path.to_string_lossy().replace('\\', "/").ends_with(excl) {
                        continue;
                    }
                }
                if let Ok(contents) = fs::read_to_string(&path) {
                    buf.push_str(&contents);
                }
            }
        }
    }
}

/// Downstream post-mortem defense: every pub item in src/ must appear in at least
/// one test file. This is the library-shaped version of "dispatch functions with no
/// tests" — if a future AI campaign adds a pub fn/struct/enum/trait without a test,
/// the build fails. LAW-003 (No Orphan Infrastructure), FM-007 (Island Syndrome).
///
/// Fast text scan plus the canonical YAML allowlist. Keep this build-time gate
/// cheap and local, even though the richer structural mirror lives in the
/// integrity tool.
fn check_pub_items_have_tests() {
    // Collect all test file contents into one searchable string.
    let mut test_contents = String::new();
    collect_rs_contents(Path::new("tests"), &mut test_contents, None);
    // Also include src/ inline #[cfg(test)] modules — they count as tests.
    let mut src_contents = String::new();
    collect_rs_contents(Path::new("src"), &mut src_contents, None);

    let allowlist = load_pub_item_allowlist();
    let allowed_names: BTreeSet<&str> = allowlist.iter().map(|entry| entry.name.as_str()).collect();

    // Walk src/ and extract pub item names.
    walk_rs_files(Path::new("src"), &mut |path, contents| {
        let path_str = path.display().to_string();
        for (line_no, line) in contents.lines().enumerate() {
            let trimmed = line.trim();
            // Match: pub fn NAME, pub struct NAME, pub enum NAME, pub trait NAME
            let item_name = extract_pub_item_name(trimmed);
            if let Some(name) = item_name {
                if allowed_names.contains(name) {
                    continue;
                }
                // Check if this name appears in any test file
                if !test_contents.contains(name) && !has_test_reference(&src_contents, name) {
                    panic!(
                        "PUB ITEM UNTESTED: `{name}` in {path_str}:{}\n\
                         Every pub fn/struct/enum/trait must appear in at least one test file.\n\
                         Either add a test that exercises this item, or add it to\n\
                         traceability/pub_item_allowlist.yaml with a justification for why it's tested indirectly.\n\
                         See: LAW-003 (No Orphan Infrastructure), FM-007 (Island Syndrome).\n\
                         Post-mortem: downstream had 5 dispatch functions with zero tests.",
                        line_no + 1
                    );
                }
            }
        }
    });
}

fn load_pub_item_allowlist() -> Vec<PubItemAllowlistEntry> {
    let path = Path::new("traceability/pub_item_allowlist.yaml");
    let contents = fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
    let entries: Vec<PubItemAllowlistEntry> = yaml_serde::from_str(&contents)
        .unwrap_or_else(|err| panic!("failed to parse {}: {err}", path.display()));
    for entry in &entries {
        if entry.name.trim().is_empty() {
            panic!(
                "invalid {} entry: `name` must not be empty (justification: {})",
                path.display(),
                entry.justification
            );
        }
        if entry.justification.trim().is_empty() {
            panic!(
                "invalid {} entry for `{}`: `justification` must not be empty",
                path.display(),
                entry.name
            );
        }
    }
    entries
}

/// Extract the item name from a line like `pub fn foo(`, `pub struct Bar {`, etc.
/// Returns None if the line doesn't match a pub item declaration.
/// TODO(integrity-hardening): this heuristic does not handle `pub async fn`.
/// Upgrade public-item discovery to a syn visitor before trusting it as a
/// language-aware detector rather than a repo-shaped text scan.
fn extract_pub_item_name(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("pub ")?;
    // Skip pub(crate), pub(super), pub(in ...) — those aren't public API
    if rest.starts_with('(') {
        return None;
    }
    // Match the keyword
    let after_keyword = if let Some(r) = rest.strip_prefix("fn ") {
        r
    } else if let Some(r) = rest.strip_prefix("struct ") {
        r
    } else if let Some(r) = rest.strip_prefix("enum ") {
        r
    } else if let Some(r) = rest.strip_prefix("trait ") {
        r
    } else {
        return None;
    };
    // Extract the name (up to first non-alphanumeric/underscore)
    let name = after_keyword
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .next()?;
    if name.is_empty() {
        return None;
    }
    Some(name)
}

/// Check if a name appears in a #[cfg(test)] context within src/ contents.
/// Simple heuristic: the name appears somewhere in the source that also contains #[cfg(test)].
fn has_test_reference(src_contents: &str, name: &str) -> bool {
    // This is a coarse check — if the name appears in src/ at all beyond its definition,
    // it's likely referenced by inline tests or other modules.
    // The primary guard is the test_contents check; this is the fallback for inline tests.
    let mut count = 0;
    for line in src_contents.lines() {
        if line.contains(name) {
            count += 1;
        }
        // More than just the definition line means it's referenced elsewhere
        if count > 2 {
            return true;
        }
    }
    false
}

fn walk_rs_files(dir: &Path, check: &mut dyn FnMut(&Path, &str)) {
    //Recursive directory walk. Only reads .rs files.
    //Uses std::fs only — no external deps allowed in build scripts
    //unless declared in [build-dependencies].
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk_rs_files(&path, check);
            } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
                if let Ok(contents) = fs::read_to_string(&path) {
                    check(&path, &contents);
                }
            }
        }
    }
}
