use crate::store::StoreError;
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
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
    let tmp = NamedTempFile::new_in(data_dir)?;
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
    let tmp = NamedTempFile::new_in(data_dir)?;
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
