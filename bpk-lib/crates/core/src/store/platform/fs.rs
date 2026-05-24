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

#[cfg(test)]
mod tests {
    use super::remove_dir_all;
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
}
