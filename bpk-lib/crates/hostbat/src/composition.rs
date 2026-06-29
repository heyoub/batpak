//! The content-addressed host-composition schema manifest.
//!
//! When a host is built, hostbat aggregates every mounted module's
//! [`SchemaDescriptor`]s into one [`HostCompositionManifest`] — the Merkle-root
//! analogue of a Nix/Bazel hermetic derivation (§8). The aggregation is
//! **fail-closed**: two modules declaring the same `(SchemaId, SchemaVersion,
//! SchemaRole)` with a *different* [`CanonicalEncoding`] is a hard error
//! ([`HostError::SchemaCollision`]). The same schema declared identically by two
//! modules is fine (it deduplicates).
//!
//! The composition manifest is the Rust-side schema contract projection for a
//! mounted host. Its digest seals the schema *set*, order- and
//! mount-order-independent.
//!
//! [`SchemaDescriptor`]: crate::schema::SchemaDescriptor

use std::collections::BTreeMap;

use serde::Serialize;

use crate::error::{HostError, SchemaCollision};
use crate::identity::{canonical_digest, Digest, HostFingerprint};
use crate::schema::{SchemaDescriptor, SchemaRole};

/// Domain separator for the composition schema-manifest digest.
const COMPOSITION_DIGEST_DOMAIN: &str = "hostbat.composition.schema.v1";

/// One resolved schema entry in the composition: its identity, content hash, and
/// the module that owns it. Identical re-declarations across modules collapse to
/// the first owner seen.
#[derive(Clone, Debug)]
pub struct CompositionSchema {
    descriptor: SchemaDescriptor,
    owner_module: String,
}

impl CompositionSchema {
    /// The aggregated schema descriptor.
    #[must_use]
    pub fn descriptor(&self) -> &SchemaDescriptor {
        &self.descriptor
    }

    /// The id of the module that first declared this schema.
    #[must_use]
    pub fn owner_module(&self) -> &str {
        &self.owner_module
    }
}

/// Content-addressed aggregation of every mounted module's schema descriptors.
#[derive(Clone, Debug)]
pub struct HostCompositionManifest {
    schemas: Vec<CompositionSchema>,
    digest: HostFingerprint,
}

/// Identity-bearing canonical view of one resolved composition schema entry.
#[derive(Serialize)]
struct CompositionSchemaView<'a> {
    id: &'a str,
    version: u32,
    role: &'a str,
    encoding: Digest,
    owner_module: &'a str,
}

/// Domain-separated canonical view of the whole composition schema set.
#[derive(Serialize)]
struct CompositionView<'a> {
    domain: &'a str,
    schemas: Vec<CompositionSchemaView<'a>>,
}

/// The canonical `(id, version, role)` key two declarations must agree on.
type SchemaKey = (String, u32, SchemaRole);

/// Aggregate module schema descriptors into a content-addressed composition
/// manifest, detecting collisions.
#[derive(Default)]
pub(crate) struct CompositionSchemaBuilder {
    /// Keyed by `(id, version, role)` → (encoding, owner, descriptor).
    by_key: BTreeMap<SchemaKey, CompositionSchema>,
}

impl CompositionSchemaBuilder {
    /// Add one module's schema descriptor.
    ///
    /// # Errors
    /// [`HostError::SchemaCollision`] if a schema with the same `(id, version,
    /// role)` was already added with a *different* canonical encoding.
    pub(crate) fn add(
        &mut self,
        module: &str,
        descriptor: &SchemaDescriptor,
    ) -> Result<(), HostError> {
        let (id, version, role) = descriptor.identity_key();
        let key: SchemaKey = (id.to_owned(), version, role);
        match self.by_key.get(&key) {
            Some(existing) => {
                if existing.descriptor.encoding() != descriptor.encoding() {
                    return Err(HostError::SchemaCollision(Box::new(SchemaCollision {
                        schema: id.to_owned(),
                        version,
                        role: role.to_string(),
                        first_module: existing.owner_module.clone(),
                        first_encoding: existing.descriptor.encoding().to_hex(),
                        second_module: module.to_owned(),
                        second_encoding: descriptor.encoding().to_hex(),
                    })));
                }
                // Identical re-declaration: keep the first owner, no-op.
                Ok(())
            }
            None => {
                self.by_key.insert(
                    key,
                    CompositionSchema {
                        descriptor: descriptor.clone(),
                        owner_module: module.to_owned(),
                    },
                );
                Ok(())
            }
        }
    }

    /// Seal the aggregated schemas into a content-addressed manifest. The
    /// `BTreeMap` already holds entries in canonical `(id, version, role)` order,
    /// so the digest depends on the schema *set*, never on mount or declaration
    /// order.
    ///
    /// # Errors
    /// [`HostError::CanonicalEncoding`] if sealing the digest fails.
    pub(crate) fn seal(self) -> Result<HostCompositionManifest, HostError> {
        let schemas: Vec<CompositionSchema> = self.by_key.into_values().collect();
        let view = CompositionView {
            domain: COMPOSITION_DIGEST_DOMAIN,
            schemas: schemas
                .iter()
                .map(|entry| {
                    let (id, version, role) = entry.descriptor.identity_key();
                    CompositionSchemaView {
                        id,
                        version,
                        role: role.as_str(),
                        encoding: *entry.descriptor.encoding().bytes(),
                        owner_module: &entry.owner_module,
                    }
                })
                .collect(),
        };
        let digest = canonical_digest(&view).map(HostFingerprint)?;
        Ok(HostCompositionManifest { schemas, digest })
    }
}

impl HostCompositionManifest {
    /// The aggregated schemas in canonical `(id, version, role)` order.
    pub fn schemas(&self) -> impl Iterator<Item = &CompositionSchema> {
        self.schemas.iter()
    }

    /// The number of distinct schemas in the composition.
    #[must_use]
    pub fn len(&self) -> usize {
        self.schemas.len()
    }

    /// Whether the composition declares no schemas.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.schemas.is_empty()
    }

    /// The content-addressed digest over the whole schema set (Merkle root).
    #[must_use]
    pub fn digest(&self) -> HostFingerprint {
        self.digest
    }

    /// Look up a resolved schema by its `(id, version, role)` identity.
    #[must_use]
    pub fn find(&self, id: &str, version: u32, role: SchemaRole) -> Option<&CompositionSchema> {
        self.schemas.iter().find(|entry| {
            let (eid, eversion, erole) = entry.descriptor.identity_key();
            eid == id && eversion == version && erole == role
        })
    }

    /// Recompute and compare the sealed digest against the stored schema set.
    ///
    /// # Errors
    /// [`HostError::CanonicalEncoding`] if re-encoding fails.
    pub fn verify_digest(&self) -> Result<bool, HostError> {
        let view = CompositionView {
            domain: COMPOSITION_DIGEST_DOMAIN,
            schemas: self
                .schemas
                .iter()
                .map(|entry| {
                    let (id, version, role) = entry.descriptor.identity_key();
                    CompositionSchemaView {
                        id,
                        version,
                        role: role.as_str(),
                        encoding: *entry.descriptor.encoding().bytes(),
                        owner_module: &entry.owner_module,
                    }
                })
                .collect(),
        };
        let recomputed = canonical_digest(&view).map(HostFingerprint)?;
        Ok(recomputed == self.digest)
    }
}

#[cfg(test)]
#[path = "composition_schema_tests.rs"]
mod composition_schema_tests;
