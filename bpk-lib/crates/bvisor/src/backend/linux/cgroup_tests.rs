//! Leaf-lifecycle unit tests for the SAFE cgroup v2 manager (split out of
//! `cgroup.rs` to keep the production file under the non-overridable
//! structural-check size cap). Reaches the crate-private items via
//! `use super::*` (`super` = the `cgroup` module, of which this is the
//! `#[path]`-included `tests` child).

use super::*;

/// Build a FAKE cgroup v2 tree under a tempdir: a `base` dir whose
/// `cgroup.subtree_control` delegates `delegated`, containing an EMPTY leaf
/// dir (no interface files yet — `create` writes them). Returns
/// `(tempdir, base_path)`; the tempdir is kept alive by the caller.
fn fake_base(delegated: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = tmp.path().join("base");
    fs::create_dir(&base).expect("base dir");
    fs::write(base.join(CONTROLLERS_FILE), delegated).expect("controllers");
    fs::write(base.join(SUBTREE_CONTROL_FILE), delegated).expect("subtree_control");
    (tmp, base)
}

/// Pre-seed a leaf's kill+procs interface files (the kernel auto-creates
/// these; the fake tree must materialise them for the write/read assertions).
fn seed_leaf_interface(dir: &Path) {
    fs::write(dir.join(PROCS_FILE), "").expect("procs");
    fs::write(dir.join(KILL_FILE), "").expect("kill");
}

#[test]
fn create_writes_pids_max_to_the_interface_file() {
    let (_tmp, base) = fake_base("pids memory");
    let leaf =
        CgroupLeaf::create(&base, "leaf", CgroupLimits::with_pids_max(64)).expect("create leaf");
    let written =
        fs::read_to_string(leaf.dir().expect("dir").join(PIDS_MAX_FILE)).expect("read pids.max");
    assert_eq!(
        written, "64",
        "pids.max must hold exactly the requested limit"
    );
    assert!(leaf.setup().pids_enforced, "pids was delegated ⇒ enforced");
    assert!(!leaf.setup().memory_enforced, "memory was not requested");
}

#[test]
fn create_writes_memory_max_when_requested_and_delegated() {
    let (_tmp, base) = fake_base("pids memory");
    let limits = CgroupLimits::with_pids_max(8).and_memory_max(1_048_576);
    let leaf = CgroupLeaf::create(&base, "leaf", limits).expect("create leaf");
    let mem = fs::read_to_string(leaf.dir().expect("dir").join(MEMORY_MAX_FILE))
        .expect("read memory.max");
    assert_eq!(
        mem, "1048576",
        "memory.max must hold exactly the requested bytes"
    );
    assert!(
        leaf.setup().memory_enforced,
        "memory was delegated ⇒ enforced"
    );
}

#[test]
fn limit_on_undelegated_controller_is_an_honest_error_not_silent() {
    // `base` delegates ONLY pids; requesting a memory limit must fail closed —
    // never silently leave memory unbounded while the caller believes it set.
    let (_tmp, base) = fake_base("pids");
    let limits = CgroupLimits::with_pids_max(4).and_memory_max(4096);
    let err =
        CgroupLeaf::create(&base, "leaf", limits).expect_err("undelegated memory limit must error");
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    // The failed create must leave NO half-configured leaf behind.
    assert!(
        !base.join("leaf").exists(),
        "a failed create must remove the half-leaf"
    );
}

#[test]
fn member_pids_parses_multiline_procs_into_a_vec() {
    let (_tmp, base) = fake_base("pids");
    let leaf =
        CgroupLeaf::create(&base, "leaf", CgroupLimits::with_pids_max(16)).expect("create leaf");
    // The kernel writes one pid per line; create did not seed procs, so write it.
    fs::write(leaf.dir().expect("dir").join(PROCS_FILE), "101\n202\n303\n").expect("seed procs");
    let pids = leaf.member_pids().expect("member pids");
    assert_eq!(pids, vec![101, 202, 303]);
}

#[test]
fn member_pids_on_empty_procs_is_empty() {
    let (_tmp, base) = fake_base("pids");
    let leaf =
        CgroupLeaf::create(&base, "leaf", CgroupLimits::with_pids_max(2)).expect("create leaf");
    fs::write(leaf.dir().expect("dir").join(PROCS_FILE), "").expect("empty procs");
    assert!(leaf.member_pids().expect("member pids").is_empty());
}

