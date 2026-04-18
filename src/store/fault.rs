//! Fault injection framework for testing failure scenarios.
//!
//! This module provides structured fault injection for testing recovery,
//! error handling paths, and partial-write scenarios that are otherwise
//! hard to trigger in normal testing.
//!
//! # Usage
//!
//! ```rust,ignore
//! use batpak::store::fault::{FaultInjector, InjectionPoint};
//! use batpak::store::StoreError;
//!
//! struct FailAfterBegin;
//! impl FaultInjector for FailAfterBegin {
//!     fn check(&self, point: InjectionPoint) -> Option<StoreError> {
//!         if matches!(point, InjectionPoint::BatchBeginWritten { .. }) {
//!             Some(StoreError::FaultInjected(
//!                 format!("simulated failure at {point:?}"),
//!             ))
//!         } else {
//!             None
//!         }
//!     }
//! }
//!
//! config.fault_injector = Some(Arc::new(FailAfterBegin));
//! ```

use crate::store::StoreError;
use std::sync::Arc;

/// Injection points in the writer where faults can be triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectionPoint {
    /// Before writing any batch frames.
    BatchStart {
        /// Monotonic batch identifier from global sequence.
        batch_id: u64,
        /// Number of items in this batch.
        item_count: usize,
    },

    /// After writing the BEGIN marker but before any items.
    BatchBeginWritten {
        /// Monotonic batch identifier from global sequence.
        batch_id: u64,
        /// Number of items in this batch.
        item_count: usize,
    },

    /// After writing N items in a batch.
    BatchItemWritten {
        /// Monotonic batch identifier from global sequence.
        batch_id: u64,
        /// Zero-based index of the item just written.
        item_index: usize,
        /// Total number of items in the batch.
        total_items: usize,
    },

    /// After writing all items but before the COMMIT marker.
    BatchItemsComplete {
        /// Monotonic batch identifier from global sequence.
        batch_id: u64,
        /// Number of items in this batch.
        item_count: usize,
    },

    /// After writing the COMMIT marker but before fsync.
    BatchCommitWritten {
        /// Monotonic batch identifier from global sequence.
        batch_id: u64,
    },

    /// During fsync after COMMIT marker.
    BatchFsync {
        /// Monotonic batch identifier from global sequence.
        batch_id: u64,
    },

    /// After successful fsync, before index publish.
    BatchPrePublish {
        /// Monotonic batch identifier from global sequence.
        batch_id: u64,
        /// Number of items in this batch.
        item_count: usize,
    },

    /// Single event append before write.
    SingleAppendStart {
        /// Entity name for the event being appended.
        entity: &'static str,
    },

    /// Single event append after write, before fsync.
    SingleAppendWritten {
        /// Entity name for the event just written.
        entity: &'static str,
    },

    /// During segment rotation.
    SegmentRotation {
        /// Segment being sealed.
        old_segment: u64,
        /// Segment being opened.
        new_segment: u64,
    },
}

/// Trait for implementing fault injection scenarios.
///
/// Implementors inspect each [`InjectionPoint`] and return
/// `Some(StoreError)` to inject a fault, or `None` to proceed normally.
pub trait FaultInjector: Send + Sync {
    /// Inspect an injection point and optionally return an error to inject.
    ///
    /// Return `None` to let the operation proceed. Return
    /// `Some(StoreError::FaultInjected(..))` (or any `StoreError` variant)
    /// to abort the current operation with that error.
    fn check(&self, point: InjectionPoint) -> Option<StoreError>;

