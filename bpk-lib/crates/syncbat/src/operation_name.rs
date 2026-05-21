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
mod tests {
    use super::*;

    #[test]
    fn accepts_canonical_names() -> Result<(), String> {
        for name in [
            "system.heartbeat",
            "bank.commit",
            "event.get",
            "a-b_c.d",
            "ping",
            "0",
            "a",
        ] {
            let op = OperationName::new(name).map_err(|error| format!("{name:?}: {error:?}"))?;
            check_eq(op.as_str(), name, name)?;
        }
        Ok(())
    }

    #[test]
    fn rejects_empty() -> Result<(), String> {
        check_eq(
            &OperationName::new(""),
            &Err(OperationNameError::Empty),
            "empty",
        )
    }

    #[test]
    fn rejects_too_long() -> Result<(), String> {
        let overlong = "a".repeat(OperationName::MAX_BYTES + 1);
        let err = expect_err(OperationName::new(overlong.clone()), "too long")?;
        check_eq(
            &err,
            &OperationNameError::TooLong {
                len: OperationName::MAX_BYTES + 1,
                max: OperationName::MAX_BYTES,
            },
            "too long",
        )
    }

    #[test]
    fn accepts_exact_length_boundary() -> Result<(), String> {
        let exact = "a".repeat(OperationName::MAX_BYTES);
        let op = OperationName::new(exact.clone()).map_err(|error| error.to_string())?;
        check_eq(op.as_str(), exact.as_str(), "exact length")
    }

    #[test]
    fn rejects_leading_or_trailing_dot() -> Result<(), String> {
        for name in [".x", "x.", ".", ".a.b", "a.b."] {
            check_eq(
                &OperationName::new(name),
                &Err(OperationNameError::LeadingOrTrailingDot),
                name,
            )?;
        }
        Ok(())
    }

    #[test]
    fn rejects_consecutive_dots() -> Result<(), String> {
        check_eq(
            &OperationName::new("a..b"),
            &Err(OperationNameError::ConsecutiveDots),
            "a..b",
        )?;
        check_eq(
            &OperationName::new("foo..bar..baz"),
            &Err(OperationNameError::ConsecutiveDots),
            "foo..bar..baz",
        )
    }

    #[test]
    fn rejects_illegal_characters() -> Result<(), String> {
        for (name, byte) in [
            ("a b", b' '),
            ("a/b", b'/'),
            ("a:b", b':'),
            ("a@b", b'@'),
            ("a$b", b'$'),
            ("a\tb", b'\t'),
        ] {
            check_eq(
                &OperationName::new(name),
                &Err(OperationNameError::IllegalCharacter { byte }),
                name,
            )?;
        }
        Ok(())
    }

    #[test]
    fn rejects_non_ascii() -> Result<(), String> {
        let name = "café";
        let err = expect_err(OperationName::new(name), "non-ascii rejected")?;
        match err {
            OperationNameError::IllegalCharacter { byte } if byte >= 0x80 => Ok(()),
            OperationNameError::IllegalCharacter { byte } => Err(format!(
                "expected high-bit illegal-character variant, got low byte {byte:#04x}"
            )),
            OperationNameError::Empty => Err(
                "expected high-bit illegal-character variant, got empty-name error".to_owned(),
            ),
            OperationNameError::TooLong { len, max } => Err(format!(
                "expected high-bit illegal-character variant, got too-long error len={len} max={max}"
            )),
            OperationNameError::LeadingOrTrailingDot => Err(
                "expected high-bit illegal-character variant, got leading/trailing-dot error"
                    .to_owned(),
            ),
            OperationNameError::ConsecutiveDots => Err(
                "expected high-bit illegal-character variant, got consecutive-dot error".to_owned(),
            ),
        }
    }

    #[test]
    fn accepts_all_numeric() -> Result<(), String> {
        let op = OperationName::new("0123456789").map_err(|error| error.to_string())?;
        check_eq(op.as_str(), "0123456789", "digits")
    }

    #[test]
    fn accepts_punctuation_only_names_when_not_dot_only() -> Result<(), String> {
        OperationName::new("-").map_err(|error| format!("hyphen: {error}"))?;
        OperationName::new("_").map_err(|error| format!("underscore: {error}"))?;
        // A bare "." is leading-and-trailing dot -> rejected.
        check_eq(
            &OperationName::new("."),
            &Err(OperationNameError::LeadingOrTrailingDot),
            "bare dot",
        )
    }

    #[test]
    fn display_round_trips_value() -> Result<(), String> {
        let op = OperationName::new("system.heartbeat").map_err(|error| error.to_string())?;
        let rendered = format!("{op}");
        check_eq(rendered.as_str(), "system.heartbeat", "display")
    }

    #[test]
    fn error_display_is_human_readable() -> Result<(), String> {
        check_contains(&OperationNameError::Empty.to_string(), "empty", "empty")?;
        check_contains(
            &OperationNameError::TooLong { len: 200, max: 128 }.to_string(),
            "max 128",
            "too long",
        )?;
        check_contains(
            &OperationNameError::LeadingOrTrailingDot.to_string(),
            "'.'",
            "leading/trailing dot",
        )?;
        check_contains(
            &OperationNameError::ConsecutiveDots.to_string(),
            "'..'",
            "consecutive dots",
        )?;
        check_contains(
            &OperationNameError::IllegalCharacter { byte: b'/' }.to_string(),
            "0x2f",
            "illegal character",
        )
    }

    fn expect_err<T, E>(result: Result<T, E>, label: &str) -> Result<E, String> {
        match result {
            Ok(_) => Err(format!("{label}: expected Err, got Ok")),
            Err(error) => Ok(error),
        }
    }

    fn check_eq<T: PartialEq + std::fmt::Debug + ?Sized>(
        actual: &T,
        expected: &T,
        label: &str,
    ) -> Result<(), String> {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{label}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn check_contains(haystack: &str, needle: &str, label: &str) -> Result<(), String> {
        if haystack.contains(needle) {
            Ok(())
        } else {
            Err(format!(
                "{label}: expected {haystack:?} to contain {needle:?}"
            ))
        }
    }
}
