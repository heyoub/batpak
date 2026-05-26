use serde::{Deserialize, Serialize};
// NOTE: No `use crate::wire::*` needed. serde(with = "crate::wire::...") resolves via string path.

/// Combinators for merging multiple outcomes (join_all, join_any, zip).
pub mod combine;
/// Structured error types used in `Outcome::Err`.
pub mod error;
/// Wait condition and compensation action types for `Outcome::Pending` and errors.
pub mod wait;

pub use combine::{join_all, join_any, zip};
pub use error::{ErrorKind, OutcomeError};
pub use wait::{CompensationAction, WaitCondition};

/// `Outcome<T>`: the core algebraic type. 6 variants.
/// Named "Outcome" not "Effect" to eliminate Effect/Event confusion.

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
#[non_exhaustive]
pub enum Outcome<T> {
    /// The operation succeeded and produced a value.
    Ok(T),
    /// The operation failed with a structured error.
    Err(OutcomeError),
    /// The operation should be retried after a delay.
    Retry {
        /// Milliseconds to wait before retrying.
        after_ms: u64,
        /// Current attempt number (1-based).
        attempt: u32,
        /// Maximum number of attempts before giving up.
        max_attempts: u32,
        /// Human-readable reason for the retry.
        reason: String,
    },
    /// The operation is waiting on an external condition to resume.
    Pending {
        /// The condition that must be satisfied before resuming.
        condition: WaitCondition,
        /// Opaque token used to correlate the resume event with this pending outcome.
        #[serde(with = "crate::wire::u128_bytes")]
        resume_token: u128,
    },
    /// The operation was explicitly cancelled.
    Cancelled {
        /// Human-readable reason for the cancellation.
        reason: String,
    },
    /// A collection of outcomes to be processed together.
    Batch(Vec<Outcome<T>>),
}

impl<T> Outcome<T> {
    // --- Construction ---
    /// Creates a successful outcome wrapping the given value.
    pub fn ok(val: T) -> Self {
        Self::Ok(val)
    }
    /// Creates a failed outcome wrapping the given error.
    pub fn err(e: OutcomeError) -> Self {
        Self::Err(e)
    }
    /// Creates a cancelled outcome with the given reason.
    pub fn cancelled(reason: impl Into<String>) -> Self {
        Self::Cancelled {
            reason: reason.into(),
        }
    }
    /// Creates a retry outcome with delay, attempt counters, and a reason.
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
    /// Creates a pending outcome that waits on the given condition and resume token.
    pub fn pending(condition: WaitCondition, resume_token: u128) -> Self {
        Self::Pending {
            condition,
            resume_token,
        }
    }