    /// Optional: called when injector is registered to verify configuration.
    ///
    /// # Errors
    /// Returns `Err(String)` if the injector configuration is invalid.
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

/// A fault injector that triggers at a specific sequence of points.
///
/// Useful for testing "crash at Nth operation" scenarios.
pub struct CountdownInjector {
    /// Total number of matching points to wait for before triggering.
    trigger_after: usize,
    /// Current count of matching points seen.
    current: std::sync::atomic::AtomicUsize,
    /// The point type to count, or None for any point.
    filter: Option<Box<dyn Fn(InjectionPoint) -> bool + Send + Sync>>,
    /// Action to take when triggering.
    action: CountdownAction,
}

/// Action to take when a [`CountdownInjector`] or [`ProbabilisticInjector`] triggers.
#[derive(Clone, Copy, Debug)]
pub enum CountdownAction {
    /// Return a [`StoreError::FaultInjected`] with the given message.
    Fail(&'static str),
    /// Count the point but do not inject a fault.
    Noop,
}

impl CountdownInjector {
    /// Create a new countdown injector that triggers after N occurrences.
    pub fn new(trigger_after: usize, action: CountdownAction) -> Self {
        Self {
            trigger_after,
            current: std::sync::atomic::AtomicUsize::new(0),
            filter: None,
            action,
        }
    }

    /// Add a filter so only specific points are counted.
    pub fn with_filter<F>(mut self, filter: F) -> Self
    where
        F: Fn(InjectionPoint) -> bool + Send + Sync + 'static,
    {
        self.filter = Some(Box::new(filter));
        self
    }

    /// Convenience: fail after N batch items written.
    pub fn after_batch_items(n: usize) -> Self {
        Self::new(
            n,
            CountdownAction::Fail("simulated fault during batch item write"),
        )
        .with_filter(|p| matches!(p, InjectionPoint::BatchItemWritten { .. }))
    }

    /// Convenience: fail after BEGIN marker written.
    pub fn after_batch_begin() -> Self {
        Self::new(
            1,
            CountdownAction::Fail("simulated fault after BEGIN marker"),
        )
        .with_filter(|p| matches!(p, InjectionPoint::BatchBeginWritten { .. }))
    }

    /// Convenience: fail after COMMIT marker but before fsync.
    pub fn after_commit_before_fsync() -> Self {
        Self::new(
            1,
            CountdownAction::Fail("simulated fault after COMMIT before fsync"),
        )
        .with_filter(|p| matches!(p, InjectionPoint::BatchCommitWritten { .. }))
    }
}

impl FaultInjector for CountdownInjector {
    fn check(&self, point: InjectionPoint) -> Option<StoreError> {
        let dominated = self.filter.as_ref().is_none_or(|f| f(point));
        if !dominated {
            return None;
        }

        let count = self
            .current
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if count + 1 < self.trigger_after {
            return None;
        }

        match self.action {
            CountdownAction::Fail(msg) => {
                Some(StoreError::FaultInjected(format!("{msg} at {point:?}")))
            }
            CountdownAction::Noop => {
                tracing::debug!("FaultInjector noop at {point:?}");
                None
            }
        }
    }
}

/// A fault injector that triggers based on a probability.
///
/// Useful for chaos testing scenarios.
pub struct ProbabilisticInjector {
    probability: f64,
    filter: Option<Box<dyn Fn(InjectionPoint) -> bool + Send + Sync>>,
    action: CountdownAction,
}

impl ProbabilisticInjector {
    /// Create a new probabilistic injector.
    /// probability: 0.0 to 1.0
    pub fn new(probability: f64, action: CountdownAction) -> Self {
        assert!(
            (0.0..=1.0).contains(&probability),
            "probability must be in [0.0, 1.0]"
        );
        Self {
            probability,
            filter: None,
            action,
        }
    }

