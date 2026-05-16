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
const REGISTER_ACTION_UPDATE: &str = "update";
const REGISTER_ACTION_DELETE: &str = "delete";
const REGISTER_ACTION_SUPERSEDE: &str = "supersede";

/// Stable action spelling for a syncbat register catalog row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RegisterOperationActionV1 {
    /// Insert a new operation descriptor, or repeat the same descriptor idempotently.
    Put,
    /// Replace fields for an active operation descriptor.
    Update,
    /// Remove an operation descriptor and leave a terminal tombstone.
    Delete,
    /// Tombstone one operation name and activate a replacement descriptor.
    Supersede,
}

impl RegisterOperationActionV1 {
    /// Return the stable lowercase catalog spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Put => REGISTER_ACTION_PUT,
            Self::Update => REGISTER_ACTION_UPDATE,
            Self::Delete => REGISTER_ACTION_DELETE,
            Self::Supersede => REGISTER_ACTION_SUPERSEDE,
        }
    }

    fn from_catalog_str(value: &str) -> Option<Self> {
        match value {
            REGISTER_ACTION_PUT => Some(Self::Put),
            REGISTER_ACTION_UPDATE => Some(Self::Update),
            REGISTER_ACTION_DELETE => Some(Self::Delete),
            REGISTER_ACTION_SUPERSEDE => Some(Self::Supersede),
            _ => None,
        }
    }
}

/// Durable operation-catalog row for one syncbat operation lifecycle event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RegisterOperationRowV1 {
    /// Row schema version.
    pub schema_version: u16,
    /// Stable row action spelling.
    pub action: String,
    /// Stable operation name.
    pub name: String,
    /// Operation name superseded by this descriptor for `supersede` rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
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

impl EventPayload for RegisterOperationRowV1 {
    const KIND: EventKind = SYNCBAT_REGISTER_EVENT_KIND;
}

impl RegisterOperationRowV1 {
    /// Build a durable row from an operation descriptor.
    #[must_use]
    pub fn from_descriptor(descriptor: &OperationDescriptor) -> Self {
        Self {
            schema_version: REGISTER_SCHEMA_VERSION,
            action: RegisterOperationActionV1::Put.as_str().to_owned(),
            name: descriptor.name().to_owned(),
            supersedes: None,
            title: descriptor.title().map(str::to_owned),
            effect: descriptor.effect.as_str().to_owned(),
            input_schema_ref: descriptor.input_schema_ref().to_owned(),
            output_schema_ref: descriptor.output_schema_ref().to_owned(),
            receipt_kind: descriptor.receipt_kind().to_owned(),
        }
    }

    /// Build a durable update row from an active operation descriptor.
    #[must_use]
    pub fn update(descriptor: &OperationDescriptor) -> Self {
        let mut row = Self::from_descriptor(descriptor);
        row.action = RegisterOperationActionV1::Update.as_str().to_owned();
        row
    }

    /// Build a durable tombstone row for an operation name.
    #[must_use]
    pub fn delete(name: impl Into<String>) -> Self {
        Self {
            schema_version: REGISTER_SCHEMA_VERSION,
            action: RegisterOperationActionV1::Delete.as_str().to_owned(),
            name: name.into(),
            supersedes: None,
            title: None,
            effect: String::new(),
            input_schema_ref: String::new(),
            output_schema_ref: String::new(),
            receipt_kind: String::new(),
        }
    }

    /// Build a durable supersession row from an old name to a new descriptor.
    #[must_use]
    pub fn supersede(superseded_name: impl Into<String>, descriptor: &OperationDescriptor) -> Self {
        let mut row = Self::from_descriptor(descriptor);
        row.action = RegisterOperationActionV1::Supersede.as_str().to_owned();
        row.supersedes = Some(superseded_name.into());
        row
    }

    fn action_kind(&self) -> Result<RegisterOperationActionV1, StoreRegisterCatalogError> {
        if self.schema_version != REGISTER_SCHEMA_VERSION {
            return Err(StoreRegisterCatalogError::InvalidSchemaVersion {
                version: self.schema_version,
            });
        }
        RegisterOperationActionV1::from_catalog_str(&self.action).ok_or_else(|| {
            StoreRegisterCatalogError::InvalidAction {
                action: self.action.clone(),
            }
        })
    }

    fn into_descriptor(self) -> Result<OperationDescriptor, StoreRegisterCatalogError> {
        self.descriptor()
    }

    fn descriptor(&self) -> Result<OperationDescriptor, StoreRegisterCatalogError> {
        let effect = EffectClass::from_catalog_str(&self.effect).ok_or_else(|| {
            StoreRegisterCatalogError::InvalidEffect {
                effect: self.effect.clone(),
            }
        })?;
        let mut descriptor = OperationDescriptor::owned(
            self.name.clone(),
            effect,
            self.input_schema_ref.clone(),
            self.output_schema_ref.clone(),
            self.receipt_kind.clone(),
        );
        if let Some(title) = &self.title {
            descriptor = descriptor.with_owned_title(title.clone());
        }
        descriptor
            .validate()
            .map_err(StoreRegisterCatalogError::InvalidDescriptor)?;
        Ok(descriptor)
    }

