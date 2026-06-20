use crate::store::StoreError;
use std::fs::{File, Metadata, ReadDir};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

pub(crate) fn reject_symlink_leaf(path: &Path, purpose: &str) -> Result<(), StoreError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "refusing to write {purpose} through symlink {}",
                path.display()
            ),
        ))),
        Ok(_) | Err(_) => Ok(()),
    }
}

pub(crate) fn reject_cache_symlink_leaf(path: &Path) -> Result<(), StoreError> {
    match reject_symlink_leaf(path, "cache path") {
        Ok(()) => Ok(()),
        Err(StoreError::Io(error)) => Err(StoreError::CacheFailed(Box::new(error))),
        Err(error) => Err(error),
    }
}

pub(crate) fn write_file_atomically(
    data_dir: &Path,
    final_path: &Path,
    purpose: &str,
    write: impl FnOnce(&mut File) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    reject_symlink_leaf(final_path, purpose)?;
    let tmp = named_temp_in(data_dir)?;
    let mut file = tmp.reopen().map_err(StoreError::Io)?;
    write(&mut file)?;
    file.sync_all().map_err(StoreError::Io)?;
    drop(file);
    let admission = crate::store::platform::sync::admit_current_parent_dir_sync()?;
    crate::store::platform::sync::persist_temp_with_parent_sync(tmp, final_path, admission)
        .map_err(StoreError::Io)?;
    Ok(())
}

pub(crate) fn write_derivative_file_atomically(
    data_dir: &Path,
    final_path: &Path,
    purpose: &str,
    bytes: &[u8],
) -> io::Result<()> {
    match reject_symlink_leaf(final_path, purpose) {
        Ok(()) => {}
        Err(StoreError::Io(error)) => return Err(error),
        Err(error) => return Err(io::Error::other(error.to_string())),
    }
    let tmp = named_temp_in(data_dir)?;
    {
        let mut file = io::BufWriter::new(tmp.as_file());
        file.write_all(bytes)?;
        file.into_inner().map_err(|error| error.into_error())?;
    }
    tmp.persist(final_path).map_err(|error| error.error)?;
    Ok(())
}

pub(crate) fn create_new_file(path: &Path) -> Result<File, StoreError> {
    File::create_new(path).map_err(StoreError::Io)
}

pub(crate) fn open_file(path: &Path) -> io::Result<File> {
    File::open(path)
}

pub(crate) fn read(path: &Path) -> io::Result<Vec<u8>> {
    std::fs::read(path)
}

pub(crate) fn read_dir(path: &Path) -> io::Result<ReadDir> {
    std::fs::read_dir(path)
}

pub(crate) fn create_dir_all(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)
}

pub(crate) fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    std::fs::canonicalize(path)
}

pub(crate) fn metadata(path: &Path) -> io::Result<Metadata> {
    std::fs::metadata(path)
}

pub(crate) fn remove_file(path: &Path) -> io::Result<()> {
    std::fs::remove_file(path)
}

pub(crate) fn remove_file_if_present(path: &Path) -> io::Result<bool> {
    match remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub(crate) fn remove_dir_all(path: &Path) -> io::Result<()> {
    std::fs::remove_dir_all(path)
}

pub(crate) fn remove_dir_all_if_present(path: &Path) -> io::Result<bool> {
    match remove_dir_all(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub(crate) fn named_temp_in(dir: &Path) -> io::Result<NamedTempFile> {
    NamedTempFile::new_in(dir)
}

pub(crate) fn rename(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(from, to)
}

pub(crate) fn copy(from: &Path, to: &Path) -> io::Result<u64> {
    std::fs::copy(from, to)
}

#[derive(Debug)]
pub(crate) enum PositionedReadError {
    Io(std::io::Error),
    ShortRead { bytes_read: usize },
}

pub(crate) fn read_exact_at(
    file: &mut File,
    offset: u64,
    buf: &mut [u8],
) -> Result<(), PositionedReadError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        let mut total_read = 0;
        while total_read < buf.len() {
            let n = file
                .read_at(&mut buf[total_read..], offset + total_read as u64)
                .map_err(PositionedReadError::Io)?;
            if n == 0 {
                return Err(PositionedReadError::ShortRead {
                    bytes_read: total_read,
                });
            }
            total_read = total_read.saturating_add(n);
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        use std::io::Read;
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(offset))
            .map_err(PositionedReadError::Io)?;
        let mut total_read = 0;
        while total_read < buf.len() {
            let n = file
                .read(&mut buf[total_read..])
                .map_err(PositionedReadError::Io)?;
            if n == 0 {
                return Err(PositionedReadError::ShortRead {
                    bytes_read: total_read,
                });
            }
            total_read = total_read.saturating_add(n);
        }
        Ok(())
    }
}

/// Filesystem seam for production and (future) deterministic simulation.
///
/// Boundary: this is the narrow trait through which store code reaches the
/// target filesystem. Production routes through [`RealFs`], whose every method
/// is a byte-for-byte delegate to the existing `platform::fs::*` /
/// `platform::sync::*` free functions — the only observable difference from a
/// direct free-fn call is the indirection through a trait object. A later
/// gauntlet item installs a `SimFs` that fakes the filesystem on an in-memory
/// model for deterministic crash/fault tests; it is NOT built here. This trait
/// only introduces the seam so call sites that hold a `StoreConfig` stop
/// reaching `std::fs` (via the free fns) directly.
///
/// `Send + Sync` so it can live behind `Arc<dyn StoreFs>` on `StoreConfig` and
/// be shared across threads, mirroring the [`super::spawn::Spawn`] seam.
///
/// Scope note: this is the routed subset of the platform fs/sync surface — the
/// two ops that had a `StoreConfig`-bearing, non-ratcheted call site to route
/// through in this pass: `create_dir_all` (`open_components`,
/// `WriterHandle::spawn`) and `read_dir` (`clear_snapshot_store_artifacts`).
/// The remaining ops (`metadata`, `remove_file`, `rename`, `copy`,
/// `reject_symlink_leaf`, and the durability cluster `read`, `named_temp_in`,
/// `read_exact_at`, `sync_file_all_io`, `sync_parent_dir`,
/// `persist_temp_with_parent_sync`) live behind deep `data_dir`-only free fns,
/// the config-less `Reader`, or complexity-ratcheted `lifecycle` fns with no
/// line headroom; they join this trait in the follow-up that threads
/// `&dyn StoreFs` through those signatures (and splits the ratcheted fns).
/// Until then their `RealFs` free fns remain the live path. See
/// GAUNTLET_ISSUES.md for the deferred set.
pub(crate) trait StoreFs: Send + Sync {
    /// Iterate a directory's entries. Mirrors [`std::fs::read_dir`].
    fn read_dir(&self, path: &Path) -> io::Result<ReadDir>;

    /// Create a directory and all missing parents. Mirrors
    /// [`std::fs::create_dir_all`].
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;
}

/// Production [`StoreFs`]: every method delegates to the existing
/// `platform::fs::*` free functions, so the default build behaves byte-for-byte
/// as it did before the seam was introduced.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct RealFs;

impl StoreFs for RealFs {
    fn read_dir(&self, path: &Path) -> io::Result<ReadDir> {
        read_dir(path)
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        create_dir_all(path)
    }
}

#[cfg(test)]
mod tests {
    use super::remove_dir_all;
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
        Ok(())
    }
}
