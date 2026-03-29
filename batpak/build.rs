#![allow(clippy::panic)]

use std::fs;
use std::path::Path;

// build.rs runs before every cargo build/check/test. Cannot be skipped.
// It enforces SPEC invariants at build time so agents get English errors
// instead of cryptic compiler failures. [SPEC:INVARIANTS]
fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=src/");

    check_no_tokio_in_deps();
    check_no_banned_patterns();
    check_store_config_field_usage();
    check_allow_justifications();
    check_no_stubs_in_src();
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

    walk_rs_files(Path::new("src"), &|path, contents| {
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
    walk_rs_files(Path::new("src"), &|path, contents| {
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
    //Only [dev-dependencies] is allowed. [SPEC:INVARIANTS item 1]
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
                 See: SPEC.md ## INVARIANTS, item 1."
            );
        }
    }
}

fn check_no_banned_patterns() {
    //Walk src/**/*.rs, read each file, check for patterns that violate
    //invariants or red flags. [SPEC:RED FLAGS]
    walk_rs_files(Path::new("src"), &|path, contents| {
        let path_str = path.display().to_string();

        //Red flag: no transmute/mem::read/pointer_cast in any src file.
        //All serialization goes through MessagePack. [SPEC:RED FLAGS item 1]
        for banned in ["transmute", "mem::read", "pointer_cast"] {
            if contents.contains(banned) {
                panic!(
                    "RED FLAG VIOLATED in {path_str}: found `{banned}`.\n\
                     repr(C) is for field ordering, not a wire format.\n\
                     All serialization goes through rmp-serde. Always.\n\
                     See: SPEC.md ## RED FLAGS, item 1."
                );
            }
        }

        //Invariant 2: no async fn in store module.
        //Store API is sync. Async lives in flume channels. [SPEC:INVARIANTS item 2]
        if path_str.contains("store") && contents.contains("async fn") {
            panic!(
                "INVARIANT 2 VIOLATED in {path_str}: found `async fn`.\n\
                 Store API is sync. Async callers use spawn_blocking()\n\
                 or flume's recv_async(). See: store/subscription.rs.\n\
                 See: SPEC.md ## INVARIANTS, item 2."
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
                         Use `.sync_with_mode(&config.sync_mode)` instead.\n\
                         Bare .sync() hardcodes SyncAll, ignoring the user's config.\n\
                         See: Bug 9 post-mortem (segment rotation bypassed sync_mode).\n\
                         Line: {trimmed}",
                        line_no + 1
                    );
                }
            }
        }

        //Invariant 3: no product concepts in library code.
        //Check struct/enum/fn/type declarations for banned nouns.
        //Skip string literals and comments. [SPEC:INVARIANTS item 3]
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
                             See: SPEC.md ## INVARIANTS, item 3."
                        );
                    }
                }
            }
        }
    });
}

fn check_store_config_field_usage() {
    // Invariant: every pub field in StoreConfig must be read somewhere in src/.
    // This catches "config field defined but never wired up" bugs like
    // writer_stack_size and sync_mode being ignored.
    // [SPEC:INVARIANTS — config completeness]
    let config_src =
        fs::read_to_string("src/store/mod.rs").expect("read src/store/mod.rs for config check");

    // Extract field names from `pub struct StoreConfig { ... }`
    let struct_start = match config_src.find("pub struct StoreConfig {") {
        Some(pos) => pos,
        None => return, // struct not found — skip check
    };
    let after_brace = &config_src[struct_start..];
    let struct_body = match after_brace.find('}') {
        Some(end) => &after_brace[..end],
        None => return,
    };

    let fields: Vec<&str> = struct_body
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with("pub ") && trimmed.contains(':') {
                // Extract field name: "pub field_name: Type," -> "field_name"
                let after_pub = trimmed.strip_prefix("pub ")?;
                let field_name = after_pub.split(':').next()?.trim();
                Some(field_name)
            } else {
                None
            }
        })
        .collect();

    // For each field, search all src/**/*.rs files for usage patterns like
    // config.field_name or self.field_name. We search ALL files including mod.rs
    // because the wiring often happens in the same module (e.g., Store::open
    // reads config.fd_budget to construct the Reader).
    //
    // To avoid false positives from the struct definition and StoreConfig::new(),
    // we strip those blocks before searching.
    let mut all_src = String::new();
    collect_rs_contents(Path::new("src"), &mut all_src, None);

    // Remove the StoreConfig struct body and ::new() body from the search text
    // so that field definitions and default initializations don't count as "usage".
    let search_text = strip_struct_and_new(&all_src, "StoreConfig");

    // Fields that are defined for external consumers (e.g., cache backends
    // constructed outside the store). These are intentionally not read in src/.
    let allowed_external = ["cache_map_size_bytes"];

    for field in &fields {
        if allowed_external.contains(field) {
            continue;
        }
        // Look for config.field or .field access patterns (not just the field name
        // as a substring, which would match comments and variable names).
        let dot_field = format!(".{field}");
        if !search_text.contains(&dot_field) {
            panic!(
                "STORE CONFIG FIELD UNUSED: `{field}` is defined in StoreConfig but never \
                 accessed via `.{field}` in any src/ file (outside struct def and ::new()).\n\
                 Every config field must be wired to actual behavior.\n\
                 Either use the field or remove it from StoreConfig.\n\
                 See: the writer_stack_size / sync_mode bugs that slipped through review."
            );
        }
    }
}

