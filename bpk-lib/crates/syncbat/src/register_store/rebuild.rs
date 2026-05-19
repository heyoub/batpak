//! Replay catalog rows into a syncbat register.

use std::collections::BTreeMap;

use batpak::coordinate::Coordinate;
use batpak::coordinate::{KindFilter, Region};
use batpak::event::DecodeTyped;
use batpak::store::Store;

use crate::register::Register;

use super::error::StoreRegisterCatalogError;
use super::row::{RegisterOperationActionV1, RegisterOperationRowV1, SYNCBAT_REGISTER_EVENT_KIND};
use super::{validate_catalog_name, CatalogEntryState, TombstoneState};

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

pub(super) fn fold_catalog_entries<State>(
    store: &Store<State>,
    coordinate: &Coordinate,
) -> Result<BTreeMap<String, CatalogEntryState>, StoreRegisterCatalogError> {
    let region = Region::entity(coordinate.entity())
        .with_scope(coordinate.scope())
        .with_fact(KindFilter::Exact(SYNCBAT_REGISTER_EVENT_KIND));
    let mut hits = store.query(&region);
    hits.retain(|hit| hit.coord() == coordinate);
    hits.sort_by(|left, right| {
        left.global_sequence()
            .cmp(&right.global_sequence())
            .then_with(|| left.event_id().cmp(&right.event_id()))
    });

    let mut entries = BTreeMap::<String, CatalogEntryState>::new();
    for hit in hits {
        let stored = store.get(hit.event_id())?;
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
