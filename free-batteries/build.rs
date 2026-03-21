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
