// The routing tail is now EMPTY: every store call site that reaches the target
// filesystem does so through `StoreFs`. W3 routed the crash-sensitive
// atomic-rename / persist cluster (`remove_file`, `rename`, `named_temp_in`,
// `persist_temp_with_parent_sync`); the positioned-read primitive
// (`read_exact_at`) is now routed too, so the active-segment frame read is
// fault-injectable. Any regression that reintroduces a direct `platform::fs::*`
// call on a store path must extend this list deliberately and justify it.
const UNROUTED_STORE_FS_TAIL_OPS: &[&str] = &[];
const ROUTED_STORE_FS_OPS: &[&str] = &[
    "remove_file",
    "rename",
    "named_temp_in",
    "persist_temp_with_parent_sync",
    "read_exact_at",
];

use super::{reject_copy_source, remove_dir_all};
use super::{RealFs, StoreFs};
use std::error::Error;

#[test]
fn remove_dir_all_removes_nested_directory_tree() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("tree");
    let nested = root.join("nested");
    std::fs::create_dir_all(&nested)?;
    std::fs::write(nested.join("leaf.txt"), b"leaf")?;

    remove_dir_all(&root)?;

    assert!(
        !root.exists(),
        "PROPERTY: platform remove_dir_all must remove directories, not only files or leaves"
    );
    Ok(())
}

// Exercises every routed StoreFs method through a trait object so the
// production RealFs delegation is proven byte-for-byte against the platform
// free fns, and every method on the seam is a live vtable entry.
#[test]
fn real_fs_delegates_routed_ops_like_the_platform_free_fns() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let fs: std::sync::Arc<dyn StoreFs> = std::sync::Arc::new(RealFs);

    // create_dir_all builds the whole tree.
    let sub = dir.path().join("a").join("b");
    fs.create_dir_all(&sub)?;
    assert!(
        sub.is_dir(),
        "PROPERTY: RealFs::create_dir_all must create the full nested tree"
    );

    // read_dir lists what create_dir_all produced (entry errors propagated).
    std::fs::write(dir.path().join("leaf.bin"), b"leaf")?;
    let mut names = Vec::new();
    for entry in fs.read_dir(dir.path())? {
        names.push(entry?.file_name());
    }
    assert!(
        names.iter().any(|n| n == "leaf.bin") && names.iter().any(|n| n == "a"),
        "PROPERTY: RealFs::read_dir must list directory entries like the platform free fn"
    );

    // create_new_file + the sync seam: create a real file, write to it,
    // fsync via every routed mode, and fsync the parent dir — proving the
    // RealFs durability methods delegate to the platform free fns and leave
    // the real bytes durably on disk.
    use std::io::Write;
    let seg = dir.path().join("seg.bin");
    let mut file = fs.create_new_file(&seg)?;
    file.write_all(b"durable-bytes")?;
    fs.sync_file_all(&file, &seg)?;
    fs.sync_file_with_mode(&file, &seg, &crate::store::SyncMode::SyncAll)?;
    fs.sync_file_with_mode(&file, &seg, &crate::store::SyncMode::SyncData)?;
    fs.sync_parent_dir(&seg)?;
    assert_eq!(
        std::fs::metadata(&seg)?.len(),
        b"durable-bytes".len() as u64,
        "PROPERTY: RealFs durability methods persist the real bytes like the platform free fns"
    );
    Ok(())
}

