use serde::{Deserialize, Serialize};
// NOTE: No `use crate::wire::*` needed. serde(with = "crate::wire::...") resolves via string path.

pub mod combine;
pub mod error;
pub mod wait;

pub use combine::{join_all, join_any, zip};
pub use error::{ErrorKind, OutcomeError};
pub use wait::{CompensationAction, WaitCondition};

/// `Outcome<T>`: the core algebraic type. 6 variants.
/// Named "Outcome" not "Effect" to eliminate Effect/Event confusion.
/// [SPEC:src/outcome/mod.rs]

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
#[non_exhaustive]
pub enum Outcome<T> {
    Ok(T),
    Err(OutcomeError),
    Retry {
        after_ms: u64,
        attempt: u32,
        max_attempts: u32,
        reason: String,
    },
    Pending {
        condition: WaitCondition,
        #[serde(with = "crate::wire::u128_bytes")]
        resume_token: u128,
    },
    Cancelled {
        reason: String,
    },
    Batch(Vec<Outcome<T>>),
}

// Monadic combinators intentionally use wildcard matches for pass-through patterns.
// Each combinator handles 1-2 specific variants and passes the rest unchanged.
#[allow(clippy::wildcard_enum_match_arm)]
impl<T> Outcome<T> {
    // --- Construction ---
    pub fn ok(val: T) -> Self {
        Self::Ok(val)
    }
    pub fn err(e: OutcomeError) -> Self {
        Self::Err(e)
    }
    pub fn cancelled(reason: impl Into<String>) -> Self {
        Self::Cancelled {
            reason: reason.into(),
        }
    }
    pub fn retry(
        after_ms: u64,
        attempt: u32,
        max_attempts: u32,
        reason: impl Into<String>,
    ) -> Self {
        Self::Retry {
            after_ms,
            attempt,
            max_attempts,
            reason: reason.into(),
        }
    }
    pub fn pending(condition: WaitCondition, resume_token: u128) -> Self {
        Self::Pending {
            condition,
            resume_token,
        }
    }

