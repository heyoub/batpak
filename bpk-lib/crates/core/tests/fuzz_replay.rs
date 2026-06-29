//! GAUNT-FUZZ-1 — the PR-blocking fuzz-replay gate (the gateable half).
//!
//! This is a NORMAL `#[test]` (not cargo-fuzz): it runs on the standard toolchain
//! under `--features dangerous-test-hooks`, with no nightly / libFuzzer / sanitizer.
//! It enumerates every fuzz target declared as a `[[bin]]` in `fuzz/Cargo.toml`,
//! loads every committed file under `fuzz/corpus/<target>/` and
//! `fuzz/regressions/<target>/`, calls the matching `batpak::__fuzz::*` contract fn
//! (under `catch_unwind`, so a panic becomes a test failure naming the offending
//! file), and asserts NO PANIC. Deterministic, fast (<1s/file), hardware-independent
//! — it earns blocking authority.
//!
//! The self-proving half is [`fuzz_replay_covers_every_target`]: it cross-checks the
//! declared `[[bin]]` set against this file's dispatch table AND the on-disk
//! regression dirs, so deleting a target's regressions — or adding a `[[bin]]`
//! without wiring it here / committing a fixture — REDS the gate.
//!
//! These wrappers all live behind `dangerous-test-hooks`; this whole test file only
//! compiles under that feature (the dispatch calls `batpak::__fuzz::*`).
#![cfg(feature = "dangerous-test-hooks")]

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use batpak::__fuzz;

/// The repo-relative path to the fuzz crate's manifest, resolved from this test
/// crate's `CARGO_MANIFEST_DIR` (`bpk-lib/crates/core`).
fn fuzz_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fuzz")
}

/// Every fuzz target declared in `fuzz/Cargo.toml` `[[bin]]`, paired with the
/// closure that replays one input buffer against the matching contract fn. The
/// closure MUST reproduce the same scalar-prefix split the corresponding fuzz
/// target uses, so the replayed bytes drive the identical code path.
///
/// Adding a `[[bin]]` without adding it here reds
/// [`fuzz_replay_covers_every_target`]; the dispatcher is the single source of
/// truth the meta-test holds the manifest accountable to.
type Replay = fn(&[u8]);

fn dispatch_table() -> Vec<(&'static str, Replay)> {
    vec![
        // frame_decode is the Phase 0 target. It uses the PUBLIC
        // `batpak::store::segment::frame_decode` (no `__fuzz` shim), but the gate
        // still replays its committed corpus/regressions to prove it never panics.
        ("frame_decode", replay_frame_decode as Replay),
        ("segment_header", replay_segment_header as Replay),
        ("sidx_entry", replay_sidx_entry as Replay),
        ("checkpoint_data", replay_checkpoint_data as Replay),
        (
            "checkpoint_snapshot_v6",
            replay_checkpoint_snapshot_v6 as Replay,
        ),
        ("mmap_entry", replay_mmap_entry as Replay),
        ("cache_meta", replay_cache_meta as Replay),
        ("projection_state", replay_projection_state as Replay),
        ("hidden_ranges", replay_hidden_ranges as Replay),
        ("mmap_index_load", replay_mmap_index_load as Replay),
        ("sidx_footer", replay_sidx_footer as Replay),
    ]
}

// --- Per-target replay closures (mirror each fuzz target's body) ---------------

fn replay_frame_decode(data: &[u8]) {
    let _ = batpak::store::segment::frame_decode(data);
}

fn replay_segment_header(data: &[u8]) {
    let _ = __fuzz::__fuzz_segment_header(data);
}

fn replay_sidx_entry(data: &[u8]) {
    let (segment_id, buf) = split_u64_prefix(data);
    let _ = __fuzz::__fuzz_sidx_entry(buf, segment_id);
}

fn replay_checkpoint_data(data: &[u8]) {
    let (version, body) = split_u16_prefix(data);
    let _ = __fuzz::__fuzz_checkpoint_data(version, body);
}

fn replay_checkpoint_snapshot_v6(data: &[u8]) {
    let _ = __fuzz::__fuzz_checkpoint_snapshot_v6(data);
}

fn replay_mmap_entry(data: &[u8]) {
    let (version, buf) = split_u16_prefix(data);
    let _ = __fuzz::__fuzz_mmap_entry(buf, version);
}

fn replay_cache_meta(data: &[u8]) {
    let _ = __fuzz::__fuzz_cache_meta(data);
}

fn replay_projection_state(data: &[u8]) {
    let _ = __fuzz::__fuzz_projection_state(data);
}

fn replay_hidden_ranges(data: &[u8]) {
    let _ = __fuzz::__fuzz_hidden_ranges(data);
}

fn replay_mmap_index_load(data: &[u8]) {
    let _ = __fuzz::__fuzz_mmap_index_load(data);
}

fn replay_sidx_footer(data: &[u8]) {
    let (segment_id, body) = split_u64_prefix(data);
    let _ = __fuzz::__fuzz_sidx_footer(body, segment_id);
}

/// Split an 8-byte LE `u64` prefix (matches the `sidx_entry` / `sidx_footer`
/// targets). Short inputs yield scalar 0 and an empty remainder.
fn split_u64_prefix(data: &[u8]) -> (u64, &[u8]) {
    if data.len() < 8 {
        return (0, &[]);
    }
    let (head, rest) = data.split_at(8);
    let scalar = u64::from_le_bytes(head.try_into().expect("8-byte prefix"));
    (scalar, rest)
}