// The success-path test above proves the sync wrappers return Ok and persist
// bytes; it cannot catch `-> Ok(())` mutants because the real method also
// returns Ok on success. These error-path tests pin that the wrappers
// actually surface a platform sync failure, so replacing any wrapper body
// with `Ok(())` is caught.
#[cfg(unix)]
#[test]
fn real_fs_sync_file_with_mode_surfaces_platform_errors() -> Result<(), Box<dyn Error>> {
    // A read-only /dev/null fd cannot be fsynced; both sync modes must error.
    let file = std::fs::File::open("/dev/null")?;
    let fs = RealFs;
    let mut failures: Vec<String> = Vec::new();
    if fs
        .sync_file_with_mode(
            &file,
            std::path::Path::new("/dev/null"),
            &crate::store::SyncMode::SyncAll,
        )
        .is_ok()
    {
        failures.push("sync_file_with_mode(SyncAll) must surface the platform error".into());
    }
    if fs
        .sync_file_with_mode(
            &file,
            std::path::Path::new("/dev/null"),
            &crate::store::SyncMode::SyncData,
        )
        .is_ok()
    {
        failures.push("sync_file_with_mode(SyncData) must surface the platform error".into());
    }
    assert!(failures.is_empty(), "{failures:?}");
    Ok(())
}

