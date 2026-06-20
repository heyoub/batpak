use crate::store::StoreError;
use std::fs::{File, Metadata, ReadDir};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CowStrategyUsed {
    Reflink,
    Hardlink,
    DeepCopy,
}

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

pub(crate) fn symlink_metadata(path: &Path) -> io::Result<Metadata> {
    std::fs::symlink_metadata(path)
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

pub(crate) fn hard_link(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::hard_link(from, to)
}

pub(crate) fn reflink(from: &Path, to: &Path) -> io::Result<()> {
    reject_copy_source(from)?;
    remove_file_if_present(to)?;
    reflink_impl(from, to).inspect_err(|_| {
        drop(remove_file_if_present(to));
    })
}

pub(crate) fn cow_copy_file(
    from: &Path,
    to: &Path,
    use_reflink: bool,
    use_hardlink: bool,
) -> io::Result<CowStrategyUsed> {
    reject_copy_source(from)?;
    remove_file_if_present(to)?;

    if use_reflink {
        match reflink(from, to) {
            Ok(()) => return Ok(CowStrategyUsed::Reflink),
            Err(error) => {
                tracing::debug!(
                    source = %from.display(),
                    destination = %to.display(),
                    error = %error,
                    "reflink failed; falling back to next fork copy rung"
                );
                remove_file_if_present(to)?;
            }
        }
    }

    if use_hardlink {
        match hard_link(from, to) {
            Ok(()) => return Ok(CowStrategyUsed::Hardlink),
            Err(error) => {
                tracing::debug!(
                    source = %from.display(),
                    destination = %to.display(),
                    error = %error,
                    "hardlink failed; falling back to deep copy"
                );
                remove_file_if_present(to)?;
            }
        }
    }

    copy(from, to)?;
    Ok(CowStrategyUsed::DeepCopy)
}

fn reject_copy_source(path: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to copy symlink source {}", path.display()),
        ));
    }
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to copy non-file source {}", path.display()),
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn reflink_impl(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    const FICLONE: libc::c_ulong = 0x4004_9409;
    let source = File::open(from)?;
    let destination = File::create_new(to)?;
    // SAFETY: `source` and `destination` are live file descriptors opened by
    // this function. `FICLONE` does not retain pointers into Rust memory; it
    // asks the kernel to clone file data from `source` into `destination`.
    let result = unsafe { libc::ioctl(destination.as_raw_fd(), FICLONE, source.as_raw_fd()) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn reflink_impl(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let from = CString::new(from.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "source path contains interior NUL",
        )
    })?;
    let to = CString::new(to.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "destination path contains interior NUL",
        )
    })?;
    // SAFETY: the C strings are NUL-terminated and live for the duration of
    // the call. `clonefile` does not retain the pointers after returning.
    let result = unsafe { libc::clonefile(from.as_ptr(), to.as_ptr(), 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn reflink_impl(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "reflink is not supported on this platform",
    ))
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

#[cfg(test)]
mod tests {
    use super::{reject_copy_source, remove_dir_all};
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
}