#[test]
fn member_pids_rejects_a_nonnumeric_line() {
    let (_tmp, base) = fake_base("pids");
    let leaf =
        CgroupLeaf::create(&base, "leaf", CgroupLimits::with_pids_max(2)).expect("create leaf");
    fs::write(leaf.dir().expect("dir").join(PROCS_FILE), "101\nnope\n").expect("seed bad procs");
    let err = leaf.member_pids().expect_err("non-numeric line must error");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn kill_writes_one_to_the_kill_file_when_present() {
    let (_tmp, base) = fake_base("pids");
    let leaf =
        CgroupLeaf::create(&base, "leaf", CgroupLimits::with_pids_max(2)).expect("create leaf");
    seed_leaf_interface(leaf.dir().expect("dir"));
    leaf.kill().expect("kill writes 1");
    let killed = fs::read_to_string(leaf.dir().expect("dir").join(KILL_FILE)).expect("read kill");
    assert_eq!(killed, "1", "cgroup.kill must receive exactly \"1\"");
}

#[test]
fn kill_without_kill_file_is_an_honest_unsupported_error() {
    // A pre-5.14 fake (no cgroup.kill) must NOT pretend it killed.
    let (_tmp, base) = fake_base("pids");
    let leaf =
        CgroupLeaf::create(&base, "leaf", CgroupLimits::with_pids_max(2)).expect("create leaf");
    // Deliberately do NOT seed cgroup.kill.
    let err = leaf.kill().expect_err("absent cgroup.kill must error");
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
}

#[test]
fn dir_fd_opens_the_leaf_directory() {
    let (_tmp, base) = fake_base("pids");
    let leaf =
        CgroupLeaf::create(&base, "leaf", CgroupLimits::with_pids_max(2)).expect("create leaf");
    // Opening the leaf dir as an OwnedFd is the 8b CLONE_INTO_CGROUP handle.
    let fd = leaf.dir_fd().expect("open leaf dir fd");
    // A directory fd is non-negative and owned (closed on drop) — just prove
    // it materialised.
    use std::os::fd::AsRawFd;
    assert!(fd.as_raw_fd() >= 0);
}

#[test]
fn peak_pids_reads_the_high_water_mark_or_honest_none() {
    let (_tmp, base) = fake_base("pids");
    let leaf =
        CgroupLeaf::create(&base, "leaf", CgroupLimits::with_pids_max(8)).expect("create leaf");
    // Absent pids.peak (older-kernel fake) ⇒ honest None, never a fabricated peak.
    assert_eq!(leaf.peak_pids().expect("peak read"), None);
    // Present pids.peak ⇒ the parsed high-water mark.
    fs::write(leaf.dir().expect("dir").join(PIDS_PEAK_FILE), "5\n").expect("seed peak");
    assert_eq!(leaf.peak_pids().expect("peak read"), Some(5));
}

#[test]
fn wait_until_empty_returns_immediately_on_an_empty_leaf() {
    let (_tmp, base) = fake_base("pids");
    let leaf =
        CgroupLeaf::create(&base, "leaf", CgroupLimits::with_pids_max(2)).expect("create leaf");
    fs::write(leaf.dir().expect("dir").join(PROCS_FILE), "").expect("empty procs");
    // First read is empty ⇒ true with no sleep at all.
    assert!(leaf
        .wait_until_empty(1, Duration::from_millis(0))
        .expect("wait"));
}

#[test]
fn wait_until_empty_reports_honest_false_when_members_persist() {
    let (_tmp, base) = fake_base("pids");
    let leaf =
        CgroupLeaf::create(&base, "leaf", CgroupLimits::with_pids_max(2)).expect("create leaf");
    // A fake procs that never empties ⇒ the bounded poll terminates with an
    // honest `false` (never a hang, never a pretend-empty).
    fs::write(leaf.dir().expect("dir").join(PROCS_FILE), "999\n").expect("persistent member");
    assert!(!leaf
        .wait_until_empty(2, Duration::from_millis(1))
        .expect("wait"));
}

#[test]
fn remove_then_drop_does_not_double_remove() {
    // A leaf with NO limits, so the fake leaf dir is empty — mirroring a REAL
    // cgroup leaf, whose kernel interface files (pids.max, cgroup.kill, …) do
    // NOT block `rmdir` of a member-less leaf (verified on the live delegated
    // box: `mkdir` a leaf, then `rmdir` succeeds despite its interface files).
    // The fake tree can't reproduce kernel pseudo-files that rmdir ignores, so
    // it uses a limit-free (genuinely empty) leaf to exercise the same rmdir.
    let (_tmp, base) = fake_base("pids");
    let mut leaf = CgroupLeaf::create(&base, "leaf", CgroupLimits::default()).expect("create leaf");
    let dir = leaf.dir().expect("dir").to_path_buf();
    leaf.remove().expect("explicit remove");
    assert!(!dir.exists(), "remove rmdir'd the leaf");
    // dir() now errors (leaf gone) and Drop is a no-op (no panic / double-rmdir).
    assert_eq!(
        leaf.dir().expect_err("dir after remove").kind(),
        io::ErrorKind::NotFound
    );
    drop(leaf);
}

#[test]
fn invalid_leaf_name_is_rejected() {
    let (_tmp, base) = fake_base("pids");
    for bad in ["", "a/b"] {
        let err = CgroupLeaf::create(&base, bad, CgroupLimits::default())
            .expect_err("invalid name must error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
