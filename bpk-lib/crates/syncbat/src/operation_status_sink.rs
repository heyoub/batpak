//! Store-backed sink for durable operation-status facts.

use std::sync::Arc;
use std::{error::Error, fmt};

use batpak::coordinate::Coordinate;
use batpak::store::{AppendOptions, Open, Store, StoreError};

use crate::operation_status::OperationStatusFactV1;

const OPERATION_STATUS_SCOPE: &str = "scope:operation-status";
const OPERATION_STATUS_ENTITY_PREFIX: &str = "syncbat:operation-status:";

/// Error returned when an operation-status sink cannot record a fact.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct OperationStatusSinkError {
    message: String,
}

impl OperationStatusSinkError {
    /// Construct a sink error from a displayable message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Return the sink error message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for OperationStatusSinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for OperationStatusSinkError {}

impl From<StoreError> for OperationStatusSinkError {
    fn from(error: StoreError) -> Self {
        Self::new(error.to_string())
    }
}

/// Sink for durable operation-status facts.
pub trait OperationStatusSink: Send + Sync {
    /// Persist one operation-status fact.
    ///
    /// # Errors
    /// Returns [`OperationStatusSinkError`] when the sink rejects or fails the write.
    fn record_fact(&self, fact: &OperationStatusFactV1) -> Result<(), OperationStatusSinkError>;
}

/// Build the entity coordinate for one operation's status stream.
///
/// # Errors
/// Returns [`StoreError`] when the entity string is invalid for batpak coordinates.
pub fn operation_status_entity(
    operation: &str,
) -> Result<String, batpak::coordinate::CoordinateError> {
    let entity = format!("{OPERATION_STATUS_ENTITY_PREFIX}{operation}");
    Coordinate::new(&entity, OPERATION_STATUS_SCOPE)?;
    Ok(entity)
}

/// Batpak-backed operation-status sink writing facts to one entity per operation.
pub struct StoreOperationStatusSink {
    store: Arc<Store<Open>>,
    base_options: AppendOptions,
}

impl StoreOperationStatusSink {
    /// Construct a sink backed by one batpak store.
    #[must_use]
    pub fn new(store: Arc<Store<Open>>) -> Self {
        Self {
            store,
            base_options: AppendOptions::new(),
        }
    }

    /// Set append options used as the baseline for every status write.
    #[must_use]
    pub fn with_options(mut self, options: AppendOptions) -> Self {
        self.base_options = options;
        self
    }

    fn coordinate_for(&self, operation: &str) -> Result<Coordinate, OperationStatusSinkError> {
        let entity = operation_status_entity(operation)
            .map_err(|error| OperationStatusSinkError::new(error.to_string()))?;
        Coordinate::new(&entity, OPERATION_STATUS_SCOPE)
            .map_err(|error| OperationStatusSinkError::new(error.to_string()))
    }
}

impl OperationStatusSink for StoreOperationStatusSink {
    fn record_fact(&self, fact: &OperationStatusFactV1) -> Result<(), OperationStatusSinkError> {
        let coordinate = self.coordinate_for(&fact.operation)?;
        self.store
            .append_typed_with_options(&coordinate, fact, self.base_options.clone())
            .map_err(OperationStatusSinkError::from)
            .map(|_receipt| ())?;
        Ok(())
    }
}

/// Convenience helpers for constructing facts from checkout phases.
impl StoreOperationStatusSink {
    /// Record a started fact for one checkout attempt.
    ///
    /// # Errors
    /// Returns [`OperationStatusSinkError`] when the append fails.
    pub fn record_started(
        &self,
        operation: &str,
        receipt_kind: &str,
    ) -> Result<(), OperationStatusSinkError> {
        self.record_fact(&OperationStatusFactV1::started(operation, receipt_kind))
    }
}