    /// Add a filter so only specific points can trigger.
    pub fn with_filter<F>(mut self, filter: F) -> Self
    where
        F: Fn(InjectionPoint) -> bool + Send + Sync + 'static,
    {
        self.filter = Some(Box::new(filter));
        self
    }
}

impl FaultInjector for ProbabilisticInjector {
    fn check(&self, point: InjectionPoint) -> Option<StoreError> {
        let dominated = self.filter.as_ref().is_none_or(|f| f(point));
        if !dominated {
            return None;
        }

        let mut rng = fastrand::Rng::new();
        if rng.f64() >= self.probability {
            return None;
        }

        match self.action {
            CountdownAction::Fail(msg) => {
                Some(StoreError::FaultInjected(format!("{msg} at {point:?}")))
            }
            CountdownAction::Noop => {
                tracing::debug!("ProbabilisticInjector noop at {point:?}");
                None
            }
        }
    }
}

/// Check the injection point and propagate any injected fault as an error.
///
/// Returns `Ok(())` if no fault is injected, or `Err(StoreError)` if the
/// injector decided to inject a fault at this point.
///
/// # Errors
/// Returns the `StoreError` produced by the injector's [`FaultInjector::check`].
pub fn maybe_inject(
    point: InjectionPoint,
    injector: &Option<Arc<dyn FaultInjector>>,
) -> Result<(), StoreError> {
    if let Some(inj) = injector {
        if let Some(err) = inj.check(point) {
            return Err(err);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countdown_triggers_at_count() {
        let injector = CountdownInjector::new(3, CountdownAction::Fail("boom"));

        let point = InjectionPoint::BatchItemWritten {
            batch_id: 1,
            item_index: 0,
            total_items: 5,
        };

        assert!(injector.check(point).is_none());
        assert!(injector.check(point).is_none());
        assert!(injector.check(point).is_some()); // 3rd time triggers
    }

    #[test]
    fn countdown_noop_never_faults() {
        let injector = CountdownInjector::new(1, CountdownAction::Noop);

        let point = InjectionPoint::BatchItemWritten {
            batch_id: 1,
            item_index: 0,
            total_items: 5,
        };

        // Noop counts but never returns an error.
        assert!(injector.check(point).is_none());
        assert!(injector.check(point).is_none());
    }

    #[test]
    fn countdown_respects_filter() {
        let injector = CountdownInjector::new(1, CountdownAction::Fail("boom"))
            .with_filter(|p| matches!(p, InjectionPoint::BatchBeginWritten { .. }));

        let item_point = InjectionPoint::BatchItemWritten {
            batch_id: 1,
            item_index: 0,
            total_items: 5,
        };
        let begin_point = InjectionPoint::BatchBeginWritten {
            batch_id: 1,
            item_count: 5,
        };

        assert!(injector.check(item_point).is_none()); // filtered out
        assert!(injector.check(begin_point).is_some()); // matches filter
    }

    #[test]
    fn fault_injected_error_is_store_error() {
        let injector = CountdownInjector::new(1, CountdownAction::Fail("test fault"));
        let point = InjectionPoint::BatchStart {
            batch_id: 42,
            item_count: 3,
        };
        let err = injector.check(point).expect("should produce error");
        assert!(
            matches!(err, StoreError::FaultInjected(_)),
            "expected FaultInjected variant"
        );
    }

    #[test]
    fn probabilistic_injector_is_deterministic_at_probability_extremes() {
        let point = InjectionPoint::BatchCommitWritten { batch_id: 7 };

        let never = ProbabilisticInjector::new(0.0, CountdownAction::Fail("boom"));
        assert!(
            never.check(point).is_none(),
            "probability=0.0 must never inject a fault"
        );

        let always = ProbabilisticInjector::new(1.0, CountdownAction::Fail("boom"));
        assert!(
            matches!(always.check(point), Some(StoreError::FaultInjected(_))),
            "probability=1.0 must always inject the configured fault"
        );
    }

    #[test]
    fn maybe_inject_propagates_injector_decision() {
        let injector: Arc<dyn FaultInjector> =
            Arc::new(CountdownInjector::new(1, CountdownAction::Fail("boom")));
        let point = InjectionPoint::BatchStart {
            batch_id: 9,
            item_count: 2,
        };

        let err = maybe_inject(point, &Some(injector)).expect_err("fault should propagate");
        assert!(
            matches!(err, StoreError::FaultInjected(_)),
            "maybe_inject must return the injector-produced StoreError variant"
        );
    }
}
