//! [`SecretResolver`] — JIT secret-lease dereference (proof-spine §5 D2 + §8).
//!
//! A [`crate::EnvSource::SecretLease`] carries only an opaque [`crate::SecretRef`];
//! the durable plan + report NEVER carry the resolved value. The backend resolves
//! leases in the PARENT, immediately before launch, into the concrete envp it hands
//! the launcher (Vault-style dynamic secrets / AWS STS: resolve JIT, never persist).
//!
//! Production supplies a real resolver as a HOST HOOK (e.g. one that talks to a
//! secret store); this crate provides only the TRAIT + an explicit in-memory test
//! resolver. Resolution is fail-closed: an unknown/erroring lease yields
//! [`SecretResolveError`], and the backend then refuses the launch (the target never
//! runs) rather than substituting an empty or default value.

use crate::contract::capability::{EnvPolicy, EnvPolicyError, EnvSource, SecretRef};
use std::collections::BTreeMap;

/// Why a [`SecretRef`] could not be resolved to a value. Carried back so the backend
/// fails closed (refuses the launch) rather than running with a missing secret.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SecretResolveError {
    /// No value is bound to this lease (the ref is unknown / expired / revoked).
    Unknown {
        /// The opaque ref that could not be resolved.
        reference: SecretRef,
    },
    /// The backing store failed to produce the value (transient/IO/auth fault).
    Backend {
        /// The opaque ref whose resolution faulted.
        reference: SecretRef,
        /// Human-readable detail (NEVER the secret value).
        detail: String,
    },
}

impl std::fmt::Display for SecretResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown { reference } => {
                write!(f, "secret lease {:?} is unknown or expired", reference.id())
            }
            Self::Backend { reference, detail } => write!(
                f,
                "secret lease {:?} could not be resolved: {detail}",
                reference.id()
            ),
        }
    }
}

impl std::error::Error for SecretResolveError {}

/// Resolves an opaque [`SecretRef`] to its concrete value, JIT, in the parent. A
/// resolver is the host hook the backend calls immediately before launch; the
/// returned value is written ONLY into the child's envp and never persisted.
///
/// Implementors MUST NOT log/persist the returned value, and MUST fail closed
/// (return [`SecretResolveError`]) rather than yield a placeholder for a missing
/// lease — the backend refuses the launch on any error.
pub trait SecretResolver {
    /// Resolve `reference` to its secret value, or fail closed.
    ///
    /// # Errors
    /// [`SecretResolveError`] when the lease is unknown or the backing store faults.
    fn resolve(&self, reference: &SecretRef) -> Result<String, SecretResolveError>;
}

/// Why an [`EnvPolicy::Exact`] could not be LOWERED to a concrete envp: either the
/// table is contract-invalid ([`EnvPolicyError`]) or a lease failed to resolve
/// ([`SecretResolveError`]). Both fail the launch closed — the target never runs.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum EnvLowerError {
    /// The policy did not pass the contract gate ([`EnvPolicy::validate`]).
    Invalid(EnvPolicyError),
    /// A secret lease could not be resolved in the parent.
    Secret(SecretResolveError),
}

impl std::fmt::Display for EnvLowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(e) => write!(f, "environment policy is invalid: {e}"),
            Self::Secret(e) => write!(f, "environment secret lease unresolved: {e}"),
        }
    }
}

impl std::error::Error for EnvLowerError {}

/// LOWER an admitted [`EnvPolicy::Exact`] to the concrete `name → value` envp the
/// launcher serves to `fexecve` (proof-spine §5 D2 lowering). Re-validates the table
/// (defense in depth — admission already validated it, but lowering is the last gate
/// before the value materializes), then resolves every [`EnvSource::SecretLease`] in
/// the PARENT via `resolver`. Entry ORDER is preserved (the table is already a
/// validated function name → value); the result is exactly the child's environment,
/// nothing inherited.
///
/// The durable plan/report keep the POLICY (with lease REFS); only this returned
/// envp — used immediately, never persisted — carries resolved secret values.
///
/// # Errors
/// [`EnvLowerError::Invalid`] if the policy fails the contract gate, or
/// [`EnvLowerError::Secret`] if any lease fails to resolve (fail-closed: no partial
/// envp is returned, so the caller never launches with a missing secret).
pub fn lower_env(
    policy: &EnvPolicy,
    resolver: &dyn SecretResolver,
) -> Result<Vec<(String, String)>, EnvLowerError> {
    policy.validate().map_err(EnvLowerError::Invalid)?;
    let EnvPolicy::Exact(entries) = policy;
    let mut envp: Vec<(String, String)> = Vec::with_capacity(entries.len());
    for entry in entries {
        let value = match &entry.source {
            EnvSource::Literal(value) => value.clone(),
            EnvSource::SecretLease(reference) => {
                resolver.resolve(reference).map_err(EnvLowerError::Secret)?
            }
        };
        envp.push((entry.name.clone(), value));
    }
    Ok(envp)
}