    fn descriptor_payload_is_empty(&self) -> bool {
        self.title.is_none()
            && self.effect.is_empty()
            && self.input_schema_ref.is_empty()
            && self.output_schema_ref.is_empty()
            && self.receipt_kind.is_empty()
    }
}

/// Backward-compatible alias for the original put-only row name.
///
/// New code should use [`RegisterOperationRowV1`], which reflects that the v1
/// payload now carries put, update, delete, and supersede lifecycle actions.
pub type RegisterOperationPutV1 = RegisterOperationRowV1;

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
    /// Catalog row is not well-formed for its declared lifecycle action.
    InvalidLifecycleRow {
        /// Operation name carried by the malformed row.
        name: String,
        /// Stable action spelling.
        action: String,
        /// Stable conflict explanation.
        reason: &'static str,
    },
    /// Rebuilt catalog contains an invalid lifecycle transition.
    CatalogConflict {
        /// Conflicting operation name.
        name: String,
        /// Stable action spelling.
        action: String,
        /// Stable conflict explanation.
        reason: &'static str,
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
            Self::InvalidLifecycleRow {
                name,
                action,
                reason,
            } => {
                write!(
                    f,
                    "invalid register catalog `{action}` row for `{name}`: {reason}"
                )
            }
            Self::CatalogConflict {
                name,
                action,
                reason,
            } => {
                write!(
                    f,
                    "conflicting register catalog `{action}` transition for `{name}`: {reason}"
                )
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
            | Self::InvalidLifecycleRow { .. }
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

fn validate_catalog_name(name: &str) -> Result<(), StoreRegisterCatalogError> {
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
enum CatalogEntryState {
    Active(OperationDescriptor),
    Tombstoned(TombstoneState),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TombstoneState {
    Deleted,
    Superseded { replacement: OperationDescriptor },
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
    let entries = fold_catalog_entries(store, coordinate)?;

    Register::from_operations(entries.into_values().filter_map(|state| match state {
        CatalogEntryState::Active(descriptor) => Some(descriptor),
        CatalogEntryState::Tombstoned(_) => None,
    }))
    .map_err(StoreRegisterCatalogError::Register)
}

fn fold_catalog_entries<State>(
    store: &Store<State>,
    coordinate: &Coordinate,
) -> Result<BTreeMap<String, CatalogEntryState>, StoreRegisterCatalogError> {
    let region = Region::entity(coordinate.entity())
        .with_scope(coordinate.scope())
        .with_fact(KindFilter::Exact(SYNCBAT_REGISTER_EVENT_KIND));
    let mut hits = store.query(&region);
    hits.retain(|hit| hit.coord == *coordinate);
    hits.sort_by_key(|hit| hit.global_sequence);

    let mut entries = BTreeMap::<String, CatalogEntryState>::new();
    for hit in hits {
        let stored = store.get(hit.event_id)?;
        let row = stored.event.decode_typed::<RegisterOperationRowV1>()?;
        let action = row.action_kind()?;
        match action {
            RegisterOperationActionV1::Put => {
                if row.supersedes.is_some() {
                    return Err(StoreRegisterCatalogError::InvalidLifecycleRow {
                        name: row.name,
                        action: action.as_str().to_owned(),
                        reason: "put rows cannot carry a supersedes name",
                    });
                }
                let descriptor = row.into_descriptor()?;
                let name = descriptor.name().to_owned();
                match entries.get(&name) {
                    Some(CatalogEntryState::Tombstoned(_)) => {
                        return Err(StoreRegisterCatalogError::CatalogConflict {
                            name,
                            action: action.as_str().to_owned(),
                            reason: "cannot put a name after it has been tombstoned",
                        });
                    }
                    Some(CatalogEntryState::Active(existing)) if existing == &descriptor => {
                        continue;
                    }
                    Some(CatalogEntryState::Active(_)) => {
                        return Err(StoreRegisterCatalogError::CatalogConflict {
                            name,
                            action: action.as_str().to_owned(),
                            reason: "put cannot replace an active descriptor; use update",
                        });
                    }
                    None => {
                        entries.insert(name, CatalogEntryState::Active(descriptor));
                    }
                }
            }
            RegisterOperationActionV1::Update => {
                if row.supersedes.is_some() {
                    return Err(StoreRegisterCatalogError::InvalidLifecycleRow {
                        name: row.name,
                        action: action.as_str().to_owned(),
                        reason: "update rows cannot carry a supersedes name",
                    });
                }
                let descriptor = row.into_descriptor()?;
                let name = descriptor.name().to_owned();
                match entries.get(&name) {
                    Some(CatalogEntryState::Active(_)) => {
                        entries.insert(name, CatalogEntryState::Active(descriptor));
                    }
                    Some(CatalogEntryState::Tombstoned(_)) => {
                        return Err(StoreRegisterCatalogError::CatalogConflict {
                            name,
                            action: action.as_str().to_owned(),
                            reason: "cannot update a tombstoned operation",
                        });
                    }
                    None => {
                        return Err(StoreRegisterCatalogError::CatalogConflict {
                            name,
                            action: action.as_str().to_owned(),
                            reason: "cannot update an operation before it has been put",
                        });
                    }
                }
            }
            RegisterOperationActionV1::Delete => {
                if row.supersedes.is_some() || !row.descriptor_payload_is_empty() {
                    return Err(StoreRegisterCatalogError::InvalidLifecycleRow {
                        name: row.name,
                        action: action.as_str().to_owned(),
                        reason: "delete rows must carry only the operation name",
                    });
                }
                validate_catalog_name(&row.name)?;
                match entries.get(&row.name) {
                    Some(CatalogEntryState::Active(_)) => {
                        entries.insert(
                            row.name,
                            CatalogEntryState::Tombstoned(TombstoneState::Deleted),
                        );
                    }
                    Some(CatalogEntryState::Tombstoned(TombstoneState::Deleted)) => {
                        continue;
                    }
                    Some(CatalogEntryState::Tombstoned(TombstoneState::Superseded { .. })) => {
                        return Err(StoreRegisterCatalogError::CatalogConflict {
                            name: row.name,
                            action: action.as_str().to_owned(),
                            reason: "cannot delete an operation after it has been superseded",
                        });
                    }
                    None => {
                        return Err(StoreRegisterCatalogError::CatalogConflict {
                            name: row.name,
                            action: action.as_str().to_owned(),
                            reason: "cannot delete an operation before it has been put",
                        });
                    }
                }
            }
            RegisterOperationActionV1::Supersede => {
                let superseded_name = row.supersedes.clone().ok_or_else(|| {
                    StoreRegisterCatalogError::InvalidLifecycleRow {
                        name: row.name.clone(),
                        action: action.as_str().to_owned(),
                        reason: "supersede rows must carry a supersedes name",
                    }
                })?;
                validate_catalog_name(&superseded_name)?;
                let descriptor = row.into_descriptor()?;
                let replacement_name = descriptor.name().to_owned();
                if superseded_name == replacement_name {
                    return Err(StoreRegisterCatalogError::InvalidLifecycleRow {
                        name: replacement_name,
                        action: action.as_str().to_owned(),
                        reason: "supersession target must differ from replacement name",
                    });
                }
                match entries.get(&superseded_name) {
                    Some(CatalogEntryState::Active(_)) => {}
                    Some(CatalogEntryState::Tombstoned(TombstoneState::Superseded {
                        replacement,
                    })) => match entries.get(&replacement_name) {
                        Some(CatalogEntryState::Active(existing))
                            if existing == &descriptor && replacement == &descriptor =>
                        {
                            continue;
                        }
                        _ => {
                            return Err(StoreRegisterCatalogError::CatalogConflict {
                                name: superseded_name,
                                action: action.as_str().to_owned(),
                                reason: "supersession source was already tombstoned",
                            });
                        }
                    },
                    Some(CatalogEntryState::Tombstoned(TombstoneState::Deleted)) => {
                        return Err(StoreRegisterCatalogError::CatalogConflict {
                            name: superseded_name,
                            action: action.as_str().to_owned(),
                            reason: "cannot supersede an operation after it has been deleted",
                        });
                    }
                    None => {
                        return Err(StoreRegisterCatalogError::CatalogConflict {
                            name: superseded_name,
                            action: action.as_str().to_owned(),
                            reason: "cannot supersede an operation before it has been put",
                        });
                    }
                }
                match entries.get(&replacement_name) {
                    Some(CatalogEntryState::Tombstoned(_)) => {
                        return Err(StoreRegisterCatalogError::CatalogConflict {
                            name: replacement_name,
                            action: action.as_str().to_owned(),
                            reason: "cannot supersede into a tombstoned replacement name",
                        });
                    }
                    Some(CatalogEntryState::Active(existing)) if existing != &descriptor => {
                        return Err(StoreRegisterCatalogError::CatalogConflict {
                            name: replacement_name,
                            action: action.as_str().to_owned(),
                            reason: "replacement name is already active with different fields",
                        });
                    }
                    Some(CatalogEntryState::Active(_)) | None => {}
                }
                entries.insert(
                    superseded_name,
                    CatalogEntryState::Tombstoned(TombstoneState::Superseded {
                        replacement: descriptor.clone(),
                    }),
                );
                entries.insert(replacement_name, CatalogEntryState::Active(descriptor));
            }
        }
    }

    Ok(entries)
}
