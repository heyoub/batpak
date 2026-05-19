//! Typed segment file identifier.
//!
//! Segment files on disk are named `{stem}.fbat`, where `{stem}` is a
//! base-10 `u64`. Before this module, that grammar was re-implemented at
//! nine independent call sites, each of which silently dropped any segment
//! whose filename failed to parse. Audit item Tier 2.7 collapses that
//! grammar into a single typed constructor and surfaces malformed
//! filenames via `tracing::warn!` so corruption on disk is never invisible.
//!
//! Construction is restricted to [`SegmentId::new`] (for ids assigned by
//! the writer) and [`SegmentId::from_filename`] / [`SegmentId::from_stem`]
//! (for ids read back from disk). All call sites that need the raw `u64`
//! call [`SegmentId::as_u64`].

use std::path::Path;

/// Typed segment file id. Wraps a `u64` parsed from the filename stem.
///
/// Construct only via [`SegmentId::from_filename`], [`SegmentId::from_stem`],
/// or [`SegmentId::new`] so the filename grammar lives in exactly one place.
///
/// `pub(crate)` because it is internal infrastructure — the public
/// `Store::*` surface still talks about segment ids as raw `u64`. Callers
/// at the boundary between disk and the public API convert via
/// [`SegmentId::as_u64`] / [`SegmentId::new`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct SegmentId(u64);

/// Reason a filename could not be parsed into a [`SegmentId`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub(crate) enum SegmentNameError {
    /// The path has no file stem (e.g. ends in `..` or is empty).
    MissingStem,
    /// The file stem is not valid UTF-8.
    NotUtf8,
    /// The file stem is the empty string (e.g. `.fbat`).
    EmptyStem,
    /// The file stem is non-empty but does not parse as a base-10 `u64`.
    NotAnInteger {
        /// The stem text that failed to parse, captured for diagnostics.
        stem: String,
    },
}

impl std::fmt::Display for SegmentNameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingStem => write!(f, "segment filename has no stem"),
            Self::NotUtf8 => write!(f, "segment filename stem is not valid UTF-8"),
            Self::EmptyStem => write!(f, "segment filename stem is empty"),
            Self::NotAnInteger { stem } => {
                write!(f, "segment filename stem {stem:?} is not a base-10 u64")
            }
        }
    }
}

impl std::error::Error for SegmentNameError {}

impl SegmentId {
    /// View the underlying `u64`. Use this at the boundary with code that
    /// has not yet been migrated to take a [`SegmentId`] directly.
    #[must_use]
    pub(crate) const fn as_u64(self) -> u64 {
        self.0
    }

    /// Parse a [`SegmentId`] from a `*.fbat` filename's stem.
    ///
    /// The path's file stem must be a non-empty base-10 `u64`. The
    /// extension itself is not inspected — callers filter by `.fbat`
    /// extension before invoking this constructor.
    ///
    /// # Errors
    ///
    /// Returns [`SegmentNameError::MissingStem`] if `path` has no stem,
    /// [`SegmentNameError::NotUtf8`] if the stem is not UTF-8,
    /// [`SegmentNameError::EmptyStem`] if the stem is empty, or
    /// [`SegmentNameError::NotAnInteger`] if the stem does not parse as
    /// a base-10 `u64`.
    pub(crate) fn from_filename(path: &Path) -> Result<Self, SegmentNameError> {
        let stem_os = path.file_stem().ok_or(SegmentNameError::MissingStem)?;
        let stem = stem_os.to_str().ok_or(SegmentNameError::NotUtf8)?;
        Self::from_stem(stem)
    }

    /// Parse a [`SegmentId`] from an already-extracted file stem string.
    ///
    /// Use this at sites that have already stripped the `.fbat`
    /// extension by hand (e.g. via `trim_end_matches`).
    ///
    /// # Errors
    ///
    /// Returns [`SegmentNameError::EmptyStem`] if `stem` is empty, or
    /// [`SegmentNameError::NotAnInteger`] if it does not parse as a
    /// base-10 `u64`.
    pub(crate) fn from_stem(stem: &str) -> Result<Self, SegmentNameError> {
        if stem.is_empty() {
            return Err(SegmentNameError::EmptyStem);
        }
        stem.parse::<u64>()
            .map(Self)
            .map_err(|_| SegmentNameError::NotAnInteger {
                stem: stem.to_string(),
            })
    }
}