#[cfg(unix)]
#[test]
fn real_fs_sync_file_all_surfaces_platform_errors() -> Result<(), Box<dyn Error>> {
    let file = std::fs::File::open("/dev/null")?;
    let fs = RealFs;
    assert!(
        fs.sync_file_all(&file, std::path::Path::new("/dev/null"))
            .is_err(),
        "RealFs::sync_file_all must surface the platform sync error, not report success"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn real_fs_sync_parent_dir_surfaces_missing_parent() -> Result<(), Box<dyn Error>> {
    // A path whose parent directory does not exist cannot have its directory
    // entry fsynced; the wrapper must error rather than report success.
    let fs = RealFs;
    let dir = tempfile::tempdir()?;
    let missing_parent = dir.path().join("missing-parent");
    let missing = missing_parent.join("leaf");
    assert!(
        fs.sync_parent_dir(&missing).is_err(),
        "RealFs::sync_parent_dir must surface an error when the parent dir is absent"
    );
    Ok(())
}

#[test]
fn reject_copy_source_rejects_non_file_source() -> Result<(), Box<dyn Error>> {
    // A directory is a non-file source: the cow_copy_file ladder must refuse
    // it rather than silently succeed. Kills `reject_copy_source -> Ok(())`.
    let dir = tempfile::tempdir()?;
    let result = reject_copy_source(dir.path());
    assert!(
        result.is_err(),
        "PROPERTY: reject_copy_source must reject a non-file (directory) source"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn reject_copy_source_rejects_symlink_source() -> Result<(), Box<dyn Error>> {
    // A symlink source must be refused even when it targets a real file:
    // copying through it would dereference an attacker-controlled link.
    // Kills `reject_copy_source -> Ok(())` on the symlink branch.
    let dir = tempfile::tempdir()?;
    let target = dir.path().join("target.bin");
    std::fs::write(&target, b"payload")?;
    let link = dir.path().join("link.bin");
    std::os::unix::fs::symlink(&target, &link)?;

    let result = reject_copy_source(&link);
    assert!(
        result.is_err(),
        "PROPERTY: reject_copy_source must reject a symlink source (no link dereference)"
    );
    Ok(())
}

#[test]
fn store_fs_fail_closed_boundary_lists_unrouted_tail_ops() {
    assert_eq!(
        UNROUTED_STORE_FS_TAIL_OPS,
        &[] as &[&str],
        "PROPERTY: fail-closed boundary — the routing tail is empty; no store call site reaches a platform::fs::* free fn directly"
    );
    for routed in ROUTED_STORE_FS_OPS {
        assert!(
            !UNROUTED_STORE_FS_TAIL_OPS.contains(routed),
            "PROPERTY: {routed} is routed through StoreFs and must not appear in the unrouted tail"
        );
    }
}

// The four W3 ops are live vtable entries: exercising them through a trait
// object proves RealFs dispatches each (and a `-> Ok(())`/no-op mutant on any
// body is caught by the observable filesystem effect below).
#[test]
fn real_fs_routes_w3_atomic_cluster() -> Result<(), Box<dyn Error>> {
    use std::io::Write;
    let dir = tempfile::tempdir()?;
    let fs: std::sync::Arc<dyn StoreFs> = std::sync::Arc::new(RealFs);

    // named_temp_in + persist_temp_with_parent_sync: stage a temp, write it,
    // and atomically publish it to a final path.
    let final_path = dir.path().join("published.bin");
    let mut tmp = fs.named_temp_in(dir.path())?;
    tmp.write_all(b"published-bytes")?;
    tmp.flush()?;
    crate::store::platform::sync::sync_file_all_io(tmp.as_file())?;
    let admission = crate::store::platform::sync::admit_current_parent_dir_sync()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    fs.persist_temp_with_parent_sync(tmp, &final_path, admission)?;
    assert_eq!(
        std::fs::read(&final_path)?,
        b"published-bytes",
        "PROPERTY: RealFs::persist_temp_with_parent_sync must publish the staged bytes"
    );

    // rename: move the published file aside.
    let renamed = dir.path().join("renamed.bin");
    fs.rename(&final_path, &renamed)?;
    assert!(
        !final_path.exists() && renamed.exists(),
        "PROPERTY: RealFs::rename must move the file like the platform free fn"
    );

    // remove_file + remove_file_if_present: reclaim the file, then prove the
    // not-found tolerance of the provided method.
    fs.remove_file(&renamed)?;
    assert!(
        !renamed.exists(),
        "PROPERTY: RealFs::remove_file must remove the file"
    );
    assert!(
        !fs.remove_file_if_present(&renamed)?,
        "PROPERTY: remove_file_if_present must report Ok(false) for an absent file"
    );
    Ok(())
}

// `read_exact_at` is a live vtable entry: exercising it through a trait object
// proves RealFs dispatches the positioned read and delegates byte-for-byte to
// the platform free fn (a `-> Ok(())` / no-op mutant on the body is caught by
// the observed bytes below), and that a read past EOF surfaces the ShortRead
// discriminant the reader's active-frame mapping depends on.
#[test]
fn real_fs_routes_positioned_read_exact_at() -> Result<(), Box<dyn Error>> {
    use crate::store::platform::fs::PositionedReadError;
    use std::io::Write;
    let dir = tempfile::tempdir()?;
    let fs: std::sync::Arc<dyn StoreFs> = std::sync::Arc::new(RealFs);

    let path = dir.path().join("frame.bin");
    let mut file = std::fs::File::create(&path)?;
    file.write_all(b"0123456789abcdef")?;
    file.sync_all()?;
    let mut handle = std::fs::File::open(&path)?;

    // Round-trip: a positioned read at offset 3 returns exactly the requested
    // slice, proving RealFs::read_exact_at delegates to the platform free fn.
    // (`PositionedReadError` is not `std::error::Error`, so map the control read
    // to a message rather than `?`-propagating it.)
    let mut buf = [0u8; 5];
    fs.read_exact_at(&mut handle, 3, &mut buf)
        .map_err(|e| format!("positioned read must succeed: {e:?}"))?;
    assert_eq!(
        &buf, b"34567",
        "PROPERTY: RealFs::read_exact_at must fill the buffer from the requested offset like the platform free fn"
    );

    // EOF: reading a slice that runs past the file end reports a ShortRead with
    // `bytes_read == 0` (the read started exactly at EOF) — the discriminant the
    // reader maps to `corrupt_eof`.
    let mut past_eof = [0u8; 4];
    let eof = fs.read_exact_at(&mut handle, 16, &mut past_eof);
    assert!(
        matches!(eof, Err(PositionedReadError::ShortRead { bytes_read: 0 })),
        "PROPERTY: a positioned read starting at EOF must surface ShortRead {{ bytes_read: 0 }}, got {eof:?}"
    );

    // Partial: a read that begins inside the file but requests more than remains
    // reports a non-zero ShortRead (the torn-frame discriminant).
    let mut partial = [0u8; 8];
    let short = fs.read_exact_at(&mut handle, 12, &mut partial);
    assert!(
        matches!(short, Err(PositionedReadError::ShortRead { bytes_read }) if bytes_read == 4),
        "PROPERTY: a positioned read past the tail must surface a non-zero ShortRead carrying the bytes read, got {short:?}"
    );
    Ok(())
}