/// Split a 2-byte LE `u16` prefix (matches the `checkpoint_data` / `mmap_entry`
/// targets). Short inputs yield scalar 0 and an empty remainder.
fn split_u16_prefix(data: &[u8]) -> (u16, &[u8]) {
    if data.len() < 2 {
        return (0, &[]);
    }
    let (head, rest) = data.split_at(2);
    let scalar = u16::from_le_bytes(head.try_into().expect("2-byte prefix"));
    (scalar, rest)
}

// --- The corpus/regression file enumerator -------------------------------------

/// Read every regular file directly under `dir` (non-recursive — the layout is one
/// flat dir per target). Returns `(path, bytes)`. A missing dir yields an empty
/// vec; that case is caught separately by the meta-test for regressions.
fn load_inputs(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let Ok(bytes) = std::fs::read(&path) else {
                unreachable!("read committed fuzz input {}", path.display());
            };
            out.push((path, bytes));
        }
    }
    out
}

/// Replay one buffer under `catch_unwind`, converting any panic into a test failure
/// that names the offending file and target.
fn assert_no_panic(target: &str, path: &Path, replay: Replay, data: &[u8]) {
    let result = catch_unwind(AssertUnwindSafe(|| replay(data)));
    assert!(
        result.is_ok(),
        "PANIC replaying fuzz input for target `{target}`: {} \
         (decode entry points must return a typed Err/false/None, never panic)",
        path.display()
    );
}

// --- The gate ------------------------------------------------------------------

/// THE GATE: replay every committed corpus + regression input through its contract
/// fn and assert none panics. This is the PR-blocking deterministic half of
/// GAUNT-FUZZ-1.
#[test]
fn fuzz_replay_no_panic_on_committed_corpus() {
    let fuzz = fuzz_dir();
    let mut replayed = 0usize;
    for (target, replay) in dispatch_table() {
        for sub in ["corpus", "regressions"] {
            let dir = fuzz.join(sub).join(target);
            for (path, data) in load_inputs(&dir) {
                assert_no_panic(target, &path, replay, &data);
                replayed += 1;
            }
        }
    }
    // Non-vacuity floor: we MUST have replayed the committed fixtures. A wrong
    // `fuzz_dir()` or an empty tree would otherwise pass by doing nothing.
    assert!(
        replayed >= dispatch_table().len(),
        "expected to replay at least one input per target ({} targets), replayed {replayed} \
         — corpus/regression dirs missing or fuzz_dir() mis-resolved?",
        dispatch_table().len()
    );
}

/// THE SELF-PROVING META-TEST: every fuzz target declared as a `[[bin]]` in
/// `fuzz/Cargo.toml` must (1) be wired into this file's [`dispatch_table`], AND (2)
/// have a `fuzz/regressions/<target>/` dir with >= 1 file. Deleting a target's
/// regressions, or adding a `[[bin]]` without a dispatcher + fixture, REDS this
/// test — so the gate cannot be silently hollowed out.
#[test]
fn fuzz_replay_covers_every_target() {
    let fuzz = fuzz_dir();
    let declared = declared_bin_targets(&fuzz.join("Cargo.toml"));
    assert!(
        !declared.is_empty(),
        "no `[[bin]]` targets parsed from fuzz/Cargo.toml — parser broken or manifest moved?"
    );

    let dispatched: std::collections::BTreeSet<&'static str> =
        dispatch_table().into_iter().map(|(name, _)| name).collect();

    for target in &declared {
        // (1) every declared bin is exercised by the replay dispatcher.
        assert!(
            dispatched.contains(target.as_str()),
            "fuzz target `{target}` is declared as a [[bin]] in fuzz/Cargo.toml but is NOT wired \
             into fuzz_replay.rs's dispatch_table — add a replay closure for it"
        );

        // (2) every declared bin has at least one committed regression fixture.
        let reg_dir = fuzz.join("regressions").join(target);
        let reg_files = load_inputs(&reg_dir);
        assert!(
            !reg_files.is_empty(),
            "fuzz target `{target}` has no committed regression fixture under {} \
             (every target must keep >= 1 RED fixture so the replay gate has teeth)",
            reg_dir.display()
        );
    }

    // And the reverse: no dispatcher entry is dead (each maps to a real [[bin]]),
    // so the dispatch table can't drift away from the manifest in either direction.
    let declared_set: std::collections::BTreeSet<&str> =
        declared.iter().map(String::as_str).collect();
    for name in &dispatched {
        assert!(
            declared_set.contains(name),
            "fuzz_replay.rs dispatches target `{name}` which is NOT declared as a [[bin]] in \
             fuzz/Cargo.toml — remove the stale dispatcher or restore the bin"
        );
    }
}

/// Parse the `name = "..."` of every `[[bin]]` section from the fuzz crate's
/// `Cargo.toml`. A deliberately tiny hand parser (no toml dep in this test crate):
/// it walks lines, tracks whether the current section header is `[[bin]]`, and
/// captures the first `name = "..."` within each such section.
fn declared_bin_targets(manifest: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(manifest) else {
        unreachable!("read fuzz manifest {}", manifest.display());
    };
    let mut targets = Vec::new();
    let mut in_bin = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            // A new section header ends the previous one.
            in_bin = line == "[[bin]]";
            continue;
        }
        if in_bin {
            if let Some(name) = parse_name_value(line) {
                targets.push(name);
                // Stop capturing further keys in this section; the next `[`
                // header resets `in_bin`.
                in_bin = false;
            }
        }
    }
    targets
}

/// If `line` is `name = "value"` (ignoring inline whitespace and trailing
/// comments), return `value`. Otherwise `None`.
fn parse_name_value(line: &str) -> Option<String> {
    let rest = line.strip_prefix("name")?.trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}