/// An explicit in-memory [`SecretResolver`] for tests + simple host wiring: a fixed
/// `ref-id → value` table. An unknown ref fails closed with
/// [`SecretResolveError::Unknown`]. NOT a production secret store (it holds values in
/// process memory); it exercises the JIT-resolve + fail-closed contract.
#[derive(Clone, Debug, Default)]
pub struct MapSecretResolver {
    table: BTreeMap<String, String>,
}

impl MapSecretResolver {
    /// An empty resolver (every lease is unknown ⇒ fail-closed).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind `value` to the lease id `reference`, returning `self` for chaining.
    #[must_use]
    pub fn with(mut self, reference: impl Into<String>, value: impl Into<String>) -> Self {
        self.table.insert(reference.into(), value.into());
        self
    }
}

impl SecretResolver for MapSecretResolver {
    fn resolve(&self, reference: &SecretRef) -> Result<String, SecretResolveError> {
        self.table
            .get(reference.id())
            .cloned()
            .ok_or_else(|| SecretResolveError::Unknown {
                reference: reference.clone(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{lower_env, EnvLowerError, MapSecretResolver, SecretResolveError, SecretResolver};
    use crate::contract::capability::{EnvEntry, EnvPolicy, EnvPolicyError, SecretRef};

    #[test]
    fn a_bound_lease_resolves_to_its_value() {
        let resolver = MapSecretResolver::new().with("lease://a", "s3cr3t");
        assert_eq!(
            resolver.resolve(&SecretRef::new("lease://a")),
            Ok("s3cr3t".to_string())
        );
    }

    #[test]
    fn an_unknown_lease_fails_closed() {
        let resolver = MapSecretResolver::new();
        let reference = SecretRef::new("lease://missing");
        assert_eq!(
            resolver.resolve(&reference),
            Err(SecretResolveError::Unknown { reference })
        );
    }

    #[test]
    fn lowering_resolves_literals_and_leases_in_order() {
        let resolver = MapSecretResolver::new().with("lease://tok", "RESOLVED-VALUE");
        let policy = EnvPolicy::Exact(vec![
            EnvEntry::literal("PATH", "/usr/bin:/bin"),
            EnvEntry::lease("TOKEN", SecretRef::new("lease://tok")),
        ]);
        let envp = lower_env(&policy, &resolver).expect("lowers cleanly");
        assert_eq!(
            envp,
            vec![
                ("PATH".to_string(), "/usr/bin:/bin".to_string()),
                ("TOKEN".to_string(), "RESOLVED-VALUE".to_string()),
            ]
        );
    }

    #[test]
    fn lowering_fails_closed_on_an_unresolvable_lease() {
        let resolver = MapSecretResolver::new(); // empty ⇒ every lease unknown
        let policy = EnvPolicy::Exact(vec![EnvEntry::lease(
            "TOKEN",
            SecretRef::new("lease://missing"),
        )]);
        let result = lower_env(&policy, &resolver);
        assert!(
            matches!(
                result,
                Err(EnvLowerError::Secret(SecretResolveError::Unknown { .. }))
            ),
            "an unresolvable lease must fail lowering closed, got {result:?}"
        );
    }

    #[test]
    fn lowering_fails_closed_on_an_invalid_policy() {
        let resolver = MapSecretResolver::new();
        let policy = EnvPolicy::Exact(vec![
            EnvEntry::literal("DUP", "a"),
            EnvEntry::literal("DUP", "b"),
        ]);
        assert_eq!(
            lower_env(&policy, &resolver),
            Err(EnvLowerError::Invalid(EnvPolicyError::DuplicateName {
                name: "DUP".to_string()
            }))
        );
    }
}