impl std::fmt::Display for SegmentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn from_filename_accepts_six_digit_stem() {
        let id = SegmentId::from_filename(&PathBuf::from("data/000123.fbat"))
            .expect("six-digit stem parses");
        assert_eq!(
            id.as_u64(),
            123,
            "PROPERTY: zero-padded six-digit stem maps to the underlying u64"
        );
    }

    #[test]
    fn from_filename_accepts_unpadded_stem() {
        let id = SegmentId::from_filename(&PathBuf::from("123.fbat"))
            .expect("unpadded stem still parses");
        assert_eq!(id.as_u64(), 123);
    }

    #[test]
    fn from_filename_rejects_non_integer_stem_with_diagnostic() {
        let err = SegmentId::from_filename(&PathBuf::from("data/abc.fbat"))
            .expect_err("non-integer stem must surface");
        assert_eq!(
            err,
            SegmentNameError::NotAnInteger {
                stem: "abc".to_string()
            },
            "PROPERTY: malformed stems carry the offending text in the error so the warn log shows it"
        );
    }

    #[test]
    fn from_stem_rejects_empty_string_with_empty_stem_variant() {
        // Path::file_stem treats ".fbat" as a dotfile-name with no extension,
        // so we cannot reach the EmptyStem branch via from_filename(".fbat").
        // The EmptyStem branch is reachable via the direct from_stem entry
        // point — verified here so the variant is not dead grammar.
        let err = SegmentId::from_stem("").expect_err("empty stem must surface");
        assert_eq!(err, SegmentNameError::EmptyStem);
    }

    #[test]
    fn from_filename_treats_dot_fbat_as_non_integer_stem() {
        // Path::file_stem(".fbat") returns Some(".fbat") — the whole name,
        // because dotfiles have no extension per Rust's Path API. The
        // grammar then rejects ".fbat" as a non-base-10 stem rather than
        // as an empty stem. Pinning this so a future Path-API change is
        // surfaced rather than silently swallowed.
        let err = SegmentId::from_filename(&PathBuf::from(".fbat"))
            .expect_err("dotfile stem is not a u64");
        assert_eq!(
            err,
            SegmentNameError::NotAnInteger {
                stem: ".fbat".to_string()
            }
        );
    }

    #[test]
    fn from_filename_rejects_missing_stem() {
        // An empty path has no file_stem.
        let err = SegmentId::from_filename(&PathBuf::from(""))
            .expect_err("empty path has no stem");
        assert_eq!(err, SegmentNameError::MissingStem);
    }

    #[test]
    fn from_filename_accepts_boundary_ids() {
        for raw in [0u64, 1, u64::MAX] {
            let name = format!("{raw}.fbat");
            let id = SegmentId::from_filename(&PathBuf::from(&name))
                .unwrap_or_else(|_| panic!("boundary id {raw} must parse"));
            assert_eq!(
                id.as_u64(),
                raw,
                "PROPERTY: u64 boundary values round-trip through SegmentId::from_filename"
            );
        }
    }

    #[test]
    fn from_stem_round_trips_with_display() {
        let id = SegmentId::from_stem("42").expect("base-10 stem parses");
        let rendered = format!("{id}");
        assert_eq!(rendered, "42");
        let parsed = SegmentId::from_stem(&rendered).expect("Display output parses back");
        assert_eq!(parsed, id);
    }

    #[test]
    fn segment_name_error_implements_std_error() {
        // Compile-time check that SegmentNameError is a real std::error::Error,
        // so downstream `?` / boxing works without surprises.
        fn assert_error<E: std::error::Error>(_: &E) {}
        let err = SegmentNameError::NotAnInteger {
            stem: "x".to_string(),
        };
        assert_error(&err);
        let rendered = format!("{err}");
        assert!(
            rendered.contains("\"x\""),
            "PROPERTY: Display of NotAnInteger shows the offending stem; got {rendered:?}"
        );
    }
}
