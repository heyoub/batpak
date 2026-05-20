//! Batpak-backed durable syncbat register catalog writer.

use std::sync::Arc;

use batpak::coordinate::Coordinate;
use batpak::store::{AppendOptions, AppendReceipt, Open, Store};

use crate::operation::OperationDescriptor;
use crate::register::Register;

use super::error::StoreRegisterCatalogError;
use super::rebuild::fold_catalog_entries;
use super::row::{RegisterOperationActionV1, RegisterOperationRowV1};
use super::{validate_catalog_name, CatalogEntryState, TombstoneState};

/// Batpak-backed durable syncbat register catalog.
pub struct StoreRegisterCatalog {
    store: Arc<Store<Open>>,
    coordinate: Coordinate,
    base_options: AppendOptions,
}

impl StoreRegisterCatalog {
    /// Construct a catalog writer for one register coordinate.
    #[must_use]
    pub fn new(store: Arc<Store<Open>>, coordinate: Coordinate) -> Self {
        Self {
            store,
            coordinate,
            base_options: AppendOptions::new(),
        }
    }

    /// Set append options used as the baseline for each catalog row write.
    #[must_use]
    pub fn with_options(mut self, options: AppendOptions) -> Self {
        self.base_options = options;
        self
    }

    /// Persist one operation descriptor as a durable catalog row.
    ///
    /// # Errors
    /// Returns [`StoreRegisterCatalogError`] when descriptor validation or
    /// batpak append fails.
    pub fn persist_operation(
        &self,
        descriptor: &OperationDescriptor,
    ) -> Result<AppendReceipt, StoreRegisterCatalogError> {
        descriptor
            .validate()
            .map_err(StoreRegisterCatalogError::InvalidDescriptor)?;
        let state = fold_catalog_entries(self.store.as_ref(), &self.coordinate)?;
        let name = descriptor.name().to_owned();
        match state.get(&name) {
            Some(CatalogEntryState::Active(existing)) if existing == descriptor => {}
            Some(CatalogEntryState::Active(_)) => {
                return Err(StoreRegisterCatalogError::CatalogConflict {
                    name,
                    action: RegisterOperationActionV1::Put.as_str().to_owned(),
                    reason: "put cannot replace an active descriptor; use update_operation",
                });
            }
            Some(CatalogEntryState::Tombstoned(_)) => {
                return Err(StoreRegisterCatalogError::CatalogConflict {
                    name,
                    action: RegisterOperationActionV1::Put.as_str().to_owned(),
                    reason: "cannot put a name after it has been tombstoned",
                });
            }
            None => {}
        }
        self.append_row(&RegisterOperationRowV1::from_descriptor(descriptor))
    }

    /// Persist an explicit update row for an active operation descriptor.
    ///
    /// # Errors
    /// Returns [`StoreRegisterCatalogError`] when descriptor validation,
    /// lifecycle preflight, or batpak append fails.
    pub fn update_operation(
        &self,
        descriptor: &OperationDescriptor,
    ) -> Result<AppendReceipt, StoreRegisterCatalogError> {
        descriptor
            .validate()
            .map_err(StoreRegisterCatalogError::InvalidDescriptor)?;
        let state = fold_catalog_entries(self.store.as_ref(), &self.coordinate)?;
        let name = descriptor.name().to_owned();
        match state.get(&name) {
            Some(CatalogEntryState::Active(_)) => {}
            Some(CatalogEntryState::Tombstoned(_)) => {
                return Err(StoreRegisterCatalogError::CatalogConflict {
                    name,
                    action: RegisterOperationActionV1::Update.as_str().to_owned(),
                    reason: "cannot update a tombstoned operation",
                });
            }
            None => {
                return Err(StoreRegisterCatalogError::CatalogConflict {
                    name,
                    action: RegisterOperationActionV1::Update.as_str().to_owned(),
                    reason: "cannot update an operation before it has been put",
                });
            }
        }
        self.append_row(&RegisterOperationRowV1::update(descriptor))
    }

    /// Persist a terminal delete/tombstone row for one operation name.
    ///
    /// # Errors
    /// Returns [`StoreRegisterCatalogError`] when name validation or batpak
    /// append fails.
    pub fn delete_operation(
        &self,
        name: impl AsRef<str>,
    ) -> Result<AppendReceipt, StoreRegisterCatalogError> {
        validate_catalog_name(name.as_ref())?;
        let state = fold_catalog_entries(self.store.as_ref(), &self.coordinate)?;
        match state.get(name.as_ref()) {
            Some(CatalogEntryState::Active(_)) => {}
            Some(CatalogEntryState::Tombstoned(TombstoneState::Deleted)) => {
                return Err(StoreRegisterCatalogError::CatalogConflict {
                    name: name.as_ref().to_owned(),
                    action: RegisterOperationActionV1::Delete.as_str().to_owned(),
                    reason: "operation is already deleted",
                });
            }
            Some(CatalogEntryState::Tombstoned(TombstoneState::Superseded { .. })) => {
                return Err(StoreRegisterCatalogError::CatalogConflict {
                    name: name.as_ref().to_owned(),
                    action: RegisterOperationActionV1::Delete.as_str().to_owned(),
                    reason: "cannot delete an operation after it has been superseded",
                });
            }
            None => {
                return Err(StoreRegisterCatalogError::CatalogConflict {
                    name: name.as_ref().to_owned(),
                    action: RegisterOperationActionV1::Delete.as_str().to_owned(),
                    reason: "cannot delete an operation before it has been put",
                });
            }
        }
        self.append_row(&RegisterOperationRowV1::delete(name.as_ref()))
    }

