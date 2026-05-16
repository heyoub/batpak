//! Batpak-backed durable register catalog rows.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::{error::Error, fmt};

use batpak::coordinate::Coordinate;
use batpak::coordinate::{KindFilter, Region};
use batpak::event::{DecodeTyped, EventKind, EventPayload, TypedDecodeError};
use batpak::store::{AppendOptions, AppendReceipt, Open, Store, StoreError};
use serde::{Deserialize, Serialize};

use crate::operation::{DescriptorValidationError, EffectClass, OperationDescriptor};
use crate::register::{Register, RegisterValidationError};

/// Batpak custom event kind used for syncbat register catalog rows.
pub const SYNCBAT_REGISTER_EVENT_KIND: EventKind = EventKind::custom(0xC, 0x5B8);

const REGISTER_SCHEMA_VERSION: u16 = 1;
const REGISTER_ACTION_PUT: &str = "put";

/// Durable operation-catalog row for one syncbat operation descriptor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RegisterOperationPutV1 {
    /// Row schema version.
    pub schema_version: u16,
    /// Row action. Currently always `put`.
    pub action: String,
    /// Stable operation name.
    pub name: String,
    /// Optional human-readable title.
    pub title: Option<String>,
    /// Stable effect class spelling.
    pub effect: String,
    /// Stable input schema reference.
    pub input_schema_ref: String,
    /// Stable output schema reference.
    pub output_schema_ref: String,
    /// Stable receipt kind.
    pub receipt_kind: String,
}

impl EventPayload for RegisterOperationPutV1 {
    const KIND: EventKind = SYNCBAT_REGISTER_EVENT_KIND;
}

impl RegisterOperationPutV1 {
    /// Build a durable row from an operation descriptor.
    #[must_use]
    pub fn from_descriptor(descriptor: &OperationDescriptor) -> Self {
        Self {
            schema_version: REGISTER_SCHEMA_VERSION,
            action: REGISTER_ACTION_PUT.to_owned(),
            name: descriptor.name().to_owned(),
            title: descriptor.title().map(str::to_owned),
            effect: descriptor.effect.as_str().to_owned(),
            input_schema_ref: descriptor.input_schema_ref().to_owned(),
            output_schema_ref: descriptor.output_schema_ref().to_owned(),
            receipt_kind: descriptor.receipt_kind().to_owned(),
        }
    }

    fn into_descriptor(self) -> Result<OperationDescriptor, StoreRegisterCatalogError> {
        if self.schema_version != REGISTER_SCHEMA_VERSION {
            return Err(StoreRegisterCatalogError::InvalidSchemaVersion {
                version: self.schema_version,
            });
        }
        if self.action != REGISTER_ACTION_PUT {
            return Err(StoreRegisterCatalogError::InvalidAction {
                action: self.action,
            });
        }
        let effect = EffectClass::from_catalog_str(&self.effect).ok_or_else(|| {
            StoreRegisterCatalogError::InvalidEffect {
                effect: self.effect.clone(),
            }
        })?;
        let mut descriptor = OperationDescriptor::owned(
            self.name,
            effect,
            self.input_schema_ref,
            self.output_schema_ref,
            self.receipt_kind,
        );
        if let Some(title) = self.title {
            descriptor = descriptor.with_owned_title(title);
        }
        descriptor
            .validate()
            .map_err(StoreRegisterCatalogError::InvalidDescriptor)?;
        Ok(descriptor)
    }
}

/// Error returned by batpak-backed syncbat register catalog operations.
#[derive(Debug)]
#[non_exhaustive]
pub enum StoreRegisterCatalogError {
    /// Batpak store operation failed.
    Store(StoreError),
    /// Stored payload could not be decoded as a register row.
    Decode(TypedDecodeError),
    /// Catalog row used an unsupported schema version.
    InvalidSchemaVersion {
        /// Unsupported schema version.
        version: u16,
    },
    /// Catalog row used an unsupported action.
    InvalidAction {
        /// Unsupported action.
        action: String,
    },
    /// Catalog row used an unsupported effect spelling.
    InvalidEffect {
        /// Unsupported effect spelling.
        effect: String,
    },
    /// Catalog row decoded but did not validate as an operation descriptor.
    InvalidDescriptor(DescriptorValidationError),
    /// Rebuilt catalog contains the same operation name with different fields.
    CatalogConflict {
        /// Conflicting operation name.
        name: String,
    },
    /// Rebuilt register rejected the catalog rows.
    Register(RegisterValidationError),
}

impl fmt::Display for StoreRegisterCatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => write!(f, "batpak register catalog operation failed: {error}"),
            Self::Decode(error) => write!(f, "register catalog row decode failed: {error}"),
            Self::InvalidSchemaVersion { version } => {
                write!(f, "unsupported register catalog schema version {version}")
            }
            Self::InvalidAction { action } => {
                write!(f, "unsupported register catalog action `{action}`")
            }
            Self::InvalidEffect { effect } => {
                write!(f, "unsupported register catalog effect `{effect}`")
            }
            Self::InvalidDescriptor(error) => {
                write!(f, "invalid register catalog descriptor: {error}")
            }
            Self::CatalogConflict { name } => {
                write!(f, "conflicting register catalog rows for `{name}`")
            }
            Self::Register(error) => write!(f, "rebuilt register rejected catalog: {error}"),
        }
    }
}

impl Error for StoreRegisterCatalogError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            Self::Decode(error) => Some(error),
            Self::InvalidDescriptor(error) => Some(error),
            Self::Register(error) => Some(error),
            Self::InvalidSchemaVersion { .. }
            | Self::InvalidAction { .. }
            | Self::InvalidEffect { .. }
            | Self::CatalogConflict { .. } => None,
        }
    }
}

impl From<StoreError> for StoreRegisterCatalogError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<TypedDecodeError> for StoreRegisterCatalogError {
    fn from(error: TypedDecodeError) -> Self {
        Self::Decode(error)
    }
}

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
        let row = RegisterOperationPutV1::from_descriptor(descriptor);
        self.store
            .append_typed_with_options(&self.coordinate, &row, self.base_options.clone())
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

/// Rebuild a syncbat register from durable catalog rows at one coordinate.
///
/// # Errors
/// Returns [`StoreRegisterCatalogError`] when a matching row cannot be read,
/// decoded, validated, or folded into a conflict-free register.
pub fn rebuild_register_from_store<State>(
    store: &Store<State>,
    coordinate: &Coordinate,
) -> Result<Register, StoreRegisterCatalogError> {
    let region = Region::entity(coordinate.entity())
        .with_scope(coordinate.scope())
        .with_fact(KindFilter::Exact(SYNCBAT_REGISTER_EVENT_KIND));
    let mut hits = store.query(&region);
    hits.retain(|hit| hit.coord == *coordinate);
    hits.sort_by_key(|hit| hit.global_sequence);

    let mut descriptors = BTreeMap::<String, OperationDescriptor>::new();
    for hit in hits {
        let stored = store.get(hit.event_id)?;
        let row = stored.event.decode_typed::<RegisterOperationPutV1>()?;
        let descriptor = row.into_descriptor()?;
        let name = descriptor.name().to_owned();
        if let Some(existing) = descriptors.get(&name) {
            if existing != &descriptor {
                return Err(StoreRegisterCatalogError::CatalogConflict { name });
            }
            continue;
        }
        descriptors.insert(name, descriptor);
    }

    Register::from_operations(descriptors.into_values())
        .map_err(StoreRegisterCatalogError::Register)
}