    // --- Predicates ---
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok(_))
    }
    pub fn is_err(&self) -> bool {
        matches!(self, Self::Err(_))
    }
    pub fn is_retry(&self) -> bool {
        matches!(self, Self::Retry { .. })
    }
    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending { .. })
    }
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled { .. })
    }
    pub fn is_batch(&self) -> bool {
        matches!(self, Self::Batch(_))
    }
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Ok(_) | Self::Err(_) | Self::Cancelled { .. })
    }

    // --- Combinators ---

    /// map: transform the Ok value. Distributes over Batch.
    /// [SPEC:src/outcome/mod.rs — combinators distribute over Batch via F: Clone]
    pub fn map<U, F: FnOnce(T) -> U + Clone>(self, f: F) -> Outcome<U> {
        match self {
            Self::Ok(v) => Outcome::Ok(f(v)),
            Self::Err(e) => Outcome::Err(e),
            Self::Retry {
                after_ms,
                attempt,
                max_attempts,
                reason,
            } => Outcome::Retry {
                after_ms,
                attempt,
                max_attempts,
                reason,
            },
            Self::Pending {
                condition,
                resume_token,
            } => Outcome::Pending {
                condition,
                resume_token,
            },
            Self::Cancelled { reason } => Outcome::Cancelled { reason },
            Self::Batch(items) => {
                Outcome::Batch(items.into_iter().map(|o| o.map(f.clone())).collect())
            }
        }
    }

    /// and_then: the monad bind. Distributes over Batch.
    /// F: Clone is required for Batch distribution (called once per element).
    /// [SPEC:src/outcome/mod.rs — The and_then monad fix]
    /// This is THE critical method. Monad laws are verified by proptest.
    /// [FILE:tests/monad_laws.rs]
    pub fn and_then<U, F: FnOnce(T) -> Outcome<U> + Clone>(self, f: F) -> Outcome<U> {
        match self {
            Self::Ok(v) => f(v),
            Self::Err(e) => Outcome::Err(e),
            Self::Retry {
                after_ms,
                attempt,
                max_attempts,
                reason,
            } => Outcome::Retry {
                after_ms,
                attempt,
                max_attempts,
                reason,
            },
            Self::Pending {
                condition,
                resume_token,
            } => Outcome::Pending {
                condition,
                resume_token,
            },
            Self::Cancelled { reason } => Outcome::Cancelled { reason },
            Self::Batch(items) => {
                Outcome::Batch(items.into_iter().map(|o| o.and_then(f.clone())).collect())
            }
        }
    }

    pub fn map_err<F: FnOnce(OutcomeError) -> OutcomeError + Clone>(self, f: F) -> Self {
        match self {
            Self::Err(e) => Self::Err(f(e)),
            Self::Batch(items) => {
                Self::Batch(items.into_iter().map(|o| o.map_err(f.clone())).collect())
            }
            other => other,
        }
    }

    pub fn or_else<F: FnOnce(OutcomeError) -> Outcome<T> + Clone>(self, f: F) -> Outcome<T> {
        match self {
            Self::Err(e) => f(e),
            Self::Batch(items) => {
                Self::Batch(items.into_iter().map(|o| o.or_else(f.clone())).collect())
            }
            other => other,
        }
    }

    pub fn inspect<F: FnOnce(&T) + Clone>(self, f: F) -> Self {
        match self {
            Self::Ok(v) => {
                f(&v);
                Self::Ok(v)
            }
            Self::Batch(items) => {
                Self::Batch(items.into_iter().map(|o| o.inspect(f.clone())).collect())
            }
            other => other,
        }
    }

    pub fn inspect_err<F: FnOnce(&OutcomeError) + Clone>(self, f: F) -> Self {
        match self {
            Self::Err(e) => {
                f(&e);
                Self::Err(e)
            }
            Self::Batch(items) => Self::Batch(
                items
                    .into_iter()
                    .map(|o| o.inspect_err(f.clone()))
                    .collect(),
            ),
            other => other,
        }
    }

    pub fn and_then_if<F: Fn(&T) -> bool, G: FnOnce(T) -> Outcome<T> + Clone>(
        self,
        pred: F,
        f: G,
    ) -> Outcome<T> {
        match self {
            Self::Ok(v) => {
                if pred(&v) {
                    f(v)
                } else {
                    Self::Ok(v)
                }
            }
            Self::Batch(items) => Self::Batch(
                items
                    .into_iter()
                    .map(|o| match o {
                        Self::Ok(v) => {
                            if pred(&v) {
                                f.clone()(v)
                            } else {
                                Self::Ok(v)
                            }
                        }
                        other => other,
                    })
                    .collect(),
            ),
            other => other,
        }
    }

    pub fn into_result(self) -> Result<T, OutcomeError> {
        match self {
            Self::Ok(v) => Ok(v),
            Self::Err(e) => Err(e),
            Self::Cancelled { reason } => Err(OutcomeError {
                kind: ErrorKind::Internal,
                message: format!("cancelled: {reason}"),
                compensation: None,
                retryable: false,
            }),
            _ => Err(OutcomeError {
                kind: ErrorKind::Internal,
                message: "outcome is not terminal".into(),
                compensation: None,
                retryable: false,
            }),
        }
    }

    pub fn unwrap_or(self, default: T) -> T {
        match self {
            Self::Ok(v) => v,
            _ => default,
        }
    }

    pub fn unwrap_or_else<F: FnOnce() -> T>(self, f: F) -> T {
        match self {
            Self::Ok(v) => v,
            _ => f(),
        }
    }
}

/// flatten: unwrap one layer of nesting. `Outcome<Outcome<T>>` → `Outcome<T>`.
/// Implemented on the nested type (like Option::flatten), not as a bounded
/// method on `Outcome<T>`. This is join in category theory: join = bind(id).
/// Composes with and_then (the monad bind, proptest-proven).
#[allow(clippy::wildcard_enum_match_arm)]
impl<T> Outcome<Outcome<T>> {
    pub fn flatten(self) -> Outcome<T> {
        self.and_then(|inner| inner)
    }
}
