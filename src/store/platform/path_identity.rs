//! Path-byte materialization for stable identity digests (not display strings).

use std::borrow::Cow;
use std::path::Path;

/// Os-native path bytes suitable for hashing as store identity material.
///
/// On Unix this is the exact `OsStr` byte sequence. On other targets it falls
/// back to a UTF-8 lossy encoding allocation so hashing remains defined.
#[must_use]
pub(crate) fn path_bytes_for_identity_digest(path: &Path) -> Cow<'_, [u8]> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Cow::Borrowed(path.as_os_str().as_bytes())
    }
    #[cfg(not(unix))]
    {
        let s = path.to_string_lossy();
        Cow::Owned(s.as_bytes().to_vec())
    }
}
