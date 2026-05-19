//! Batpak-backed durable register catalog rows.

mod catalog;
mod error;
mod rebuild;
mod row;

pub use catalog::StoreRegisterCatalog;
pub use error::StoreRegisterCatalogError;
pub use rebuild::rebuild_register_from_store;
pub use row::{
    RegisterOperationActionV1, RegisterOperationRowV1, SYNCBAT_REGISTER_EVENT_KIND,
};

use crate::operation::{EffectClass, OperationDescriptor};

pub(super) fn validate_catalog_name(name: &str) -> Result<(), StoreRegisterCatalogError> {
    OperationDescriptor::owned(
        name.to_owned(),
        EffectClass::Inspect,
        "syncbat.lifecycle.input.v1",
        "syncbat.lifecycle.output.v1",
        "syncbat.lifecycle.receipt.v1",
    )
    .validate()
    .map_err(StoreRegisterCatalogError::InvalidDescriptor)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum CatalogEntryState {
    Active(OperationDescriptor),
    Tombstoned(TombstoneState),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum TombstoneState {
    Deleted,
    Superseded { replacement: OperationDescriptor },
}
