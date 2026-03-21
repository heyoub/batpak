use super::{Outcome, OutcomeError};
use crate::outcome::error::ErrorKind;

/// zip: combine two outcomes into a tuple outcome.
/// If either is Err, the first Err wins.
/// [SPEC:src/outcome/combine.rs]
pub fn zip<A: Clone, B: Clone>(a: Outcome<A>, b: Outcome<B>) -> Outcome<(A, B)> {
    // Priority order for non-Ok variants (highest wins):
    //   Err > Cancelled > Retry > Pending > Batch > Ok
    // When both are non-Ok, the FIRST (a) argument's variant wins at equal priority.
    match (a, b) {
        // Both Ok → combine
        (Outcome::Ok(a), Outcome::Ok(b)) => Outcome::Ok((a, b)),

        // Either Err → first Err wins
        (Outcome::Err(e), _) | (_, Outcome::Err(e)) => Outcome::Err(e),

        // Either Cancelled → first Cancelled wins
        (Outcome::Cancelled { reason }, _) | (_, Outcome::Cancelled { reason }) => {
            Outcome::Cancelled { reason }
        }

        // Either Retry → first Retry wins
        (
            Outcome::Retry {
                after_ms,
                attempt,
                max_attempts,
                reason,
            },
            _,
        )
        | (
            _,
            Outcome::Retry {
                after_ms,
                attempt,
                max_attempts,
                reason,
            },
        ) => Outcome::Retry {
            after_ms,
            attempt,
            max_attempts,
            reason,
        },

        // Either Pending → first Pending wins
        (
            Outcome::Pending {
                condition,
                resume_token,
            },
            _,
        )
        | (
            _,
            Outcome::Pending {
                condition,
                resume_token,
            },
        ) => Outcome::Pending {
            condition,
            resume_token,
        },

        // Both Batch → zip elements pairwise (truncate to shorter)
        (Outcome::Batch(a_items), Outcome::Batch(b_items)) => Outcome::Batch(
            a_items
                .into_iter()
                .zip(b_items)
                .map(|(a, b)| zip(a, b))
                .collect(),
        ),

        // One Batch, one Ok → map the Ok into each Batch element
        (Outcome::Batch(items), Outcome::Ok(b)) => Outcome::Batch(
            items
                .into_iter()
                .map(|a| zip(a, Outcome::Ok(b.clone())))
                .collect(),
        ),
        (Outcome::Ok(a), Outcome::Batch(items)) => Outcome::Batch(
            items
                .into_iter()
                .map(|b| zip(Outcome::Ok(a.clone()), b))
                .collect(),
        ),
    }
}
// A: Clone and B: Clone required for the Batch+Ok distribution cases above.

/// join_all: collect a Vec of outcomes into an outcome of Vec.
/// All must be Ok for the result to be Ok. First Err short-circuits.
/// [SPEC:src/outcome/combine.rs]
pub fn join_all<T>(outcomes: Vec<Outcome<T>>) -> Outcome<Vec<T>> {
    let mut results = Vec::with_capacity(outcomes.len());
    for outcome in outcomes {
        match outcome {
            Outcome::Ok(v) => results.push(v),
            Outcome::Err(e) => return Outcome::Err(e),
            Outcome::Cancelled { reason } => return Outcome::Cancelled { reason },
            Outcome::Retry {
                after_ms,
                attempt,
                max_attempts,
                reason,
            } => {
                return Outcome::Retry {
                    after_ms,
                    attempt,
                    max_attempts,
                    reason,
                };
            }
            Outcome::Pending {
                condition,
                resume_token,
            } => {
                return Outcome::Pending {
                    condition,
                    resume_token,
                };
            }
            Outcome::Batch(inner) => {
                // Flatten: join_all on the inner batch, then continue collecting.
                match join_all(inner) {
                    Outcome::Ok(vs) => results.extend(vs),
                    Outcome::Err(e) => return Outcome::Err(e),
                    Outcome::Cancelled { reason } => return Outcome::Cancelled { reason },
                    Outcome::Retry {
                        after_ms,
                        attempt,
                        max_attempts,
                        reason,
                    } => {
                        return Outcome::Retry {
                            after_ms,
                            attempt,
                            max_attempts,
                            reason,
                        };
                    }
                    Outcome::Pending {
                        condition,
                        resume_token,
                    } => {
                        return Outcome::Pending {
                            condition,
                            resume_token,
                        };
                    }
                    Outcome::Batch(vs) => {
                        // Nested batch from recursive join_all — extend results
                        for item in vs {
                            if let Outcome::Ok(v) = item {
                                results.extend(v);
                            }
                        }
                    }
                }
            }
        }
    }
    Outcome::Ok(results)
}

/// join_any: first Ok wins. If all fail, last Err wins.
/// [SPEC:src/outcome/combine.rs]
pub fn join_any<T>(outcomes: Vec<Outcome<T>>) -> Outcome<T> {
    let mut last_err = None;
    for outcome in outcomes {
        match outcome {
            Outcome::Ok(v) => return Outcome::Ok(v),
            Outcome::Err(e) => last_err = Some(e),
            other => return other, // Retry/Pending/Cancelled propagate immediately
        }
    }
    match last_err {
        Some(e) => Outcome::Err(e),
        None => Outcome::Err(OutcomeError {
            kind: ErrorKind::Internal,
            message: "join_any called with empty vec".into(),
            compensation: None,
            retryable: false,
        }),
    }
}
