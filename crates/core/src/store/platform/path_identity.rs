//! Path-byte materialization for stable identity digests (not display strings).

use std::borrow::Cow;
use std::path::Path;

#[cfg(any(not(unix), test))]
fn normalize_non_unix_path_identity_string(raw: &str) -> Cow<'_, str> {
    if let Some(rest) = raw.strip_prefix("\\\\?\\UNC\\") {
        let mut normalized = String::with_capacity(raw.len() - 6);
        normalized.push_str("\\\\");
        normalized.push_str(rest);
        return Cow::Owned(normalized);
    }

    if let Some(rest) = raw.strip_prefix("\\\\?\\") {
        return Cow::Borrowed(rest);
    }

    Cow::Borrowed(raw)
}

/// Os-native path bytes suitable for hashing as store identity material.
///
/// On Unix this is the exact `OsStr` byte sequence. On other targets it falls
/// back to a UTF-8 lossy encoding allocation so hashing remains defined. Windows
/// verbatim prefixes are presentation details from canonicalization, so they are
/// stripped before hashing.
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
        Cow::Owned(
            normalize_non_unix_path_identity_string(&s)
                .as_bytes()
                .to_vec(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_non_unix_path_identity_string;

    #[test]
    fn non_unix_path_identity_strips_windows_verbatim_prefix() {
        assert_eq!(
            normalize_non_unix_path_identity_string("\\\\?\\batpak\\store").as_ref(),
            "batpak\\store"
        );
        assert_eq!(
            normalize_non_unix_path_identity_string("\\\\?\\UNC\\server\\share\\store").as_ref(),
            "\\\\server\\share\\store"
        );
        assert_eq!(
            normalize_non_unix_path_identity_string("batpak\\store").as_ref(),
            "batpak\\store"
        );
    }
}