    // --- Predicates ---
    /// Returns true if this outcome is `Ok`.
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok(_))
    }
    /// Returns true if this outcome is `Err`.
    pub fn is_err(&self) -> bool {
        matches!(self, Self::Err(_))
    }
    /// Returns true if this outcome is `Retry`.
    pub fn is_retry(&self) -> bool {
        matches!(self, Self::Retry { .. })
    }
    /// Returns true if this outcome is `Pending`.
    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending { .. })
    }
    /// Returns true if this outcome is `Cancelled`.
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled { .. })
    }
    /// Returns true if this outcome is `Batch`.
    pub fn is_batch(&self) -> bool {
        matches!(self, Self::Batch(_))
    }
    /// Returns true if this outcome is terminal (`Ok`, `Err`, or `Cancelled`).
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Ok(_) | Self::Err(_) | Self::Cancelled { .. })
    }

    // --- Combinators ---

    /// map: transform the Ok value. Distributes over Batch.
    ///
    /// # Example
    /// ```
    /// use batpak::outcome::Outcome;
    ///
    /// let doubled: Outcome<i32> = Outcome::ok(21).map(|x| x * 2);
    /// assert_eq!(doubled, Outcome::ok(42));
    /// ```
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

    /// Transforms the `Err` value using `f`, leaving all other variants unchanged.
    pub fn map_err<F: FnOnce(OutcomeError) -> OutcomeError + Clone>(self, f: F) -> Self {
        match self {
            Self::Err(e) => Self::Err(f(e)),
            Self::Batch(items) => {
                Self::Batch(items.into_iter().map(|o| o.map_err(f.clone())).collect())
            }
            Self::Ok(_) | Self::Retry { .. } | Self::Pending { .. } | Self::Cancelled { .. } => {
                self
            }
        }
    }

    /// Applies `f` to recover from an `Err`, leaving all other variants unchanged.
    pub fn or_else<F: FnOnce(OutcomeError) -> Outcome<T> + Clone>(self, f: F) -> Outcome<T> {
        match self {
            Self::Err(e) => f(e),
            Self::Batch(items) => {
                Self::Batch(items.into_iter().map(|o| o.or_else(f.clone())).collect())
            }
            Self::Ok(_) | Self::Retry { .. } | Self::Pending { .. } | Self::Cancelled { .. } => {
                self
            }
        }
    }

    /// Calls `f` with a reference to the `Ok` value for side effects, then returns self unchanged.
    pub fn inspect<F: FnOnce(&T) + Clone>(self, f: F) -> Self {
        match self {
            Self::Ok(v) => {
                f(&v);
                Self::Ok(v)
            }
            Self::Batch(items) => {
                Self::Batch(items.into_iter().map(|o| o.inspect(f.clone())).collect())
            }
            Self::Err(_) | Self::Retry { .. } | Self::Pending { .. } | Self::Cancelled { .. } => {
                self
            }
        }
    }

    /// Calls `f` with a reference to the `Err` value for side effects, then returns self unchanged.
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
            Self::Ok(v) => Self::Ok(v),
            Self::Retry {
                after_ms,
                attempt,
                max_attempts,
                reason,
            } => Self::Retry {
                after_ms,
                attempt,
                max_attempts,
                reason,
            },
            Self::Pending {
                condition,
                resume_token,
            } => Self::Pending {
                condition,
                resume_token,
            },
            Self::Cancelled { reason } => Self::Cancelled { reason },
        }
    }

    /// Applies `f` only when this is `Ok` and `pred` returns true; otherwise returns self unchanged.
    pub fn and_then_if<F: Fn(&T) -> bool + Clone, G: FnOnce(T) -> Outcome<T> + Clone>(
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
                    .map(|o| o.and_then_if(pred.clone(), f.clone()))
                    .collect(),
            ),
            Self::Err(error) => Self::Err(error),
            Self::Retry {
                after_ms,
                attempt,
                max_attempts,
                reason,
            } => Self::Retry {
                after_ms,
                attempt,
                max_attempts,
                reason,
            },
            Self::Pending {
                condition,
                resume_token,
            } => Self::Pending {
                condition,
                resume_token,
            },
            Self::Cancelled { reason } => Self::Cancelled { reason },
        }
    }

    /// Converts this outcome into a `Result`.
    ///
    /// # Errors
    /// Returns an [`OutcomeError`] describing why this outcome did not resolve
    /// to an `Ok` value.
    pub fn into_result(self) -> Result<T, OutcomeError> {
        match self {
            Self::Ok(v) => Ok(v),
            Self::Err(error) => Err(error),
            Self::Cancelled { reason } => Err(OutcomeError::new(
                ErrorKind::Cancelled,
                format!("cancelled: {reason}"),
            )),
            Self::Retry {
                after_ms,
                attempt,
                max_attempts,
                reason,
            } => Err(OutcomeError::new(
                // Timeout is inherently retryable per `ErrorKind::is_retryable`,
                // which is the sole source of truth (G9) — no separate
                // boolean on the error.
                ErrorKind::Timeout,
                format!(
                    "retry after {}ms (attempt {}/{}) - {}",
                    after_ms, attempt, max_attempts, reason
                ),
            )),
            Self::Pending {
                condition,
                resume_token,
            } => Err(OutcomeError::new(
                ErrorKind::Pending,
                format!(
                    "pending outcome cannot collapse into Result: {:?} (resume {:032x})",
                    condition, resume_token
                ),
            )),
            Self::Batch(items) => Err(OutcomeError::new(
                ErrorKind::BatchCollapse,
                format!(
                    "batch outcome cannot collapse into Result without dropping {} item(s)",
                    items.len()
                ),
            )),
        }
    }

    /// Returns the `Ok` value or `default` if this outcome is not `Ok`.
    pub fn unwrap_or(self, default: T) -> T {
        match self {
            Self::Ok(v) => v,
            Self::Err(_)
            | Self::Retry { .. }
            | Self::Pending { .. }
            | Self::Cancelled { .. }
            | Self::Batch(_) => default,
        }
    }

    /// Returns the `Ok` value or computes a default from `f` if this outcome is not `Ok`.
    pub fn unwrap_or_else<F: FnOnce() -> T>(self, f: F) -> T {
        match self {
            Self::Ok(v) => v,
            Self::Err(_)
            | Self::Retry { .. }
            | Self::Pending { .. }
            | Self::Cancelled { .. }
            | Self::Batch(_) => f(),
        }
    }
}

/// flatten: unwrap one layer of nesting. `Outcome<Outcome<T>>` → `Outcome<T>`.
/// Implemented on the nested type (like Option::flatten), not as a bounded
/// method on `Outcome<T>`. This is join in category theory: join = bind(id).
/// Composes with and_then (the monad bind, proptest-proven).
impl<T> Outcome<Outcome<T>> {
    /// Unwraps one layer of nesting, equivalent to `and_then(|inner| inner)`.
    pub fn flatten(self) -> Outcome<T> {
        self.and_then(|inner| inner)
    }
}
