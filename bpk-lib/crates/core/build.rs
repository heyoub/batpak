// justifies: INV-BUILD-FAIL-FAST; build.rs consolidates panic through the `fail` helper below, see build.rs and traceability/invariants.yaml
#![allow(clippy::panic)]

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;
mod shared_checks {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/build_support/shared_checks.rs"
    ));
    // Build-script-ONLY receipt + per-file-rerun helpers. Kept in a separate
    // file so the `batpak-integrity` binary (which includes shared_checks.rs via
    // tools/shared/shared_checks.rs) never compiles them as dead code under
    // `-D warnings`. Included here so they share shared_checks.rs's imports
    // (BTreeSet, Path/PathBuf, fs) inside this same module.
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/build_support/build_receipts.rs"
    ));
}

/// Single documented failure path for build.rs. Every invariant violation
/// routes through here so the panic surface is consolidated and auditable.
/// Emits a cargo-parseable warning on stdout, then panics so Cargo stops the
/// build without relying on `process::exit`.
fn fail(msg: &str) -> ! {
    println!("cargo:warning=build.rs: {msg}");
    panic!("build.rs invariant failed: {msg}")
}

/// Mirror of a `traceability/typed_waivers.yaml` entry, build-time view. The
/// authoritative validator (expiry, owner, adr resolution, L4 sign-off) lives in
/// `tools/integrity/src/typed_waivers.rs`; here build.rs only needs each
/// `pub-item` waiver's `target` so the public-surface check can skip it without
/// false-failing the build. `serde(default)` on unused fields keeps this view
/// tolerant of the full schema.
#[derive(Debug, Deserialize)]
struct TypedWaiverEntry {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    target: String,
}

// build.rs runs before every cargo build/check/test. Cannot be skipped.
// It enforces live runtime invariants at build time so agents get English
// errors instead of cryptic compiler failures. See README.md, MODEL.md,
// INVARIANTS.md, and CONFORMANCE.md for the current truth hierarchy.
fn main() {
    let repo_invariants_available = repo_invariant_surface_available();

    emit_rerun_lines(repo_invariants_available);

    check_no_tokio_in_deps();
    check_no_banned_patterns();
    check_store_config_field_usage();
    // The three surface-dependent lints are the vacuous-pass risk: in a packaged
    // crate the repo invariant surface is absent. Rather than skip them
    // silently, `run_surface_lint` emits a `cargo:warning=` AND writes an
    // auditable SKIPPED_PACKAGED receipt; on a real workspace run each writes a
    // PASS receipt with its real counts. See P1-3b.
    let avail = repo_invariants_available;
    shared_checks::run_surface_lint(
        avail,
        "no-dead-code-silencers",
        check_no_dead_code_silencers,
    );
    shared_checks::run_surface_lint(avail, "allow-justifications", check_allow_justifications);
    check_no_stubs_in_src();
    check_store_surface_honesty();
    check_no_fixed_temp_patterns();
    shared_checks::run_surface_lint(avail, "pub-items-have-tests", check_pub_items_have_tests);
    check_platform_profile_env();
}

