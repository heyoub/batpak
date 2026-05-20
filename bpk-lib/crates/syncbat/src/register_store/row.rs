//! Batpak-backed durable register catalog rows.

use batpak::event::{EventKind, EventPayload};
use serde::{Deserialize, Serialize};

use crate::operation::{EffectClass, OperationDescriptor};

use super::error::StoreRegisterCatalogError;

/// Batpak custom event kind used for syncbat register catalog rows.
pub const SYNCBAT_REGISTER_EVENT_KIND: EventKind = EventKind::custom(0xC, 0x5B8);

pub(super) const REGISTER_SCHEMA_VERSION: u16 = 1;
pub(super) const REGISTER_ACTION_PUT: &str = "put";
pub(super) const REGISTER_ACTION_UPDATE: &str = "update";
pub(super) const REGISTER_ACTION_DELETE: &str = "delete";
pub(super) const REGISTER_ACTION_SUPERSEDE: &str = "supersede";

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

    pub(super) fn action_kind(
        &self,
    ) -> Result<RegisterOperationActionV1, StoreRegisterCatalogError> {
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

    pub(super) fn into_descriptor(self) -> Result<OperationDescriptor, StoreRegisterCatalogError> {
        self.descriptor()
    }

    pub(super) fn descriptor(&self) -> Result<OperationDescriptor, StoreRegisterCatalogError> {
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

    pub(super) fn descriptor_payload_is_empty(&self) -> bool {
        self.title.is_none()
            && self.effect.is_empty()
            && self.input_schema_ref.is_empty()
            && self.output_schema_ref.is_empty()
            && self.receipt_kind.is_empty()
    }
}
