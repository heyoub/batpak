//! The content-identified module manifest.
//!
//! A [`HostModuleManifest`] is the **declarative attestation** of a module: its
//! id, version, operation descriptors, receipt-extension namespaces, guard
//! descriptor, lifecycle hooks, and supervised-job kinds. Its identity is
//! `H_module = H("hostbat.module.v1" ‖ canonical(parts))`.
//!
//! A manifest is never hand-authored beside the impl — it is *sealed* from the
//! exact parts registered on a [`crate::module::HostModuleBuilder`], so the
//! declaration and the implementation cannot drift. [`verify_hash`] recomputes
//! the digest from the stored parts; a mismatch means the manifest was tampered
//! with or constructed out of band, and the host refuses to mount it.
//!
//! [`verify_hash`]: HostModuleManifest::verify_hash

use serde::Serialize;
use syncbat::OperationDescriptor;

use crate::descriptor::{GuardDescriptor, HookDescriptor, JobDescriptor};
use crate::error::HostError;
use crate::identity::{canonical_digest, ModuleDigest};
use crate::schema::{SchemaDescriptor, SchemaManifestView};
use crate::subscription::SubscriptionDescriptor;

/// Domain separator for the module-manifest digest.
const MODULE_DIGEST_DOMAIN: &str = "hostbat.module.v1";

/// Content-identified declaration of one host module.
///
/// All list fields are held in canonical order (operations by name, namespaces
/// lexically, hooks by `(phase, order, name)`, jobs by kind), so the digest
/// depends on the declared *set*, never on registration order.
#[derive(Clone, Debug)]
pub struct HostModuleManifest {
    id: String,
    version: u32,
    operations: Vec<OperationDescriptor>,
    receipt_namespaces: Vec<String>,
    guard: Option<GuardDescriptor>,
    hooks: Vec<HookDescriptor>,
    jobs: Vec<JobDescriptor>,
    schemas: Vec<SchemaDescriptor>,
    subscriptions: Vec<SubscriptionDescriptor>,
    digest: ModuleDigest,
}

/// Serializable view of one operation descriptor for canonical hashing — captures
/// exactly the descriptor's stable declarative fields.
#[derive(Serialize)]
struct OperationView<'a> {
    name: &'a str,
    effect: &'a str,
    input_schema_ref: &'a str,
    output_schema_ref: &'a str,
    receipt_kind: &'a str,
    title: Option<&'a str>,
}

impl<'a> From<&'a OperationDescriptor> for OperationView<'a> {
    fn from(descriptor: &'a OperationDescriptor) -> Self {
        Self {
            name: descriptor.name(),
            effect: descriptor.effect.as_str(),
            input_schema_ref: descriptor.input_schema_ref(),
            output_schema_ref: descriptor.output_schema_ref(),
            receipt_kind: descriptor.receipt_kind(),
            title: descriptor.title(),
        }
    }
}

/// Domain-separated canonical view of a whole manifest.
#[derive(Serialize)]
struct ManifestView<'a> {
    domain: &'a str,
    id: &'a str,
    version: u32,
    operations: Vec<OperationView<'a>>,
    receipt_namespaces: &'a [String],
    guard: Option<&'a GuardDescriptor>,
    hooks: &'a [HookDescriptor],
    jobs: &'a [JobDescriptor],
    // Identity-bearing schema views only (id/version/role/encoding); the
    // diagnostic Rust type is excluded so renaming/dropping it changes no digest.
    schemas: Vec<SchemaManifestView<'a>>,
    subscriptions: &'a [SubscriptionDescriptor],
}

/// Borrowed view of the canonically ordered parts a manifest digest is sealed
/// over. Bundling them keeps `compute_digest` single-argument and the seal /
/// verify call sites symmetric.
struct ManifestParts<'a> {
    id: &'a str,
    version: u32,
    operations: &'a [OperationDescriptor],
    receipt_namespaces: &'a [String],
    guard: Option<&'a GuardDescriptor>,
    hooks: &'a [HookDescriptor],
    jobs: &'a [JobDescriptor],
    schemas: &'a [SchemaDescriptor],
    subscriptions: &'a [SubscriptionDescriptor],
}

fn compute_digest(parts: &ManifestParts<'_>) -> Result<ModuleDigest, HostError> {
    let view = ManifestView {
        domain: MODULE_DIGEST_DOMAIN,
        id: parts.id,
        version: parts.version,
        operations: parts.operations.iter().map(OperationView::from).collect(),
        receipt_namespaces: parts.receipt_namespaces,
        guard: parts.guard,
        hooks: parts.hooks,
        jobs: parts.jobs,
        schemas: parts
            .schemas
            .iter()
            .map(SchemaDescriptor::manifest_view)
            .collect(),
        subscriptions: parts.subscriptions,
    };
    canonical_digest(&view).map(ModuleDigest)
}

