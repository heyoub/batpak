//! Single validating constructor for syncbat operation names.
//!
//! [`OperationName`] is the substrate-wide newtype that owns the operation-name
//! grammar. Every other validator at any layer (operation-descriptor catalog
//! insertion, module/register checks, netbat route boundary, netbat wire-frame
//! decode, TS client) MUST reach for this type instead of re-coding the
//! grammar.
//!
//! Grammar:
//!
//! - non-empty
//! - <= [`OperationName::MAX_BYTES`] bytes
//! - ASCII letters/digits and the three punctuation bytes `.`, `_`, `-`
//! - no leading or trailing `.`
//! - no `..` substring
//!
//! The constructor is validating; downstream code never re-parses the grammar.

use std::sync::Arc;

use crate::operation::MAX_OPERATION_NAME_BYTES;

/// Stable operation name. Validated once at construction; downstream
/// code never re-parses the grammar.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct OperationName(Arc<str>);

/// Operation-name grammar violation surfaced by [`OperationName::new`].
///
/// `#[non_exhaustive]` so post-1.0 we can add finer-grained variants
/// (e.g. distinguishing high-bit-set from control-byte rejections)
/// without breaking downstream exhaustive `match` arms.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OperationNameError {
    /// Operation name was empty.
    Empty,
    /// Operation name exceeded the type-level byte bound.
    TooLong {
        /// Observed byte length of the rejected input.
        len: usize,
        /// Type-level maximum, equal to [`OperationName::MAX_BYTES`].
        max: usize,
    },
    /// Operation name started or ended with `.`.
    LeadingOrTrailingDot,
    /// Operation name contained the substring `..`.
    ConsecutiveDots,
    /// Operation name contained a byte outside the ASCII grammar.
    IllegalCharacter {
        /// First illegal byte encountered.
        byte: u8,
    },
}

impl std::fmt::Display for OperationNameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("operation name is empty"),
            Self::TooLong { len, max } => {
                write!(f, "operation name is {len} bytes (max {max})")
            }
            Self::LeadingOrTrailingDot => {
                f.write_str("operation name must not start or end with '.'")
            }
            Self::ConsecutiveDots => f.write_str("operation name must not contain '..'"),
            Self::IllegalCharacter { byte } => write!(
                f,
                "operation name contains illegal byte 0x{byte:02x} (allowed: [A-Za-z0-9._-])"
            ),
        }
    }
}

impl std::error::Error for OperationNameError {}

impl OperationName {
    /// Maximum bytes accepted for an operation name.
    pub const MAX_BYTES: usize = MAX_OPERATION_NAME_BYTES;

    /// Validate `value` against the operation-name grammar and construct an
    /// [`OperationName`].
    ///
    /// # Errors
    /// Returns [`OperationNameError`] when the input violates any rule of the
    /// operation-name grammar.
    pub fn new(value: impl Into<Arc<str>>) -> Result<Self, OperationNameError> {
        let value = value.into();
        validate(value.as_ref())?;
        Ok(Self(value))
    }

    /// Borrow the validated operation name as `&str`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for OperationName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for OperationName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn validate(value: &str) -> Result<(), OperationNameError> {
    if value.is_empty() {
        return Err(OperationNameError::Empty);
    }
    let len = value.len();
    if len > OperationName::MAX_BYTES {
        return Err(OperationNameError::TooLong {
            len,
            max: OperationName::MAX_BYTES,
        });
    }
    if let Some(&byte) = value.as_bytes().iter().find(|byte| {
        !matches!(
            **byte,
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b'-'
        )
    }) {
        return Err(OperationNameError::IllegalCharacter { byte });
    }
    if value.starts_with('.') || value.ends_with('.') {
        return Err(OperationNameError::LeadingOrTrailingDot);
    }
    if value.contains("..") {
        return Err(OperationNameError::ConsecutiveDots);
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn accepts_canonical_names() {
        for name in [
            "system.heartbeat",
            "bank.commit",
            "event.get",
            "a-b_c.d",
            "ping",
            "0",
            "a",
        ] {
            let op = OperationName::new(name).unwrap_or_else(|e| panic!("{name:?}: {e:?}"));
            assert_eq!(op.as_str(), name);
        }
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(OperationName::new(""), Err(OperationNameError::Empty));
    }

    #[test]
    fn rejects_too_long() {
        let overlong = "a".repeat(OperationName::MAX_BYTES + 1);
        let err = OperationName::new(overlong.clone()).expect_err("too long");
        assert_eq!(
            err,
            OperationNameError::TooLong {
                len: OperationName::MAX_BYTES + 1,
                max: OperationName::MAX_BYTES,
            }
        );
    }

    #[test]
    fn accepts_exact_length_boundary() {
        let exact = "a".repeat(OperationName::MAX_BYTES);
        let op = OperationName::new(exact.clone()).expect("exact length is valid");
        assert_eq!(op.as_str(), exact);
    }

    #[test]
    fn rejects_leading_or_trailing_dot() {
        for name in [".x", "x.", ".", ".a.b", "a.b."] {
            assert_eq!(
                OperationName::new(name),
                Err(OperationNameError::LeadingOrTrailingDot),
                "{name:?}",
            );
        }
    }

    #[test]
    fn rejects_consecutive_dots() {
        assert_eq!(
            OperationName::new("a..b"),
            Err(OperationNameError::ConsecutiveDots),
        );
        assert_eq!(
            OperationName::new("foo..bar..baz"),
            Err(OperationNameError::ConsecutiveDots),
        );
    }

    #[test]
    fn rejects_illegal_characters() {
        for (name, byte) in [
            ("a b", b' '),
            ("a/b", b'/'),
            ("a:b", b':'),
            ("a@b", b'@'),
            ("a$b", b'$'),
            ("a\tb", b'\t'),
        ] {
            assert_eq!(
                OperationName::new(name),
                Err(OperationNameError::IllegalCharacter { byte }),
                "{name:?}",
            );
        }
    }

    #[test]
    fn rejects_non_ascii() {
        let name = "café";
        let err = OperationName::new(name).expect_err("non-ascii rejected");
        let OperationNameError::IllegalCharacter { byte } = err else {
            panic!("expected illegal-character variant, got {err:?}");
        };
        assert!(byte >= 0x80);
    }

    #[test]
    fn accepts_all_numeric() {
        let op = OperationName::new("0123456789").expect("digits are valid");
        assert_eq!(op.as_str(), "0123456789");
    }

    #[test]
    fn accepts_punctuation_only_names_when_not_dot_only() {
        OperationName::new("-").expect("hyphen alone is valid");
        OperationName::new("_").expect("underscore alone is valid");
        // A bare "." is leading-and-trailing dot -> rejected.
        assert_eq!(
            OperationName::new("."),
            Err(OperationNameError::LeadingOrTrailingDot),
        );
    }

    #[test]
    fn display_round_trips_value() {
        let op = OperationName::new("system.heartbeat").unwrap();
        assert_eq!(format!("{op}"), "system.heartbeat");
    }

    #[test]
    fn error_display_is_human_readable() {
        assert!(OperationNameError::Empty.to_string().contains("empty"));
        assert!(OperationNameError::TooLong { len: 200, max: 128 }
            .to_string()
            .contains("max 128"));
        assert!(OperationNameError::LeadingOrTrailingDot
            .to_string()
            .contains("'.'"));
        assert!(OperationNameError::ConsecutiveDots
            .to_string()
            .contains("'..'"));
        assert!(OperationNameError::IllegalCharacter { byte: b'/' }
            .to_string()
            .contains("0x2f"));
    }
}