/// Emit ONE `cargo:rerun-if-changed` per `.rs` file the lints actually read
/// (per-file is the only form Cargo honors for nested edits), plus the explicit
/// non-`.rs` inputs (Cargo.toml, traceability YAML, doc/ADR markdown). The
/// surface-dependent roots are only emitted when present.
fn emit_rerun_lines(repo_invariants_available: bool) {
    shared_checks::emit_build_rerun_lines(&core_root(), &repo_root(), repo_invariants_available);
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn core_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo_invariant_surface_available() -> bool {
    let repo_root = repo_root();
    [
        repo_root.join("Cargo.toml"),
        repo_root.join("traceability/dead_code_silencer_allowlist.yaml"),
        repo_root.join("traceability/typed_waivers.yaml"),
        repo_root.join("traceability/invariants.yaml"),
        repo_root.join("tools/xtask/src"),
        repo_root.join("tools/integrity/src"),
    ]
    .iter()
    .all(|path| path.exists())
}

fn repo_relative_display(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

#[derive(Debug, Serialize, Deserialize)]
struct BuildPlatformProfile {
    schema_version: u16,
    host: BuildPlatformProfileHost,
    store_path: BuildStorePathProfile,
    admission: BuildPlatformAdmissionProfile,
    fingerprint_crc32: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct BuildPlatformProfileHost {
    monotonic_clock: BuildClockEvidence,
}

#[derive(Debug, Serialize, Deserialize)]
struct BuildStorePathProfile {
    path_status: BuildStorePathStatusEvidence,
    parent_dir_sync: BuildParentDirSyncEvidence,
    lock_leaf_symlink_protection: BuildLockLeafSymlinkProtection,
    mmap_index: BuildMmapEvidence,
    sealed_segment_mmap: BuildMmapEvidence,
    active_segment_read: BuildActiveSegmentReadEvidence,
}

#[derive(Debug, Serialize, Deserialize)]
struct BuildPlatformAdmissionProfile {
    store_lock: BuildStoreLockAdmissionSummary,
    parent_dir_sync: BuildParentDirSyncAdmissionSummary,
    mmap_index: BuildMmapAdmissionSummary,
    sealed_segment_mmap: BuildMmapAdmissionSummary,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum BuildClockEvidence {
    ProcessLocalInstantAnchor,
    Unknown,
    ProbeFailed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum BuildStorePathStatusEvidence {
    ObservedDirectory,
    UnknownMissing,
    ObservedUnsupportedNotDirectory,
    ProbeFailed { reason: String },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum BuildParentDirSyncEvidence {
    UnixFsync,
    RenameOnly,
    Unknown,
    ProbeFailed,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum BuildLockLeafSymlinkProtection {
    AtomicNoFollow,
    BestEffortCheckThenOpen,
    Unknown,
    ObservedUnsupported,
    ProbeFailed,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum BuildMmapEvidence {
    FileBacked,
    Unknown,
    ObservedUnsupported,
    ProbeFailed,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum BuildActiveSegmentReadEvidence {
    UnixReadAt,
    LockedSeekRead,
    Unknown,
    ProbeFailed,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum BuildStoreLockAdmissionSummary {
    AtomicNoFollow,
    BestEffortCheckThenOpen,
    Rejected,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum BuildParentDirSyncAdmissionSummary {
    UnixFsync,
    RenameOnly,
    Rejected,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum BuildMmapAdmissionSummary {
    FileBacked,
    Rejected,
}

#[derive(Serialize)]
struct BuildPlatformProfileBody<'a> {
    schema_version: u16,
    host: &'a BuildPlatformProfileHost,
    store_path: &'a BuildStorePathProfile,
    admission: &'a BuildPlatformAdmissionProfile,
}

fn check_platform_profile_env() {
    let Ok(path) = std::env::var("BATPAK_PLATFORM_PROFILE") else {
        return;
    };
    println!("cargo:rerun-if-changed={path}");
    let bytes = fs::read(&path).unwrap_or_else(|error| {
        fail(&format!(
            "cannot read BATPAK_PLATFORM_PROFILE={path}: {error}"
        ))
    });
    let profile: BuildPlatformProfile = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        fail(&format!(
            "cannot decode BATPAK_PLATFORM_PROFILE={path}: {error}"
        ))
    });
    if profile.schema_version != 1 {
        fail(&format!(
            "BATPAK_PLATFORM_PROFILE={path} has schema_version {}; expected 1",
            profile.schema_version
        ));
    }
    validate_build_platform_profile_semantics(&profile, &path);
    let body = BuildPlatformProfileBody {
        schema_version: profile.schema_version,
        host: &profile.host,
        store_path: &profile.store_path,
        admission: &profile.admission,
    };
    let body_bytes = serde_json::to_vec(&body).unwrap_or_else(|error| {
        fail(&format!(
            "cannot canonicalize BATPAK_PLATFORM_PROFILE={path}: {error}"
        ))
    });
    let computed = crc32fast::hash(&body_bytes);
    if computed != profile.fingerprint_crc32 {
        fail(&format!(
            "BATPAK_PLATFORM_PROFILE={path} fingerprint_crc32 {} does not match computed {}",
            profile.fingerprint_crc32, computed
        ));
    }
    println!("cargo:rustc-env=BATPAK_PLATFORM_PROFILE_PATH={path}");
    println!("cargo:rustc-env=BATPAK_PLATFORM_PROFILE_FINGERPRINT_CRC32={computed}");
}

fn validate_build_platform_profile_semantics(profile: &BuildPlatformProfile, path: &str) {
    let expected_store_lock = match profile.store_path.lock_leaf_symlink_protection {
        BuildLockLeafSymlinkProtection::AtomicNoFollow => {
            BuildStoreLockAdmissionSummary::AtomicNoFollow
        }
        BuildLockLeafSymlinkProtection::BestEffortCheckThenOpen => {
            BuildStoreLockAdmissionSummary::BestEffortCheckThenOpen
        }
        BuildLockLeafSymlinkProtection::Unknown
        | BuildLockLeafSymlinkProtection::ObservedUnsupported
        | BuildLockLeafSymlinkProtection::ProbeFailed => BuildStoreLockAdmissionSummary::Rejected,
    };
    if profile.admission.store_lock != expected_store_lock {
        fail(&format!(
            "BATPAK_PLATFORM_PROFILE={path} has inconsistent store_lock admission {:?}; expected {:?} from lock evidence {:?}",
            profile.admission.store_lock,
            expected_store_lock,
            profile.store_path.lock_leaf_symlink_protection
        ));
    }

    let expected_parent_dir_sync = match profile.store_path.parent_dir_sync {
        BuildParentDirSyncEvidence::UnixFsync => BuildParentDirSyncAdmissionSummary::UnixFsync,
        BuildParentDirSyncEvidence::RenameOnly => BuildParentDirSyncAdmissionSummary::RenameOnly,
        BuildParentDirSyncEvidence::Unknown | BuildParentDirSyncEvidence::ProbeFailed => {
            BuildParentDirSyncAdmissionSummary::Rejected
        }
    };
    if profile.admission.parent_dir_sync != expected_parent_dir_sync {
        fail(&format!(
            "BATPAK_PLATFORM_PROFILE={path} has inconsistent parent_dir_sync admission {:?}; expected {:?} from parent-dir evidence {:?}",
            profile.admission.parent_dir_sync,
            expected_parent_dir_sync,
            profile.store_path.parent_dir_sync
        ));
    }

    validate_build_path_mmap_consistency(path, "mmap_index", profile);
    validate_build_path_mmap_consistency(path, "sealed_segment_mmap", profile);
    validate_build_mmap_admission(
        path,
        "mmap_index",
        &profile.store_path.mmap_index,
        &profile.admission.mmap_index,
    );
    validate_build_mmap_admission(
        path,
        "sealed_segment_mmap",
        &profile.store_path.sealed_segment_mmap,
        &profile.admission.sealed_segment_mmap,
    );
}

fn validate_build_path_mmap_consistency(path: &str, field: &str, profile: &BuildPlatformProfile) {
    let evidence = match field {
        "mmap_index" => &profile.store_path.mmap_index,
        "sealed_segment_mmap" => &profile.store_path.sealed_segment_mmap,
        _ => fail(&format!(
            "internal build profile validation bug: unknown mmap field {field}"
        )),
    };
    let required = match profile.store_path.path_status {
        BuildStorePathStatusEvidence::ObservedDirectory => return,
        BuildStorePathStatusEvidence::ObservedUnsupportedNotDirectory => {
            BuildMmapEvidence::ObservedUnsupported
        }
        BuildStorePathStatusEvidence::UnknownMissing => BuildMmapEvidence::Unknown,
        BuildStorePathStatusEvidence::ProbeFailed { .. } => BuildMmapEvidence::ProbeFailed,
    };
    if evidence != &required {
        fail(&format!(
            "BATPAK_PLATFORM_PROFILE={path} has inconsistent {field} evidence {evidence:?}; expected {required:?} from path_status {:?}",
            profile.store_path.path_status
        ));
    }
}

fn validate_build_mmap_admission(
    path: &str,
    field: &str,
    evidence: &BuildMmapEvidence,
    admission: &BuildMmapAdmissionSummary,
) {
    let expected = match evidence {
        BuildMmapEvidence::FileBacked => BuildMmapAdmissionSummary::FileBacked,
        BuildMmapEvidence::Unknown
        | BuildMmapEvidence::ObservedUnsupported
        | BuildMmapEvidence::ProbeFailed => BuildMmapAdmissionSummary::Rejected,
    };
    if admission != &expected {
        fail(&format!(
            "BATPAK_PLATFORM_PROFILE={path} has inconsistent {field} admission {admission:?}; expected {expected:?} from mmap evidence {evidence:?}"
        ));
    }
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
        let path_str = repo_relative_display(path);
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

fn check_no_dead_code_silencers() -> shared_checks::LintCounts {
    let repo_root = repo_root();
    let allowlisted = shared_checks::load_dead_code_silencer_allowlist(&repo_root)
        .unwrap_or_else(|err| fail(&err));
    let mut counts = shared_checks::LintCounts {
        files_examined: 0,
        assertions_run: 0,
        inputs: BTreeSet::new(),
    };
    walk_dead_code_checked_rs_files(&mut |path, contents| {
        counts.files_examined += 1;
        // One "no un-allowlisted dead_code silencer" assertion per file scanned.
        counts.assertions_run += 1;
        counts.inputs.insert(path.to_path_buf());
        let rel = path
            .strip_prefix(&repo_root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let sites =
            shared_checks::collect_dead_code_silencer_sites(contents).unwrap_or_else(|err| {
                fail(&format!(
                    "cannot parse {} while checking dead_code silencers: {err}",
                    rel
                ))
            });
        for site in sites {
            // Each candidate silencer site is an additional assertion.
            counts.assertions_run += 1;
            let allowlist_site = format!("{rel}:{}", site.line);
            if allowlisted.contains(&allowlist_site) {
                continue;
            }
            fail(&format!(
                    "dead_code silencers are not tolerated in this repo: {rel}:{}:{}\n\
                     Found `{}`.\n\
                     If code is test-only, use #[cfg(test)]. If it is unused, delete it.\n\
                     If it is shared infrastructure, restructure it so the compiler sees the real ownership surface.\n\
                     If this is the rare legitimate exception, add `{allowlist_site}` to traceability/dead_code_silencer_allowlist.yaml with `reason` and `adr`.",
                    site.line,
                    site.column,
                    site.rendered,
                ));
        }
    });
    counts
}

/// INV-ALLOW-IS-DESIGN enforcement: every #[allow(...)] in runtime or
/// toolchain Rust code must carry a structured `// justifies:` comment with
/// >= 5 words AND >= 1 resolvable anchor (INV-id, ADR-NNNN, or repo path).
fn check_allow_justifications() -> shared_checks::LintCounts {
    let repo_root = repo_root();
    let known_invariants =
        shared_checks::load_known_invariants(&repo_root).unwrap_or_else(|err| fail(&err));
    let mut counts = shared_checks::LintCounts {
        files_examined: 0,
        assertions_run: 0,
        inputs: BTreeSet::new(),
    };
    walk_allow_checked_rs_files(&mut |path, contents| {
        counts.files_examined += 1;
        counts.inputs.insert(path.to_path_buf());
        let path_str = repo_relative_display(path);
        let lines: Vec<&str> = contents.lines().collect();
        for (line_no, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("#![allow(") || trimmed.starts_with("#[allow(") {
                // Each `#[allow]`/`#![allow]` site is one justification assertion.
                counts.assertions_run += 1;
                let has_justification =
                    shared_checks::line_carries_justification(line, &repo_root, &known_invariants)
                        || (line_no > 0
                            && lines
                                .get(line_no - 1)
                                .map(|prev| {
                                    shared_checks::line_carries_justification(
                                        prev,
                                        &repo_root,
                                        &known_invariants,
                                    )
                                })
                                .unwrap_or(false));
                if !has_justification {
                    fail(&format!(
                        "ROGUE SILENCE in {path_str}:{}: `{trimmed}`\n\
                         Every #[allow(...)] must carry a `// justifies: <>=5 words + >=1 resolvable anchor>`\n\
                         comment on the same or preceding line. Anchors: INV-<NAME> from\n\
                         traceability/invariants.yaml, ADR-NNNN from root ADR files, or a concrete repo path\n\
                         (src/..., tests/..., examples/..., crates/macros/..., crates/macros-support/...,\n\
                         benches/..., tools/..., build.rs). See INV-ALLOW-IS-DESIGN.",
                        line_no + 1
                    ));
                }
            }
        }
    });
    counts
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
                 See: INVARIANTS.md."
            );
        }
    }
}

fn check_no_banned_patterns() {
    //Walk src/**/*.rs, read each file, check for patterns that violate
    //invariants or red flags.
    walk_rs_files(Path::new("src"), &mut |path, contents| {
        let path_str = repo_relative_display(path);

        //Red flag: no transmute/mem::read/pointer_cast in any src file.
        //All serialization goes through MessagePack.
        for banned in ["transmute", "mem::read", "pointer_cast"] {
            if contents.contains(banned) {
                panic!(
                    "RED FLAG VIOLATED in {path_str}: found `{banned}`.\n\
                     repr(C) is for field ordering, not a wire format.\n\
                     All serialization goes through rmp-serde. Always.\n\
                     See: INVARIANTS.md."
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
                 See: INVARIANTS.md."
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

        // INV-3: ban domain-register nouns in public declarations everywhere except the
        // documented Lane-A substrate carve-out (`src/artifact.rs` owns the `artifact` noun).
        // `trajectory` remains banned crate-wide on declaration lines because it names
        // caller/application layers, not the substrate.
        const INV3_ARTIFACT_ALLOWED_PATH: &str = "src/artifact.rs";

        #[inline]
        fn inv3_declaration_has_word(lower: &str, noun: &str) -> bool {
            lower
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .any(|word| {
                    word == noun
                        || word.starts_with(&format!("{noun}_"))
                        || word.ends_with(&format!("_{noun}"))
                        || word.contains(&format!("_{noun}_"))
                })
        }

        //NOTE: "scope" and "agent" are common English words.
        //"turn" and "note" are substrings of "return" and "annotation" —
        //substring matching would false-positive on legitimate Rust code.
        //Strategy: check lines starting with pub/fn/struct/enum/type for word-boundary matches.
        for line in contents.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") || trimmed.starts_with("///") {
                continue;
            }
            let is_decl = trimmed.starts_with("pub ")
                || trimmed.starts_with("fn ")
                || trimmed.starts_with("struct ")
                || trimmed.starts_with("enum ")
                || trimmed.starts_with("type ");
            if !is_decl {
                continue;
            }
            let lower = trimmed.to_lowercase();

            for noun in ["trajectory"] {
                if inv3_declaration_has_word(&lower, noun) {
                    panic!(
                        "INVARIANT 3 VIOLATED in {path_str}: \
                         product concept `{noun}` in declaration:\n  {trimmed}\n\
                         Library vocabulary: coordinate, entity, event, outcome, \
                         gate, region, transition.\n\
                         See: INVARIANTS.md."
                    );
                }
            }

            if inv3_declaration_has_word(&lower, "artifact") {
                let artifact_decl_ok_in_lib =
                    path_str == "src/lib.rs" && trimmed == "pub mod artifact;";
                let artifact_decl_ok_in_prelude =
                    path_str == "src/prelude.rs" && trimmed.starts_with("pub use crate::artifact");
                if !(path_str == INV3_ARTIFACT_ALLOWED_PATH
                    || artifact_decl_ok_in_lib
                    || artifact_decl_ok_in_prelude)
                {
                    panic!(
                        "INVARIANT 3 VIOLATED in {path_str}: \
                         product concept `artifact` in declaration:\n  {trimmed}\n\
                         Lane-A exception: definitions only in `{INV3_ARTIFACT_ALLOWED_PATH}`; \
                         crate wiring may use `pub mod artifact;` in `src/lib.rs` and \
                         `pub use crate::artifact::{{...}}` in `src/prelude.rs`.\n\
                         See: INVARIANTS.md."
                    );
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
        let path_str = repo_relative_display(path);
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
        let path_str = repo_relative_display(path);
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
            fail(&format!(
                "CONFIG FIELD USAGE CHECK PARSE FAILURE in {}: {err}",
                path.display()
            ))
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

/// Public-surface post-mortem defense: every public item in src/ must have at least
/// one real path-position reference in a test file (verified via AST walk, not
/// substring match). The only exemption is a typed `pub-item` waiver in
/// `traceability/typed_waivers.yaml`; the authoritative waiver validation
/// (expiry, owner, adr, L4 sign-off) lives in
/// `tools/integrity/src/typed_waivers.rs`. LAW-003 (No Orphan Infrastructure),
/// FM-007 (Island Syndrome).
fn check_pub_items_have_tests() -> shared_checks::LintCounts {
    // After the P0-2 triage this set is empty: every public item is named
    // directly in a test file, so the walk below proves coverage with zero
    // waivers. A typed `pub-item` waiver target is skipped here.
    let allowed_names: BTreeSet<String> = load_pub_item_waiver_targets();

    // Parse every test file once.
    let test_files: Vec<(std::path::PathBuf, syn::File)> = collect_rs_file_asts(Path::new("tests"));

    let mut counts = shared_checks::LintCounts {
        files_examined: 0,
        assertions_run: 0,
        inputs: BTreeSet::new(),
    };
    for (test_path, _) in &test_files {
        counts.inputs.insert(test_path.clone());
    }

    // Walk src/ and collect public item names from the parsed AST, then
    // confirm each name has at least one real path-position reference in the
    // parsed tests.
    walk_rs_files(Path::new("src"), &mut |path, contents| {
        if path.ends_with("prelude.rs") {
            return;
        }
        counts.files_examined += 1;
        counts.inputs.insert(path.to_path_buf());
        let path_str = repo_relative_display(path);
        let file = syn::parse_file(contents).unwrap_or_else(|err| {
            fail(&format!(
                "PUB ITEM REFERENCE CHECK PARSE FAILURE in {path_str}: {err}\n\
                 This detector is syntax-aware by design; fix the source or the parser input."
            ))
        });
        for name in shared_checks::public_item_names(&file) {
            if allowed_names.contains(&name) {
                continue;
            }
            // Each public item checked for a test witness is one assertion.
            counts.assertions_run += 1;
            let witnessed = test_files
                .iter()
                .any(|(_, ast)| shared_checks::ast_references_name(ast, &name));
            if !witnessed {
                let (line, _col) = item_line_in_file(contents, &name);
                fail(&format!(
                    "pub item `{name}` declared at {path_str}:{line} has no test reference (checked {n} test files via AST); either add a real test use, hide the item via `#[doc(hidden)]`, or — only if it genuinely cannot be directly test-named — add a typed `pub-item` waiver in traceability/typed_waivers.yaml.",
                    n = test_files.len(),
                ));
            }
        }
    });
    counts
}

fn collect_rs_file_asts(dir: &Path) -> Vec<(std::path::PathBuf, syn::File)> {
    let mut out = Vec::new();
    let entries = fs::read_dir(dir).unwrap_or_else(|err| {
        fail(&format!(
            "cannot read {} while collecting test witness ASTs: {err}",
            dir.display()
        ))
    });
    for entry in entries {
        let entry = entry.unwrap_or_else(|err| {
            fail(&format!(
                "cannot walk {} while collecting test witness ASTs: {err}",
                dir.display()
            ))
        });
        let path = entry.path();
        if path.is_dir() {
            out.extend(collect_rs_file_asts(&path));
        } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
            let contents = fs::read_to_string(&path).unwrap_or_else(|err| {
                fail(&format!(
                    "cannot read {} while collecting test witness ASTs: {err}",
                    path.display()
                ))
            });
            let file = syn::parse_file(&contents).unwrap_or_else(|err| {
                fail(&format!(
                    "cannot parse {} while collecting test witness ASTs: {err}",
                    path.display()
                ))
            });
            out.push((path, file));
        }
    }
    out
}

fn item_line_in_file(contents: &str, name: &str) -> (usize, usize) {
    for (idx, line) in contents.lines().enumerate() {
        if line.contains(name) {
            return (idx + 1, line.find(name).unwrap_or(0) + 1);
        }
    }
    (0, 0)
}

/// Build-time view of the typed-waiver targets for the `pub-item` gate kind. A
/// missing file means zero waivers (the desired end state). Full schema
/// validation is the integrity gate's job (`typed_waivers::check`); build.rs only
/// reads `target` for `kind: pub-item` so it can skip those names.
fn load_pub_item_waiver_targets() -> BTreeSet<String> {
    let repo_root = repo_root();
    let path = repo_root.join("traceability/typed_waivers.yaml");
    let contents = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return BTreeSet::new(),
    };
    let entries: Vec<TypedWaiverEntry> = yaml_serde::from_str(&contents)
        .unwrap_or_else(|err| fail(&format!("failed to parse {}: {err}", path.display())));
    entries
        .into_iter()
        .filter(|entry| entry.kind == "pub-item")
        .map(|entry| entry.target)
        .filter(|target| !target.trim().is_empty())
        .collect()
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

fn walk_allow_checked_rs_files(check: &mut dyn FnMut(&Path, &str)) {
    let core_root = core_root();
    let repo_root = repo_root();
    let roots = [
        core_root.join("build.rs"),
        core_root.join("src"),
        repo_root.join("tools/xtask/src"),
        repo_root.join("tools/integrity/src"),
    ];
    for root in &roots {
        if root.is_file() {
            if let Ok(contents) = fs::read_to_string(root) {
                check(root, &contents);
            }
        } else {
            walk_rs_files(root, check);
        }
    }
}

fn walk_dead_code_checked_rs_files(check: &mut dyn FnMut(&Path, &str)) {
    let core_root = core_root();
    let repo_root = repo_root();
    let roots = [
        core_root.join("build.rs"),
        core_root.join("src"),
        core_root.join("tests"),
        core_root.join("examples"),
        core_root.join("benches"),
        repo_root.join("tools/xtask/src"),
        repo_root.join("tools/integrity/src"),
        repo_root.join("crates/macros/src"),
        repo_root.join("crates/macros-support/src"),
    ];
    for root in &roots {
        if root.is_file() {
            if let Ok(contents) = fs::read_to_string(root) {
                check(root, &contents);
            }
        } else {
            walk_rs_files(root, check);
        }
    }
}
