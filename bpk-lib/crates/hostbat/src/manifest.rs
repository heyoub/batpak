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
}

fn compute_digest(
    id: &str,
    version: u32,
    operations: &[OperationDescriptor],
    receipt_namespaces: &[String],
    guard: Option<&GuardDescriptor>,
    hooks: &[HookDescriptor],
    jobs: &[JobDescriptor],
) -> Result<ModuleDigest, HostError> {
    let view = ManifestView {
        domain: MODULE_DIGEST_DOMAIN,
        id,
        version,
        operations: operations.iter().map(OperationView::from).collect(),
        receipt_namespaces,
        guard,
        hooks,
        jobs,
    };
    canonical_digest(&view).map(ModuleDigest)
}

impl HostModuleManifest {
    /// Seal a manifest from already-validated, canonically ordered parts. The
    /// digest is computed once and stored.
    ///
    /// # Errors
    /// [`HostError::CanonicalEncoding`] if the canonical encoder rejects the
    /// parts (unreachable for the frozen wire shapes).
    pub(crate) fn seal(
        id: String,
        version: u32,
        operations: Vec<OperationDescriptor>,
        receipt_namespaces: Vec<String>,
        guard: Option<GuardDescriptor>,
        hooks: Vec<HookDescriptor>,
        jobs: Vec<JobDescriptor>,
    ) -> Result<Self, HostError> {
        let digest = compute_digest(
            &id,
            version,
            &operations,
            &receipt_namespaces,
            guard.as_ref(),
            &hooks,
            &jobs,
        )?;
        Ok(Self {
            id,
            version,
            operations,
            receipt_namespaces,
            guard,
            hooks,
            jobs,
            digest,
        })
    }

    /// Recompute the digest from the stored parts and compare it to the sealed
    /// digest. `false` means the manifest does not match its declared parts.
    ///
    /// # Errors
    /// [`HostError::CanonicalEncoding`] if re-encoding fails.
    pub fn verify_hash(&self) -> Result<bool, HostError> {
        let recomputed = compute_digest(
            &self.id,
            self.version,
            &self.operations,
            &self.receipt_namespaces,
            self.guard.as_ref(),
            &self.hooks,
            &self.jobs,
        )?;
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