    /// Persist a supersession row from an old operation name to a replacement.
    ///
    /// # Errors
    /// Returns [`StoreRegisterCatalogError`] when validation or batpak append
    /// fails.
    pub fn supersede_operation(
        &self,
        superseded_name: impl AsRef<str>,
        descriptor: &OperationDescriptor,
    ) -> Result<AppendReceipt, StoreRegisterCatalogError> {
        validate_catalog_name(superseded_name.as_ref())?;
        descriptor
            .validate()
            .map_err(StoreRegisterCatalogError::InvalidDescriptor)?;
        if superseded_name.as_ref() == descriptor.name() {
            return Err(StoreRegisterCatalogError::InvalidLifecycleRow {
                name: descriptor.name().to_owned(),
                action: RegisterOperationActionV1::Supersede.as_str().to_owned(),
                reason: "supersession target must differ from replacement name",
            });
        }
        let state = fold_catalog_entries(self.store.as_ref(), &self.coordinate)?;
        match state.get(superseded_name.as_ref()) {
            Some(CatalogEntryState::Active(_)) => {}
            Some(CatalogEntryState::Tombstoned(TombstoneState::Superseded { replacement }))
                if replacement == descriptor =>
            {
                return Err(StoreRegisterCatalogError::CatalogConflict {
                    name: superseded_name.as_ref().to_owned(),
                    action: RegisterOperationActionV1::Supersede.as_str().to_owned(),
                    reason: "operation has already been superseded by that descriptor",
                });
            }
            Some(CatalogEntryState::Tombstoned(TombstoneState::Superseded { .. })) => {
                return Err(StoreRegisterCatalogError::CatalogConflict {
                    name: superseded_name.as_ref().to_owned(),
                    action: RegisterOperationActionV1::Supersede.as_str().to_owned(),
                    reason: "supersession source was already tombstoned",
                });
            }
            Some(CatalogEntryState::Tombstoned(TombstoneState::Deleted)) => {
                return Err(StoreRegisterCatalogError::CatalogConflict {
                    name: superseded_name.as_ref().to_owned(),
                    action: RegisterOperationActionV1::Supersede.as_str().to_owned(),
                    reason: "cannot supersede an operation after it has been deleted",
                });
            }
            None => {
                return Err(StoreRegisterCatalogError::CatalogConflict {
                    name: superseded_name.as_ref().to_owned(),
                    action: RegisterOperationActionV1::Supersede.as_str().to_owned(),
                    reason: "cannot supersede an operation before it has been put",
                });
            }
        }
        match state.get(descriptor.name()) {
            Some(CatalogEntryState::Tombstoned(_)) => {
                return Err(StoreRegisterCatalogError::CatalogConflict {
                    name: descriptor.name().to_owned(),
                    action: RegisterOperationActionV1::Supersede.as_str().to_owned(),
                    reason: "cannot supersede into a tombstoned replacement name",
                });
            }
            Some(CatalogEntryState::Active(existing)) if existing != descriptor => {
                return Err(StoreRegisterCatalogError::CatalogConflict {
                    name: descriptor.name().to_owned(),
                    action: RegisterOperationActionV1::Supersede.as_str().to_owned(),
                    reason: "replacement name is already active with different fields",
                });
            }
            Some(CatalogEntryState::Active(_)) | None => {}
        }
        self.append_row(&RegisterOperationRowV1::supersede(
            superseded_name.as_ref(),
            descriptor,
        ))
    }

    fn append_row(
        &self,
        row: &RegisterOperationRowV1,
    ) -> Result<AppendReceipt, StoreRegisterCatalogError> {
        self.store
            .append_typed_with_options(&self.coordinate, row, self.base_options.clone())
            .map_err(StoreRegisterCatalogError::from)
    }

    /// Persist every operation in a register in deterministic order.
    ///
    /// # Errors
    /// Returns [`StoreRegisterCatalogError`] if any row cannot be appended.
    pub fn persist_register(
        &self,
        register: &Register,
    ) -> Result<Vec<AppendReceipt>, StoreRegisterCatalogError> {
        let mut receipts = Vec::with_capacity(register.len());
        for (_, descriptor) in register.descriptors() {
            receipts.push(self.persist_operation(descriptor)?);
        }
        Ok(receipts)
    }
}