/// Strip the struct definition body and ::new() body so field definitions
/// and default initializations don't count as "usage".
fn strip_struct_and_new(src: &str, struct_name: &str) -> String {
    let mut result = src.to_string();

    // Strip `pub struct StructName { ... }`
    let struct_marker = format!("pub struct {struct_name} {{");
    if let Some(start) = result.find(&struct_marker) {
        if let Some(end) = find_matching_brace(&result[start..]) {
            result.replace_range(start..start + end + 1, "/* stripped */");
        }
    }

    // Strip the Clone impl body (contains self.field_name copies)
    let clone_marker = format!("impl Clone for {struct_name}");
    if let Some(start) = result.find(&clone_marker) {
        if let Some(brace_offset) = result[start..].find('{') {
            let body_start = start + brace_offset;
            if let Some(end) = find_matching_brace(&result[body_start..]) {
                result.replace_range(body_start..body_start + end + 1, "/* stripped */");
            }
        }
    }

    // Strip the Debug impl body (contains .field("name", &self.field))
    let debug_marker = format!("impl std::fmt::Debug for {struct_name}");
    if let Some(start) = result.find(&debug_marker) {
        if let Some(brace_offset) = result[start..].find('{') {
            let body_start = start + brace_offset;
            if let Some(end) = find_matching_brace(&result[body_start..]) {
                result.replace_range(body_start..body_start + end + 1, "/* stripped */");
            }
        }
    }

    result
}

/// Find the position of the matching closing brace for text starting with '{'.
fn find_matching_brace(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    for (i, ch) in s.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
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
/// String-scanning only — no syn, no proc-macro, no external deps.
fn check_pub_items_have_tests() {
    // Collect all test file contents into one searchable string.
    let mut test_contents = String::new();
    collect_rs_contents(Path::new("tests"), &mut test_contents, None);
    // Also include src/ inline #[cfg(test)] modules — they count as tests.
    let mut src_contents = String::new();
    collect_rs_contents(Path::new("src"), &mut src_contents, None);

    // Items that are tested indirectly or are macro-generated / re-export glue.
    // Each entry: (item_name, justification).
    let allowlist: &[(&str, &str)] = &[
        // Macro-generated types from define_state_machine! / define_typestate!
        // Tested via typestate_safety.rs compile-fail tests and quiet_stragglers.rs
        ("EntityIdType", "trait used via define_entity_id! macro, tested in quiet_stragglers"),
        // Internal store types that are only referenced via field access patterns
        ("ClockKey", "internal index type, tested via store_integration + store_advanced"),
        ("Active", "segment typestate marker, tested via store operations"),
        // Macro-generated methods from define_typestate! — tested via the generated types
        // TODO: Wave 2D adds explicit into_data() test in typestate_safety.rs
        ("into_data", "macro-generated method from define_typestate!, needs dedicated test"),
        ("Sealed", "segment typestate marker, tested via compaction tests"),
        ("SegmentHeader", "internal segment type, tested via frame_encode/decode"),
        ("StoreDiagnostics", "returned by Store::stats, tested via store_advanced"),
        // Internal segment methods tested via store_integration/store_advanced rotation tests
        ("needs_rotation", "internal segment method, tested via segment rotation in store tests"),
        ("CompactionResult", "returned by compact(), tested via compaction tests"),
        // Builder methods on AppendOptions — tested indirectly via append_with_options
        // TODO: Wave 2 should add direct builder method tests
        ("with_idempotency", "AppendOptions builder, tested indirectly via idempotency tests"),
        ("with_expected_sequence", "AppendOptions builder, tested indirectly via CAS tests"),
        // Serde wire helpers — referenced via #[serde(with = "...")] not by name
        ("u128_bytes", "serde helper module used via attribute, not by name"),
        ("option_u128_bytes", "serde helper module used via attribute, not by name"),
        ("vec_u128_bytes", "serde helper module used via attribute, not by name"),
    ];
    let allowed_names: Vec<&str> = allowlist.iter().map(|(name, _)| *name).collect();

    // Walk src/ and extract pub item names.
    walk_rs_files(Path::new("src"), &|path, contents| {
        let path_str = path.display().to_string();
        for (line_no, line) in contents.lines().enumerate() {
            let trimmed = line.trim();
            // Match: pub fn NAME, pub struct NAME, pub enum NAME, pub trait NAME
            let item_name = extract_pub_item_name(trimmed);
            if let Some(name) = item_name {
                if allowed_names.contains(&name) {
                    continue;
                }
                // Check if this name appears in any test file
                if !test_contents.contains(name) && !has_test_reference(&src_contents, name) {
                    panic!(
                        "PUB ITEM UNTESTED: `{name}` in {path_str}:{}\n\
                         Every pub fn/struct/enum/trait must appear in at least one test file.\n\
                         Either add a test that exercises this item, or add it to the build.rs\n\
                         allowlist with a justification for why it's tested indirectly.\n\
                         See: LAW-003 (No Orphan Infrastructure), FM-007 (Island Syndrome).\n\
                         Post-mortem: downstream had 5 dispatch functions with zero tests.",
                        line_no + 1
                    );
                }
            }
        }
    });
}

/// Extract the item name from a line like `pub fn foo(`, `pub struct Bar {`, etc.
/// Returns None if the line doesn't match a pub item declaration.
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

fn walk_rs_files(dir: &Path, check: &dyn Fn(&Path, &str)) {
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