/// Owned, canonically ordered parts handed to [`HostModuleManifest::seal`].
pub(crate) struct SealedParts {
    pub(crate) id: String,
    pub(crate) version: u32,
    pub(crate) operations: Vec<OperationDescriptor>,
    pub(crate) receipt_namespaces: Vec<String>,
    pub(crate) guard: Option<GuardDescriptor>,
    pub(crate) hooks: Vec<HookDescriptor>,
    pub(crate) jobs: Vec<JobDescriptor>,
    pub(crate) schemas: Vec<SchemaDescriptor>,
    pub(crate) subscriptions: Vec<SubscriptionDescriptor>,
}

impl HostModuleManifest {
    /// Seal a manifest from already-validated, canonically ordered parts. The
    /// digest is computed once and stored.
    ///
    /// # Errors
    /// [`HostError::CanonicalEncoding`] if the canonical encoder rejects the
    /// parts (unreachable for the frozen wire shapes).
    pub(crate) fn seal(parts: SealedParts) -> Result<Self, HostError> {
        let SealedParts {
            id,
            version,
            operations,
            receipt_namespaces,
            guard,
            hooks,
            jobs,
            schemas,
            subscriptions,
        } = parts;
        let digest = compute_digest(&ManifestParts {
            id: &id,
            version,
            operations: &operations,
            receipt_namespaces: &receipt_namespaces,
            guard: guard.as_ref(),
            hooks: &hooks,
            jobs: &jobs,
            schemas: &schemas,
            subscriptions: &subscriptions,
        })?;
        Ok(Self {
            id,
            version,
            operations,
            receipt_namespaces,
            guard,
            hooks,
            jobs,
            schemas,
            subscriptions,
            digest,
        })
    }

    /// Recompute the digest from the stored parts and compare it to the sealed
    /// digest. `false` means the manifest does not match its declared parts.
    ///
    /// # Errors
    /// [`HostError::CanonicalEncoding`] if re-encoding fails.
    pub fn verify_hash(&self) -> Result<bool, HostError> {
        let recomputed = compute_digest(&ManifestParts {
            id: &self.id,
            version: self.version,
            operations: &self.operations,
            receipt_namespaces: &self.receipt_namespaces,
            guard: self.guard.as_ref(),
            hooks: &self.hooks,
            jobs: &self.jobs,
            schemas: &self.schemas,
            subscriptions: &self.subscriptions,
        })?;
        Ok(recomputed == self.digest)
    }

    /// Stable module id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Module version.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Content digest `H_module`.
    #[must_use]
    pub fn digest(&self) -> ModuleDigest {
        self.digest
    }

    /// Operation descriptors in canonical (name) order.
    pub fn operations(&self) -> impl Iterator<Item = &OperationDescriptor> {
        self.operations.iter()
    }

    /// Receipt-extension namespaces in lexical order.
    pub fn receipt_namespaces(&self) -> impl Iterator<Item = &str> {
        self.receipt_namespaces.iter().map(String::as_str)
    }

    /// The guard descriptor, if this module guards its operations.
    #[must_use]
    pub fn guard(&self) -> Option<&GuardDescriptor> {
        self.guard.as_ref()
    }

    /// Lifecycle hooks in canonical `(phase, order, name)` order.
    pub fn hooks(&self) -> impl Iterator<Item = &HookDescriptor> {
        self.hooks.iter()
    }

    /// Supervised-job kinds in canonical (kind) order.
    pub fn jobs(&self) -> impl Iterator<Item = &JobDescriptor> {
        self.jobs.iter()
    }

    /// Schema descriptors this module owns, in canonical `(id, version, role)`
    /// order. These are the language-neutral wire-shape declarations the
    /// composition aggregates and S12's TypeScript codegen consumes.
    pub fn schemas(&self) -> impl Iterator<Item = &SchemaDescriptor> {
        self.schemas.iter()
    }

    /// Subscription descriptors this module exports, in canonical (id) order.
    pub fn subscriptions(&self) -> impl Iterator<Item = &SubscriptionDescriptor> {
        self.subscriptions.iter()
    }

    /// Replace the sealed digest with a corrupt value. **Test-only**, behind the
    /// gauntlet red-fixture cfg: it manufactures the tampered manifest the
    /// `ModuleHashMismatch` fixture proves the host rejects.
    #[cfg(any(test, gauntlet_red_fixture))]
    pub(crate) fn corrupt_digest_for_fixture(&mut self) {
        let mut bytes = self.digest.0;
        bytes[0] ^= 0xff;
        self.digest = ModuleDigest(bytes);
    }
}
